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
    pub(crate) production: crate::production::State,
    pub(crate) chain: ForkChain,
    pub(crate) root: u32,
    pub(crate) synced: bool,
    pub(crate) sync_pos: u32,
    pub(crate) sync_peers: usize,
    pub(crate) sync_pending: BTreeSet<u32>,
    pub(crate) sync_body_requests: u64,
    pub(crate) sync_body_retries: u64,
    pub(crate) sync_backpressure_retries: u64,
    work: VecDeque<(Record, i64, usize)>,
    work_bytes: usize,
    pending: BTreeMap<Hash, (Record, i64, usize)>,
    pending_bytes: usize,
    pub(crate) verified_work: BTreeMap<Hash, mmx_consensus::vdf::VerifiedPoint>,
    vdf_worker: Option<crate::vdf_worker::Worker>,
    responses: BTreeMap<Hash, (Record, i64, usize)>,
    response_bytes: usize,
    pub(crate) tx_pool: crate::transactions::Pool,
    inbox: Inbox,
    peer_state: PeerState,
    vote_ids: BTreeSet<Hash>,
    vote_order: VecDeque<Hash>,
    pub(crate) rpc_workers: std::sync::Arc<std::sync::atomic::AtomicUsize>,
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
pub(crate) fn bytes(v: impl Into<Vec<u8>>) -> Value {
    Value::Bytes(v.into())
}

impl Node {
    pub fn open(path: impl AsRef<Path>, network: &Network) -> Result<Self> {
        let chain_path = path.as_ref().join("chain");
        let mut chain = ForkChain::open_native(&chain_path, network.params.clone())?;
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
            .saturating_sub(chain.selected().params().commit_delay)
            .max(chain.earliest_height());
        let peer_state = PeerState::new(
            &key,
            chain
                .selected()
                .hash_at(0)?
                .ok_or_else(|| error("missing genesis"))?,
            tip.height,
        )?;
        peer_state.set_earliest_height(chain.earliest_height());
        Ok(Self {
            chain,
            production: Default::default(),
            root,
            synced: false,
            sync_pos: tip.height.saturating_add(1),
            sync_peers: 0,
            sync_pending: BTreeSet::new(),
            sync_body_requests: 0,
            sync_body_retries: 0,
            sync_backpressure_retries: 0,
            work: VecDeque::new(),
            work_bytes: 0,
            pending: BTreeMap::new(),
            pending_bytes: 0,
            verified_work: BTreeMap::new(),
            vdf_worker: None,
            responses: BTreeMap::new(),
            response_bytes: 0,
            tx_pool: Default::default(),
            inbox: Inbox::default(),
            peer_state,
            vote_ids: BTreeSet::new(),
            vote_order: VecDeque::new(),
            rpc_workers: Default::default(),
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
    /// Runtime work tasks must never hold the ledger mutex while hashing a VDF.
    /// Embedded callers retain synchronous admission unless they opt in.
    pub fn enable_background_vdf(&mut self) -> Result<()> {
        self.enable_background_vdf_with(None)
    }
    /// Keep device work outside the ledger mutex and recheck returned evidence
    /// against current authenticated history before it can extend the VDF peak.
    pub fn enable_background_vdf_with(
        &mut self,
        accelerator: Option<std::sync::Arc<mmx_consensus::accelerator::Accelerator>>,
    ) -> Result<()> {
        if self.vdf_worker.is_none() {
            self.vdf_worker = Some(crate::vdf_worker::Worker::new(accelerator)?);
        }
        Ok(())
    }
    /// Native Node::update challenge window; ancestry is resolved by the same
    /// consensus helper used to verify incoming farmer responses.
    pub fn farming_challenges(&self) -> Result<Vec<mmx::Challenge>> {
        if !self.synced {
            return Ok(Vec::new());
        }
        let tip = self
            .chain
            .selected()
            .tip()?
            .ok_or_else(|| error("missing tip"))?;
        let params = self.chain.selected().params();
        let advance = self
            .vdf_peak()?
            .0
            .saturating_sub(tip.vdf_height)
            .saturating_add(params.challenge_delay.saturating_sub(1))
            .min(params.max_vdf_count);
        (1..=advance)
            .map(|offset| {
                let (challenge, difficulty) = mmx_consensus::transitions::history::challenge(
                    &self.chain,
                    params,
                    &tip,
                    offset,
                )?;
                Ok(mmx::Challenge {
                    vdf_height: tip
                        .vdf_height
                        .checked_add(offset)
                        .ok_or_else(|| error("challenge height overflow"))?,
                    challenge: challenge.into(),
                    difficulty,
                })
            })
            .collect()
    }
    pub fn pending_proofs(&self) -> usize {
        self.responses.len()
    }
    pub fn pending_blocks(&self) -> usize {
        self.pending.len()
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
    pub fn earliest_height(&self) -> u32 {
        self.chain.earliest_height()
    }
    /// Optional startup maintenance. Retain `height` and every later block;
    /// the resulting rollback/RPC boundary survives restart.
    pub fn purge_before(&mut self, height: u32) -> Result<()> {
        let result = self.chain.purge_before_height(height);
        let earliest = self.chain.earliest_height();
        self.peer_state.set_earliest_height(earliest);
        self.root = self.root.max(earliest);
        result?;
        Ok(())
    }
    pub fn rollback_to(&mut self, height: u32) -> Result<()> {
        self.chain.rollback_to(height)?;
        self.root = height
            .saturating_sub(self.chain.selected().params().commit_delay)
            .max(self.earliest_height());
        self.synced = false;
        self.peer_state.publish(height, false);
        self.sync_pos = height.saturating_add(1);
        self.sync_peers = 0;
        self.sync_pending.clear();
        self.work.clear();
        self.work_bytes = 0;
        self.pending.clear();
        self.pending_bytes = 0;
        self.verified_work.clear();
        self.responses.clear();
        self.response_bytes = 0;
        self.tx_pool.clear()?;
        for (id, _) in &self.production.broadcasts {
            self.tx_pool.relay.remove(id)?;
        }
        self.production = Default::default();
        self.vote_ids.clear();
        self.vote_order.clear();
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
        let previous_tip = tip.clone();
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
        let now = vnx::util::get_wall_time_millis();
        let ttl = self
            .chain
            .selected()
            .params()
            .block_interval_ms
            .saturating_mul(10);
        self.work.retain(|(work, received, size)| {
            let keep = work
                .field("start")
                .and_then(Value::uint)
                .is_ok_and(|start| start >= root.vdf_iters)
                && (hash(work, "content_hash")
                    .is_ok_and(|id| self.verified_work.contains_key(&id))
                    || now.saturating_sub(*received) <= ttl);
            if !keep {
                self.work_bytes -= *size;
                if let Ok(id) = hash(work, "content_hash") {
                    self.verified_work.remove(&id);
                }
            }
            keep
        });
        if let Some(worker) = &mut self.vdf_worker {
            let retained = self
                .work
                .iter()
                .map(|(work, _, _)| hash(work, "content_hash"))
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
        for (work, _, _) in &self.work {
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
                            // The fork can move while work runs. Admission
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
        let points = self.verified_work.values().cloned().collect::<Vec<_>>();
        if self.synced {
            // Native verify_vdf_success relays advancing work immediately,
            // before a farmer has produced the corresponding full block.
            for point in mmx_consensus::vdf::next_points(
                &self.chain,
                self.chain.selected().params(),
                &tip,
                &points,
            )? {
                let id = point.proof().content_hash.0;
                if !self.production.broadcasts.iter().any(|(old, _)| *old == id) {
                    self.broadcast(&Record::from_generated(point.proof())?)?;
                }
            }
        }
        let now = vnx::util::get_wall_time_millis();
        let ttl = self
            .chain
            .selected()
            .params()
            .block_interval_ms
            .saturating_mul(10);
        let mut remove = Vec::new();
        let mut pending: Vec<_> = self.pending.iter().collect();
        pending.sort_by_key(|(_, (block, _, _))| number(block, "height").unwrap_or(u32::MAX));
        let mut attempts = 0;
        for (id, (block, received, _)) in pending {
            if number(block, "height")? <= self.root || now.saturating_sub(*received) > ttl {
                remove.push(*id);
                continue;
            }
            if attempts >= 4 || self.chain.block(&hash(block, "prev")?)?.is_none() {
                continue;
            }
            // Waiting for time work must not spend the ledger-validation budget
            // on every tick and starve another block whose work is available.
            if mmx_consensus::vdf::link_block(
                block,
                self.chain.selected().params(),
                &self.chain,
                &points,
            )
            .is_err()
            {
                continue;
            }
            attempts += 1;
            match self.chain.admit_live(block, &points, self.root) {
                Ok(true) => remove.push(*id),
                Ok(false) => (),
                Err(e) if e.is_storage_error() => return Err(e.into()),
                Err(e) => {
                    eprintln!("Rejected quarantined block: {e}");
                    remove.push(*id);
                }
            }
        }
        for id in remove {
            if let Some((_, _, size)) = self.pending.remove(&id) {
                self.pending_bytes -= size;
            }
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
        for (vote, disposition) in &out.votes.processed {
            if *disposition == mmx_execution::chain::voting::Disposition::Accepted {
                self.broadcast(vote.record())?;
            }
        }
        for record in &out.validated {
            self.broadcast(record)?;
        }
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
        self.pending.retain(|_, (b, _, size)| {
            let keep = number(b, "height").is_ok_and(|h| h > self.root);
            if !keep {
                self.pending_bytes -= *size;
            }
            keep
        });
        self.prune_transactions()?;
        if self.synced {
            if let Some(common) = out.common_ancestor.filter(|h| *h != previous_tip.hash.0) {
                let mut old = previous_tip.hash.0;
                while old != common {
                    let Some(block) = self.chain.block(&old)? else {
                        break;
                    };
                    for tx in block.field("tx_list")?.seq()? {
                        let _ = self.add_transaction(tx.record()?, false);
                    }
                    old = hash(&block, "prev")?;
                }
            }
        }
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
            .filter_map(|(w, _, _)| {
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
        // Also prune on nodes with no local farmer/production worker.
        self.production.prune(root.vdf_height);
        let now = vnx::util::get_wall_time_millis();
        let height = self.vdf_peak()?.0;
        let mut pending = BTreeMap::new();
        self.response_bytes = 0;
        let mut checks = 0;
        for (id, (record, received, size)) in std::mem::take(&mut self.responses) {
            if now.wrapping_sub(received) > params.block_interval_ms.wrapping_mul(10) {
                continue;
            }
            if number(&record, "vdf_height")? >= height.wrapping_add(params.challenge_delay)
                || checks >= 16
            {
                pending.insert(id, (record, received, size));
                self.response_bytes += size;
                continue;
            }
            checks += 1;
            match mmx_consensus::response::verify(
                &record,
                &params,
                &self.chain,
                &tip,
                root.vdf_height,
                1000,
            ) {
                Ok(proof) => {
                    self.chain.observe_proof(&proof);
                    self.remember_proof(&record, &proof)?;
                    // Native verify() publishes node.verified_proof; Router
                    // relays it before the VDF makes a full block possible.
                    // These local fields are deliberately outside the hash.
                    let mut public = record.clone();
                    public
                        .fields
                        .insert("harvester".into(), Value::String(vec![]));
                    public.fields.insert("farmer_addr".into(), Value::Uint(0));
                    self.broadcast(&public)?;
                }
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
                let height = number(message, "height")?;
                let tip = self
                    .chain
                    .selected()
                    .tip()?
                    .ok_or_else(|| error("missing tip"))?;
                if height <= self.root || height > tip.height.saturating_add(1000) {
                    return Ok(false);
                }
                let id = hash(message, "hash")?;
                if self.pending.contains_key(&id) || self.chain.block(&id)?.is_some() {
                    return Ok(false);
                }
                let size = message.memory_size().max(message.encode_compact()?.len());
                if size > 32 * 1024 * 1024
                    || self.pending.len() >= 256
                    || self.pending_bytes.saturating_add(size) > 32 * 1024 * 1024
                {
                    return Err(error("live block quarantine full"));
                }
                mmx_execution::block::prevalidate(message, self.chain.selected().params())?;
                let mmx_consensus::Value::Int(stamp) = message.field("time_stamp")? else {
                    return Err(error("invalid block timestamp"));
                };
                if !mmx_execution::chain::sync::allows_timestamp(
                    *stamp,
                    vnx::util::get_wall_time_millis(),
                    self.chain.selected().params().block_interval_ms,
                    100,
                ) {
                    return Err(error("block too far in the future"));
                }
                self.pending.insert(
                    id,
                    (message.clone(), vnx::util::get_wall_time_millis(), size),
                );
                self.pending_bytes += size;
                update = true;
            }
            "mmx.ProofOfTime" => {
                let size = message.memory_size();
                if size > 32 * 1024 * 1024 {
                    return Err(error("VDF queue exceeds 32 MiB"));
                }
                if !mmx_consensus::vdf::is_valid(message)? {
                    return Err(error("invalid VDF content"));
                }
                mmx_consensus::vdf::authenticate(message)?;
                let id = hash(message, "content_hash")?;
                if self
                    .work
                    .iter()
                    .any(|(work, _, _)| hash(work, "content_hash").ok() == Some(id))
                {
                    return Ok(false);
                }
                let root_hash = self
                    .chain
                    .selected()
                    .hash_at(self.root)?
                    .ok_or_else(|| error("missing root"))?;
                let root = self
                    .chain
                    .selected()
                    .header(&root_hash)?
                    .ok_or_else(|| error("missing root header"))?;
                if message.field("start")?.uint()? < root.vdf_iters {
                    return Ok(false);
                }
                let size = message.memory_size().max(message.encode_compact()?.len());
                if size > 32 * 1024 * 1024
                    || self.work_bytes.saturating_add(size) > 32 * 1024 * 1024
                {
                    return Err(error("VDF queue exceeds 32 MiB"));
                }
                self.work_bytes += size;
                self.work
                    .push_back((message.clone(), vnx::util::get_wall_time_millis(), size));
                while self.work.len() > 32 {
                    if let Some((old, _, size)) = self.work.pop_front() {
                        self.work_bytes -= size;
                        self.verified_work.remove(&hash(&old, "content_hash")?);
                    }
                }
                update = true;
            }
            "mmx.ValidatorVote" => {
                let id = hash(message, "content_hash")?;
                if self.vote_ids.contains(&id) {
                    return Ok(false);
                }
                if message.memory_size() > 4096 || message.encode_compact()?.len() > 4096 {
                    return Err(error("validator vote exceeds 4 KiB"));
                }
                let vote = mmx_consensus::vote::verify(message)?;
                if !self
                    .chain
                    .enqueue_vote(vote, vnx::util::get_wall_time_millis())
                {
                    return Err(error("validator vote queue full"));
                }
                self.vote_ids.insert(id);
                self.vote_order.push_back(id);
                if self.vote_order.len() > 32_768 {
                    self.vote_ids.remove(&self.vote_order.pop_front().unwrap());
                }
                update = true;
            }
            "mmx.ProofResponse" => {
                let size = message.memory_size().max(message.encode_compact()?.len());
                if size > 64 * 1024 {
                    return Err(error("proof response exceeds 64 KiB"));
                }
                if !mmx_consensus::response::is_valid(message)? {
                    return Err(error("invalid proof response"));
                }
                // Authenticate before deferral. Future height is not permission
                // to retain an unsigned envelope, nor to refresh a replay's TTL.
                mmx_consensus::response::authenticate(message)?;
                let tip = self
                    .chain
                    .selected()
                    .tip()?
                    .ok_or_else(|| error("missing tip"))?;
                let params = self.chain.selected().params();
                if number(message, "vdf_height")?
                    > tip
                        .vdf_height
                        .saturating_add(params.max_vdf_count)
                        .saturating_add(params.challenge_delay)
                {
                    return Err(error("proof response too far ahead"));
                }
                let id = hash(message, "content_hash")?;
                if self.responses.contains_key(&id)
                    || self.production.broadcasts.iter().any(|(old, _)| *old == id)
                {
                    return Ok(false);
                }
                if self.responses.len() >= 4096
                    || self.response_bytes.saturating_add(size) > 8 * 1024 * 1024
                {
                    return Err(error("proof response queue full"));
                }
                self.responses.insert(
                    id,
                    (message.clone(), vnx::util::get_wall_time_millis(), size),
                );
                self.response_bytes += size;
            }
            "mmx.Transaction" if self.synced => {
                self.add_transaction(message, false)?;
            }
            // Unrecognized broadcasts have no ledger effects.
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
        if crate::transactions::is_balance_method(&request.name) {
            return crate::transactions::balance_response(self.chain.selected().storage(), request);
        }
        let selected = self.chain.selected();
        let optional = |v: Option<Value>| Value::Optional(v.map(Box::new));
        let block_value = |v: Option<Record>| v.map(Value::Record).unwrap_or(Value::Null);
        let block_at = |height| -> Result<Option<Record>> {
            if height < self.earliest_height() {
                return Ok(None);
            }
            match selected.hash_at(height)? {
                Some(h) => Ok(self.chain.block(&h)?),
                None => Ok(None),
            }
        };
        let visible_block = |hash: &Hash| -> Result<Option<Record>> {
            Ok(self
                .chain
                .block(hash)?
                .filter(|b| number(b, "height").is_ok_and(|h| h >= self.earliest_height())))
        };
        let visible_index = |id: &Hash| -> Result<_> {
            Ok(selected
                .storage()
                .get_transaction_index(id)?
                .filter(|i| i.height >= self.earliest_height()))
        };
        let visible_transaction = |id: &Hash| -> Result<Option<Record>> {
            if visible_index(id)?.is_none() {
                return Ok(None);
            }
            Ok(selected.transaction(id)?)
        };
        let visible_header = |hash: &Hash| -> Result<Option<Record>> {
            Ok(self.chain.header(hash)?.filter(|h| {
                number(h, "height").is_ok_and(|height| height >= self.earliest_height())
            }))
        };
        let header_at = |height| -> Result<Option<Record>> {
            if height < self.earliest_height() {
                return Ok(None);
            }
            match selected.hash_at(height)? {
                Some(hash) => visible_header(&hash),
                None => Ok(None),
            }
        };
        let value = match request.name.as_str() {
            "mmx.Node.validate" => Value::Record(
                self.validate_transaction(request.field("tx")?.record()?)?
                    .to_record(),
            ),
            "mmx.Node.get_params" => Value::Record(Record::from_generated(selected.params())?),
            "mmx.Node.get_vdf_height" => Value::Uint(self.vdf_peak()?.0.into()),
            "mmx.Node.get_synced_vdf_height" => optional(
                self.synced
                    .then_some(Value::Uint(self.vdf_peak()?.0.into())),
            ),
            "mmx.Node.get_vdf_peak" => bytes(self.vdf_peak()?.1),
            "mmx.Node.get_block" => block_value(visible_block(&hash(request, "hash")?)?),
            "mmx.Node.get_block_at" => block_value(block_at(number(request, "height")?)?),
            "mmx.Node.get_header" => block_value(visible_header(&hash(request, "hash")?)?),
            "mmx.Node.get_header_at" => block_value(header_at(number(request, "height")?)?),
            "mmx.Node.get_block_hash" => {
                let height = number(request, "height")?;
                optional(if height < self.earliest_height() {
                    None
                } else {
                    selected.hash_at(height)?.map(bytes)
                })
            }
            "mmx.Node.get_block_hash_ex" => optional(
                header_at(number(request, "height")?)?
                    .map(|b| {
                        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(Value::Sequence(vec![
                            bytes(hash(&b, "hash")?),
                            bytes(hash(&b, "content_hash")?),
                        ]))
                    })
                    .transpose()?,
            ),
            "mmx.Node.get_transaction" => {
                let id = hash(request, "id")?;
                let pending = request.field("pending")?.boolean()?;
                block_value(if pending {
                    if let Some(entry) = self.tx_pool.entries.get(&id) {
                        Some(entry.record.as_ref().clone())
                    } else {
                        visible_transaction(&id)?
                    }
                } else {
                    visible_transaction(&id)?
                })
            }
            "mmx.Node.get_transactions" => {
                let ids = request.field("ids")?.seq()?;
                if ids.len() > 128 {
                    return Err(error("transaction query exceeds 128 ids"));
                }
                let mut values = Vec::new();
                let mut size = 0usize;
                for value in ids {
                    let id = value.bytes()?.try_into()?;
                    let tx = visible_transaction(&id)?;
                    size = size.saturating_add(tx.as_ref().map_or(0, Record::memory_size));
                    if size > 32 * 1024 * 1024 {
                        return Err(error("transaction query exceeds 32 MiB"));
                    }
                    values.push(block_value(tx));
                }
                Value::Sequence(values)
            }
            "mmx.Node.get_tx_height" => optional(
                visible_index(&hash(request, "id")?)?.map(|i| Value::Uint(i.height.into())),
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
            _ => return Err(error(format!("unsupported read method: {}", request.name))),
        };
        Ok(record(
            &format!("{}.return", request.name),
            [("_ret_0".into(), value)],
        ))
    }
}

#[cfg(test)]
mod work_queue_tests {
    use super::*;
    use mmx_execution::chain::genesis::Assets;
    use mmx_types::VnxType;
    use vnx::serde_json::{self, Value as Json};
    fn setup() -> (Node, Record) {
        let rows: Vec<Json> = include_str!("../../mmx-execution/tests/fixtures/chain.jsonl")
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let g = rows.iter().find(|r| r["name"] == "synthetic").unwrap();
        let raw = |v: &Json| vnx::util::from_hex_string(v.as_str().unwrap()).unwrap();
        let params = mmx::ChainParams::decode(&raw(&g["params"])).unwrap();
        let assets = Assets {
            binaries: g["binaries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| Record::decode(&raw(v)).unwrap())
                .collect::<Vec<_>>()
                .try_into()
                .unwrap(),
            testnet_rewards: None,
            project_relay: None,
        };
        let mut chain = ForkChain::memory(params);
        chain.initialize(&assets).unwrap();
        let work = &rows.iter().find(|r| r["kind"] == "chain").unwrap()["steps"][0]["vdf"];
        (
            Node::from_chain(chain, SigningKey::from_slice(&[1; 32]).unwrap()).unwrap(),
            Record::decode(&raw(work)).unwrap(),
        )
    }
    #[test]
    fn waiting_vdf_expires_without_refresh_but_verified_work_lives_until_root_advances() {
        let (mut node, work) = setup();
        node.admit_message(&work).unwrap();
        let ttl = node.chain.selected().params().block_interval_ms * 10;
        // Receipt does not refresh an existing item's original arrival time.
        node.work[0].1 -= ttl + 1000;
        node.admit_message(&work).unwrap();
        assert_eq!(node.work.len(), 1);
        node.tick().unwrap();
        assert!(node.work.is_empty());
        assert_eq!(node.work_bytes, 0);
        // Native verified vdf_index entries survive the waiting-queue TTL.
        node.receive(&work).unwrap();
        node.tick().unwrap();
        assert_eq!(node.verified_work.len(), 1);
        node.work[0].1 -= ttl + 1000;
        node.tick().unwrap();
        assert_eq!(node.work.len(), 1);
        assert_eq!(node.verified_work.len(), 1);
        assert!(node.work_bytes > 0);
        node.rollback_to(0).unwrap();
        assert_eq!(node.work_bytes, 0);
        assert!(node.work.is_empty());
    }
    #[test]
    fn vdf_budget_charges_decoded_heap_before_retaining_or_verifying_work() {
        let (mut node, work) = setup();
        let mut oversized = work.clone();
        // Extra unhashed data cannot evade the retained-byte budget.
        oversized
            .fields
            .insert("extra".into(), Value::Bytes(vec![0; 32 * 1024 * 1024]));
        assert!(node.receive(&oversized).is_err());
        assert!(node.work.is_empty());
        assert_eq!(node.work_bytes, 0);
        node.receive(&work).unwrap();
        assert_eq!(
            node.work_bytes,
            work.memory_size().max(work.encode_compact().unwrap().len())
        );
    }
}
