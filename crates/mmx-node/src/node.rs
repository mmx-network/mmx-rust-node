use crate::peer_state::PeerState;
use crate::{error, inbox::Inbox, Result};
use k256::ecdsa::SigningKey;
use mmx_consensus::{Hash, Record, Value};
use mmx_execution::chain::{
    forks::{ForkChain, Update},
    network::Network,
    sync::{Block, Policy},
};
use mmx_types::mmx;
use mmx_vm::Storage;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::{Read, Write},
    path::Path,
    time::Instant,
};

pub struct Node {
    pub(crate) chain: ForkChain,
    pub(crate) root: u32,
    pub(crate) synced: bool,
    pub(crate) sync_pos: u32,
    pub(crate) sync_peers: usize,
    pub(crate) sync_pending: BTreeSet<u32>,
    pub(crate) sync_body_requests: u64,
    pub(crate) sync_body_retries: u64,
    pub(crate) sync_backpressure_retries: u64,
    work: VecDeque<Record>,
    pending: BTreeMap<Hash, Record>,
    verified_work: BTreeMap<Hash, mmx_consensus::vdf::VerifiedPoint>,
    vdf_worker: Option<crate::vdf_worker::Worker>,
    responses: Vec<(Record, i64)>,
    inbox: Inbox,
    peer_state: PeerState,
}
pub fn record(name: &str, fields: impl IntoIterator<Item = (String, Value)>) -> Record {
    Record {
        name: name.into(),
        fields: fields.into_iter().collect(),
    }
}
pub fn hash(record: &Record, field: &str) -> Result<Hash> {
    record
        .field(field)?
        .bytes()?
        .try_into()
        .map_err(|_| error("hash length"))
}
pub fn number(record: &Record, field: &str) -> Result<u32> {
    Ok(u32::try_from(record.field(field)?.uint()?)?)
}
fn bytes(v: impl Into<Vec<u8>>) -> Value {
    Value::Bytes(v.into())
}

impl Node {
    pub fn open(path: impl AsRef<Path>, network: &Network) -> Result<Self> {
        let chain_path = path.as_ref().join("chain");
        let legacy = ["selected", "archive"].iter().any(|database| {
            let directory = chain_path.join(database);
            directory.join("state.sqlite").exists() || directory.join("journal.mmx").exists()
        });
        let mut chain = if legacy {
            ForkChain::open_indexed(&chain_path, network.params.clone())?
        } else {
            ForkChain::open_native(&chain_path, network.params.clone())?
        };
        if chain.selected().tip()?.is_none() {
            chain.initialize(&network.assets)?;
        }
        let expected = mmx_execution::chain::genesis::build(&network.params, &network.assets)?;
        if chain.selected().hash_at(0)? != Some(hash(&expected, "hash")?) {
            return Err(error("network genesis does not match node database"));
        }
        let key_path = path.as_ref().join("identity.key");
        let key = match std::fs::read(&key_path) {
            Ok(raw) => SigningKey::from_slice(&raw)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let key = loop {
                    let mut raw = [0; 32];
                    std::fs::File::open("/dev/urandom")?.read_exact(&mut raw)?;
                    if let Ok(key) = SigningKey::from_slice(&raw) {
                        break key;
                    }
                };
                let mut temporary = tempfile::NamedTempFile::new_in(path.as_ref())?;
                temporary.write_all(&key.to_bytes())?;
                temporary.as_file().sync_all()?;
                temporary.persist_noclobber(key_path).map_err(|e| e.error)?;
                std::fs::File::open(path.as_ref())?.sync_all()?;
                key
            }
            Err(e) => return Err(e.into()),
        };
        Self::from_chain(chain, key)
    }
    /// For embedded nodes and tests; the caller supplies an initialized chain.
    pub fn from_chain(chain: ForkChain, key: SigningKey) -> Result<Self> {
        let tip = chain
            .selected()
            .tip()?
            .ok_or_else(|| error("uninitialized node"))?;
        let root = tip
            .height
            .saturating_sub(chain.selected().params().commit_delay);
        let peer_state = PeerState::new(
            &key,
            chain
                .selected()
                .hash_at(0)?
                .ok_or_else(|| error("missing genesis"))?,
            tip.height,
        )?;
        Ok(Self {
            chain,
            root,
            synced: false,
            sync_pos: tip.height.saturating_add(1),
            sync_peers: 0,
            sync_pending: BTreeSet::new(),
            sync_body_requests: 0,
            sync_body_retries: 0,
            sync_backpressure_retries: 0,
            work: VecDeque::new(),
            pending: BTreeMap::new(),
            verified_work: BTreeMap::new(),
            vdf_worker: None,
            responses: vec![],
            inbox: Inbox::default(),
            peer_state,
        })
    }
    pub fn chain(&self) -> &ForkChain {
        &self.chain
    }
    pub(crate) fn inbox(&self) -> Inbox {
        self.inbox.clone()
    }
    pub(crate) fn peer_state(&self) -> PeerState {
        self.peer_state.clone()
    }
    pub fn is_synced(&self) -> bool {
        self.synced
    }
    /// Runtime CPU tasks must never hold the ledger mutex while hashing a VDF.
    /// Embedded callers retain synchronous admission unless they opt in.
    pub fn enable_background_vdf(&mut self) -> Result<()> {
        if self.vdf_worker.is_none() {
            self.vdf_worker = Some(crate::vdf_worker::Worker::new()?);
        }
        Ok(())
    }
    pub fn pending_proofs(&self) -> usize {
        self.responses.len()
    }
    pub fn begin_resync(&mut self) {
        self.synced = false;
        self.peer_state.begin_resync();
    }
    /// Drain peer broadcasts and coalesce work in the native periodic update.
    /// Embedded callers must drive this tick even while a peer is polling.
    pub fn tick(&mut self) -> Result<()> {
        self.update().map(|_| ())
    }
    pub fn root_height(&self) -> u32 {
        self.root
    }
    pub fn rollback_to(&mut self, height: u32) -> Result<()> {
        self.chain.rollback_to(height)?;
        self.root = height.saturating_sub(self.chain.selected().params().commit_delay);
        self.synced = false;
        self.peer_state.publish(height, false);
        self.sync_pos = height.saturating_add(1);
        self.sync_peers = 0;
        self.sync_pending.clear();
        self.work.clear();
        self.pending.clear();
        self.verified_work.clear();
        self.responses.clear();
        self.inbox.clear()?;
        Ok(())
    }
    pub(crate) fn update(&mut self) -> Result<Update> {
        let started = Instant::now();
        for (message, _) in self.inbox.take()? {
            if let Err(e) = self.admit_message(&message) {
                eprintln!("Rejected {}: {e}", message.name);
            }
        }
        let selected = self.chain.selected();
        let tip = selected
            .tip()?
            .ok_or_else(|| error("missing selected tip"))?;
        self.chain.set_tip_limit(Some(if self.synced {
            tip.height.saturating_add(1000)
        } else {
            self.sync_pos.saturating_sub(1).max(tip.height)
        }));
        let selected = self.chain.selected();
        let root = selected
            .header(
                &selected
                    .hash_at(self.root)?
                    .ok_or_else(|| error("missing selected root"))?,
            )?
            .ok_or_else(|| error("missing root header"))?;
        let mut inputs: BTreeSet<Hash> = self
            .chain
            .verified_vdf_inputs(self.root)?
            .into_iter()
            .collect();
        if let Some(worker) = &mut self.vdf_worker {
            let retained = self
                .work
                .iter()
                .map(|work| hash(work, "content_hash"))
                .collect::<Result<_>>()?;
            worker.poll(&retained);
        }
        for height in self.root..=tip.height {
            let header = selected
                .header(
                    &selected
                        .hash_at(height)?
                        .ok_or_else(|| error("missing selected height"))?,
                )?
                .ok_or_else(|| error("missing selected header"))?;
            inputs.insert(header.vdf_output.0);
        }
        for work in &self.work {
            let id = hash(work, "content_hash")?;
            if work.field("start")?.uint()? < root.vdf_iters
                || !inputs.contains(&hash(work, "input")?)
            {
                continue;
            }
            if !self.verified_work.contains_key(&id) {
                if let Some(worker) = &mut self.vdf_worker {
                    if mmx_consensus::vdf::validate(
                        work,
                        self.chain.selected().params(),
                        &self.chain,
                    )
                    .is_ok()
                    {
                        if let Some(cpu) = worker.proof(id, work)? {
                            // The fork can move while CPU work runs. Admission
                            // always rechecks authenticated current history.
                            if let Ok(point) =
                                cpu.admit(self.chain.selected().params(), &self.chain)
                            {
                                self.verified_work.insert(id, point);
                            }
                        }
                    }
                } else if let Ok(point) =
                    mmx_consensus::vdf::verify(work, self.chain.selected().params(), &self.chain)
                {
                    self.verified_work.insert(id, point);
                }
            }
            if let Some(point) = self.verified_work.get(&id) {
                inputs.insert(point.point().output.0);
            }
        }
        self.verified_work
            .retain(|_, p| p.point().start >= root.vdf_iters);
        let work = self
            .verified_work
            .values()
            .map(|p| Record::from_generated(p.proof()))
            .collect::<mmx_consensus::Result<Vec<_>>>()?;
        for block in self.pending.values() {
            self.chain.admit(block, &work)?;
        }
        self.chain
            .cache_verified_work(&self.verified_work.values().cloned().collect::<Vec<_>>())?;
        self.process_responses()?;
        let root = self
            .chain
            .selected()
            .hash_at(self.root)?
            .ok_or_else(|| error("missing node root"))?;
        let now = vnx::util::get_wall_time_millis();
        let prepared = Instant::now();
        let out = if self.synced {
            self.chain
                .update_at(root, self.sync_pending.first().copied(), now)?
        } else {
            self.chain
                .update_sync_at(root, self.sync_pending.first().copied(), now)?
        };
        let forked = Instant::now();
        // A deep branch switch may replace the root's block. Heights remain soft.
        let selected = self.chain.selected();
        let header = selected
            .header(
                &selected
                    .hash_at(self.root)?
                    .ok_or_else(|| error("missing selected root"))?,
            )?
            .ok_or_else(|| error("missing root header"))?;
        let tip = selected
            .tip()?
            .ok_or_else(|| error("missing selected tip"))?;
        let mut line = Vec::new();
        for height in self.root + 1..=tip.height {
            let h = selected
                .header(
                    &selected
                        .hash_at(height)?
                        .ok_or_else(|| error("missing selected height"))?,
                )?
                .ok_or_else(|| error("missing selected header"))?;
            line.push(Block {
                height,
                vdf_height: h.vdf_height,
                proofs: h.proof.len() as u64,
            });
        }
        let p = selected.params();
        let advance = Policy {
            commit_threshold: 80,
            commit_delay: p.commit_delay,
            max_future_sync: 100,
            avg_proof_count: p.avg_proof_count,
        }
        .advance(
            Block {
                height: header.height,
                vdf_height: header.vdf_height,
                proofs: header.proof.len() as u64,
            },
            &line,
            self.synced,
            self.sync_pos,
            &self.sync_pending,
        )?;
        if let Some(height) = advance.heights.last() {
            self.root = *height;
        }
        self.pending
            .retain(|_, b| number(b, "height").is_ok_and(|h| h > self.root));
        self.peer_state.publish(tip.height, self.synced);
        if out.applied.len() >= 64 && std::env::var_os("MMX_PROFILE_REPLAY").is_some() {
            eprintln!("Node update: blocks={} prepare_ms={:.3} fork_ms={:.3} load_ms={:.3} proofs_ms={:.3} choice_ms={:.3} lineage_ms={:.3} replay_ms={:.3} policy_ms={:.3}",
                out.applied.len(),
                prepared.duration_since(started).as_secs_f64() * 1e3,
                forked.duration_since(prepared).as_secs_f64() * 1e3,
                out.load_time.as_secs_f64() * 1e3,
                out.verification_time.as_secs_f64() * 1e3,
                out.choice_time.as_secs_f64() * 1e3,
                out.lineage_time.as_secs_f64() * 1e3,
                out.replay_time.as_secs_f64() * 1e3,
                forked.elapsed().as_secs_f64() * 1e3);
        }
        Ok(out)
    }
    fn vdf_peak(&self) -> Result<(u32, Hash)> {
        let tip = self
            .chain
            .selected()
            .tip()?
            .ok_or_else(|| error("missing tip"))?;
        let points: Vec<_> = self
            .work
            .iter()
            .filter_map(|w| {
                hash(w, "content_hash")
                    .ok()
                    .and_then(|h| self.verified_work.get(&h).cloned())
            })
            .collect();
        let next = mmx_consensus::vdf::next_points(
            &self.chain,
            self.chain.selected().params(),
            &tip,
            &points,
        )?;
        Ok((
            tip.vdf_height.wrapping_add(next.len() as u32),
            next.last().map_or(tip.vdf_output.0, |p| p.point().output.0),
        ))
    }
    fn process_responses(&mut self) -> Result<()> {
        let tip = self
            .chain
            .selected()
            .tip()?
            .ok_or_else(|| error("missing tip"))?;
        let root = self
            .chain
            .selected()
            .header(
                &self
                    .chain
                    .selected()
                    .hash_at(self.root)?
                    .ok_or_else(|| error("missing root"))?,
            )?
            .ok_or_else(|| error("missing root header"))?;
        let params = self.chain.selected().params().clone();
        let now = vnx::util::get_wall_time_millis();
        let height = self.vdf_peak()?.0;
        let mut pending = Vec::new();
        for (record, received) in std::mem::take(&mut self.responses) {
            if now.wrapping_sub(received) > params.block_interval_ms.wrapping_mul(10) {
                continue;
            }
            if number(&record, "vdf_height")? >= height.wrapping_add(params.challenge_delay) {
                pending.push((record, received));
                continue;
            }
            match mmx_consensus::response::verify(
                &record,
                &params,
                &self.chain,
                &tip,
                root.vdf_height,
                1000,
            ) {
                Ok(proof) => self.chain.observe_proof(&proof),
                Err(e) => eprintln!("Rejected farmer proof: {e}"),
            }
        }
        self.responses = pending;
        Ok(())
    }
    pub fn receive(&mut self, message: &Record) -> Result<()> {
        if self.admit_message(message)? {
            self.update()?;
        }
        Ok(())
    }
    fn admit_message(&mut self, message: &Record) -> Result<bool> {
        let mut update = false;
        match message.name.as_str() {
            "mmx.Block" if self.synced => {
                self.chain.admit(message, &[])?;
                self.pending.insert(hash(message, "hash")?, message.clone());
                update = true;
            }
            "mmx.ProofOfTime" => {
                if !mmx_consensus::vdf::is_valid(message)? {
                    return Err(error("invalid VDF content"));
                }
                mmx_consensus::vdf::authenticate(message)?;
                self.work.push_back(message.clone());
                while self.work.len() > 32 {
                    if let Some(old) = self.work.pop_front() {
                        self.verified_work.remove(&hash(&old, "content_hash")?);
                    }
                }
                update = true;
            }
            "mmx.ValidatorVote" => {
                self.chain
                    .admit_vote(message, vnx::util::get_wall_time_millis())?;
            }
            "mmx.ProofResponse" => {
                if !mmx_consensus::response::is_valid(message)? {
                    return Err(error("invalid proof response"));
                }
                self.responses
                    .push((message.clone(), vnx::util::get_wall_time_millis()));
            }
            // Other broadcasts are not ledger admissions. Transaction submission
            // and farmer response relay are separate from the read/sync service.
            _ => (),
        }
        Ok(update)
    }
    /// Native method/result records, shared by peer requests and VNX/HTTP reads.
    /// Unsupported methods return an explicit error instead of placeholder data.
    pub fn call(&self, request: &Record) -> Result<Record> {
        if let Some(reply) = self.peer_state.call(request)? {
            return Ok(reply);
        }
        let selected = self.chain.selected();
        let optional = |v: Option<Value>| Value::Optional(v.map(Box::new));
        let block_value = |v: Option<Record>| v.map(Value::Record).unwrap_or(Value::Null);
        let block_at = |height| -> Result<Option<Record>> {
            match selected.hash_at(height)? {
                Some(h) => Ok(self.chain.block(&h)?),
                None => Ok(None),
            }
        };
        let header_value = |b: Option<Record>| {
            block_value(b.map(|mut b| {
                b.name = "mmx.BlockHeader".into();
                b.fields.remove("tx_list");
                b
            }))
        };
        let value = match request.name.as_str() {
            "mmx.Node.get_params" => Value::Record(Record::from_generated(selected.params())?),
            "mmx.Node.get_vdf_height" => Value::Uint(self.vdf_peak()?.0.into()),
            "mmx.Node.get_synced_vdf_height" => optional(
                self.synced
                    .then_some(Value::Uint(self.vdf_peak()?.0.into())),
            ),
            "mmx.Node.get_vdf_peak" => bytes(self.vdf_peak()?.1),
            "mmx.Node.get_block" => block_value(self.chain.block(&hash(request, "hash")?)?),
            "mmx.Node.get_block_at" => block_value(block_at(number(request, "height")?)?),
            "mmx.Node.get_header" => header_value(self.chain.block(&hash(request, "hash")?)?),
            "mmx.Node.get_header_at" => header_value(block_at(number(request, "height")?)?),
            "mmx.Node.get_block_hash" => {
                optional(selected.hash_at(number(request, "height")?)?.map(bytes))
            }
            "mmx.Node.get_block_hash_ex" => optional(
                block_at(number(request, "height")?)?
                    .map(|b| {
                        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(Value::Sequence(vec![
                            bytes(hash(&b, "hash")?),
                            bytes(hash(&b, "content_hash")?),
                        ]))
                    })
                    .transpose()?,
            ),
            "mmx.Node.get_transaction" => block_value(selected.transaction(&hash(request, "id")?)?),
            "mmx.Node.get_transactions" => Value::Sequence(
                request
                    .field("ids")?
                    .seq()?
                    .iter()
                    .map(|v| {
                        let id: Hash = v.bytes()?.try_into()?;
                        Ok(block_value(selected.transaction(&id)?))
                    })
                    .collect::<Result<_>>()?,
            ),
            "mmx.Node.get_tx_height" => optional(
                selected
                    .storage()
                    .get_transaction_index(&hash(request, "id")?)?
                    .map(|i| Value::Uint(i.height.into())),
            ),
            "mmx.Node.get_tx_ids_at" => Value::Sequence(
                block_at(number(request, "height")?)?
                    .map(|b| {
                        b.field("tx_list")?
                            .seq()?
                            .iter()
                            .map(|t| Ok(bytes(hash(t.record()?, "id")?)))
                            .collect::<Result<Vec<_>>>()
                    })
                    .transpose()?
                    .unwrap_or_default(),
            ),
            "mmx.Node.get_contract" | "mmx.Node.get_contract_for" => {
                let address = hash(request, "address")?;
                let mut contract = selected
                    .storage()
                    .get_contract(&address)?
                    .map(|c| Record::decode(&c.bytes))
                    .transpose()?;
                if contract.is_none() && request.name == "mmx.Node.get_contract_for" {
                    contract = Some(Record::from_generated(&mmx::contract::PubKey {
                        address: address.into(),
                        ..Default::default()
                    })?);
                }
                block_value(contract)
            }
            "mmx.Node.get_balance" => bytes(
                selected
                    .storage()
                    .get_balance(&hash(request, "address")?, &hash(request, "currency")?)?
                    .unwrap_or(0)
                    .to_le_bytes(),
            ),
            _ => return Err(error(format!("unsupported read method: {}", request.name))),
        };
        Ok(record(
            &format!("{}.return", request.name),
            [("_ret_0".into(), value)],
        ))
    }
}
