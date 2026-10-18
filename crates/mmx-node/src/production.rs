//! Local candidate construction. Plans pin a parent snapshot; execution and
//! farmer RPC run outside the live ledger mutex. Every signed result goes back
//! through the normal consensus admission path before peer publication.
use crate::{
    error,
    node::{hash, number},
    Node, Result,
};
use mmx_consensus::{
    header, proof, response,
    transitions::{self, history::HeaderSource},
    vdf, vote, Hash, Record, Value as N,
};
use mmx_execution::{
    block::{self, economics},
    chain::ChainState,
    nft,
};
use mmx_types::mmx;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use vnx::{
    net::NetworkClient,
    serde_json::{self, json},
};

#[derive(Default)]
pub(crate) struct State {
    proofs: BTreeMap<Hash, BTreeMap<Hash, Record>>,
    attempted: BTreeMap<Hash, u32>,
    voted: BTreeMap<Hash, (Hash, i64)>,
    pub(crate) broadcasts: VecDeque<(Hash, usize)>,
    bytes: usize,
}
pub struct Candidate {
    block: Record,
    anchor: Hash,
    work: Vec<Record>,
}
impl Candidate {
    pub fn record(&self) -> &Record {
        &self.block
    }
}
pub struct Plan {
    anchor: Hash,
    parent: ChainState,
    previous: mmx::BlockHeader,
    points: Vec<vdf::VerifiedPoint>,
    proofs: Vec<Record>,
    pending: Vec<Arc<Record>>,
    now: i64,
    reward_vote: i8,
}
fn put(r: &mut Record, key: &str, v: u64) {
    r.fields.insert(key.into(), N::Uint(v));
}
fn optional(r: &mut Record, key: &str, v: Option<Hash>) {
    r.fields.insert(
        key.into(),
        N::Optional(v.map(|v| Box::new(N::Bytes(v.to_vec())))),
    );
}
impl Node {
    pub(crate) fn remember_proof(
        &mut self,
        record: &Record,
        verified: &proof::VerifiedProof,
    ) -> Result<()> {
        if self.production.proofs.len() >= 1000
            && !self.production.proofs.contains_key(verified.challenge())
        {
            return Err(error("too many farming challenges"));
        }
        let proofs = self
            .production
            .proofs
            .entry(*verified.challenge())
            .or_default();
        if proofs.len() < 128 {
            proofs
                .entry(*verified.hash())
                .or_insert_with(|| record.clone());
        }
        Ok(())
    }
    pub fn farming_plan(
        &mut self,
        keys: &BTreeSet<[u8; 33]>,
        now: i64,
        reward_vote: i8,
    ) -> Result<Option<Plan>> {
        if !self.is_synced() || keys.is_empty() {
            return Ok(None);
        }
        if !(-1..=1).contains(&reward_vote) {
            return Err(error("reward vote must be -1, 0 or 1"));
        }
        let tip = self
            .chain
            .selected()
            .tip()?
            .ok_or_else(|| error("missing farming tip"))?;
        let root = self
            .chain
            .selected()
            .header(
                &self
                    .chain
                    .selected()
                    .hash_at(self.root)?
                    .ok_or_else(|| error("missing farming root"))?,
            )?
            .ok_or_else(|| error("missing farming root header"))?;
        self.production.proofs.retain(|_, proofs| {
            proofs.retain(|_, r| number(r, "vdf_height").is_ok_and(|h| h > root.vdf_height));
            !proofs.is_empty()
        });
        self.production
            .attempted
            .retain(|_, height| *height > root.vdf_height);
        let points: Vec<_> = self.verified_work.values().cloned().collect();
        // Native update first tries replacing an uncommitted peak, then extends it.
        let mut parents = vec![];
        if tip.height > self.root {
            if let Some(p) = self.chain.get_header(&tip.prev.0)? {
                parents.push(p);
            }
        }
        parents.push(tip.clone());
        for parent in parents {
            let next = vdf::next_points(
                &self.chain,
                self.chain.selected().params(),
                &parent,
                &points,
            )?;
            if next.is_empty() {
                continue;
            }
            let height = parent
                .vdf_height
                .checked_add(next.len().try_into()?)
                .ok_or_else(|| error("VDF height overflow"))?;
            let (challenge, _) = transitions::history::challenge(
                &self.chain,
                self.chain.selected().params(),
                &parent,
                next.len().try_into()?,
            )?;
            let Some(proofs) = self.production.proofs.get(&challenge) else {
                continue;
            };
            let Some((&best, first)) = proofs.first_key_value() else {
                continue;
            };
            if self.chain.best_proof(&challenge).is_some_and(|h| h != best)
                || self.production.attempted.contains_key(&best)
            {
                continue;
            }
            let key: [u8; 33] = first
                .field("proof")?
                .record()?
                .field("farmer_key")?
                .bytes()?
                .try_into()?;
            if !keys.contains(&key) {
                continue;
            }
            let mut selected = vec![];
            for record in proofs
                .values()
                .take(self.chain.selected().params().max_proof_count as usize)
            {
                // Fork movement and protocol boundaries must recheck eligibility.
                if response::verify(
                    record,
                    self.chain.selected().params(),
                    &self.chain,
                    &tip,
                    root.vdf_height,
                    1000,
                )
                .is_ok()
                {
                    selected.push(record.field("proof")?.record()?.clone());
                }
            }
            if selected.is_empty() || proof::calc_proof_hash(&selected[0])? != best {
                continue;
            }
            let snapshot = self.chain.selected().snapshot_at(parent.height)?;
            let plan = Plan {
                anchor: tip.hash.0,
                parent: snapshot,
                previous: parent,
                points: next.into_iter().cloned().collect(),
                proofs: selected,
                pending: self
                    .tx_pool
                    .entries
                    .values()
                    .map(|e| e.record.clone())
                    .collect(),
                now,
                reward_vote,
            };
            self.production.attempted.insert(best, height);
            return Ok(Some(plan));
        }
        Ok(None)
    }
    pub fn accept_produced(&mut self, candidate: &Candidate) -> Result<bool> {
        let block = &candidate.block;
        if !self.synced
            || self
                .chain
                .selected()
                .tip()?
                .is_none_or(|tip| tip.hash.0 != candidate.anchor)
        {
            return Ok(false);
        }
        if self
            .chain
            .best_proof(&hash(
                block.field("proof")?.seq()?[0].record()?,
                "challenge",
            )?)
            .is_some_and(|best| best != hash(block, "proof_hash").unwrap_or_default())
        {
            return Ok(false);
        }
        self.chain.admit(block, &candidate.work)?;
        self.update()?;
        self.broadcast(block)?;
        Ok(true)
    }
    pub(crate) fn broadcast(&mut self, record: &Record) -> Result<()> {
        let id = hash(record, "content_hash")?;
        if self.production.broadcasts.iter().any(|(old,_)|*old==id) {return Ok(());}
        let size = record.encode_compact()?.len();
        if size > 32 * 1024 * 1024 {
            return Err(error("broadcast exceeds queue bound"));
        }
        while self.production.broadcasts.len() >= 128
            || self.production.bytes + size > 32 * 1024 * 1024
        {
            let (old, n) = self.production.broadcasts.pop_front().unwrap();
            self.production.bytes -= n;
            self.tx_pool.relay.remove(&old)?;
        }
        self.tx_pool
            .relay
            .publish(id, Arc::new(record.clone()), size)?;
        self.production.broadcasts.push_back((id, size));
        self.production.bytes += size;
        Ok(())
    }
    pub fn farming_votes(&mut self, keys: &BTreeSet<[u8; 33]>, now: i64) -> Result<Vec<Record>> {
        if !self.synced {
            return Ok(vec![]);
        }
        let tip = self
            .chain
            .selected()
            .tip()?
            .ok_or_else(|| error("missing vote tip"))?;
        let p = self.chain.selected().params();
        let mut current = tip;
        let mut out = vec![];
        self.production.voted.retain(|_, (_, time)| {
            now.saturating_sub(*time) < p.block_interval_ms.saturating_mul(1000)
        });
        while current.height > self.root {
            let proof = current
                .proof
                .first()
                .ok_or_else(|| error("missing vote proof"))?;
            let challenge = hash(&crate::rpc::from_json_record(proof)?, "challenge")?;
            if self.chain.best_proof(&challenge) == Some(current.proof_hash.0)
                && self
                    .production
                    .voted
                    .get(&current.prev.0)
                    .is_none_or(|(h, t)| {
                        *h != current.hash.0 && now.saturating_sub(*t) > p.block_interval_ms
                    })
            {
                let before=out.len();
                for key in vote::validators(&self.chain, p, &current, 1000)?.intersection(keys) {
                    out.push(Record::from_generated(&mmx::ValidatorVote {
                        hash: current.hash.clone(),
                        farmer_key: (*key).into(),
                        ..Default::default()
                    })?);
                }
                if out.len()>before {
                    self.production
                        .voted
                        .insert(current.prev.0, (current.hash.0, now));
                }
            }
            current = self
                .chain
                .get_header(&current.prev.0)?
                .ok_or_else(|| error("missing voting ancestor"))?;
        }
        Ok(out)
    }
}
impl Plan {
    pub fn build(&self) -> Result<Record> {
        let p = self.parent.params();
        if p.reward_adjust_interval == 0 || p.max_diff_adjust >= 64 || p.block_interval_ms <= 0 {
            return Err(error("invalid block production parameters"));
        }
        let prev = &self.previous;
        let count: u32 = self.points.len().try_into()?;
        let height = prev
            .height
            .checked_add(1)
            .ok_or_else(|| error("block height overflow"))?;
        let mut block = Record::from_generated(&mmx::Block::default())?;
        for (key, value) in [
            ("height", height.into()),
            ("vdf_count", count.into()),
            ("vdf_height", prev.vdf_height.wrapping_add(count).into()),
            ("time_diff", prev.time_diff),
            ("support_flags", 3),
            ("txfee_buffer", economics::new_txfee_buffer(prev)),
        ] {
            put(&mut block, key, value);
        }
        block
            .fields
            .insert("prev".into(), N::Bytes(prev.hash.0.to_vec()));
        block.fields.insert(
            "project_addr".into(),
            N::Bytes(prev.project_addr.0.to_vec()),
        );
        let end = self
            .points
            .last()
            .ok_or_else(|| error("no production VDF"))?
            .point();
        put(
            &mut block,
            "vdf_iters",
            end.start.wrapping_add(end.num_iters),
        );
        block
            .fields
            .insert("vdf_output".into(), N::Bytes(end.output.0.to_vec()));
        block.fields.insert(
            "vdf_reward_addr".into(),
            N::Sequence(
                self.points
                    .iter()
                    .map(|p| N::Bytes(p.point().reward_addr.0.to_vec()))
                    .collect(),
            ),
        );
        block.fields.insert(
            "proof".into(),
            N::Sequence(self.proofs.iter().cloned().map(N::Record).collect()),
        );
        let best = proof::calc_proof_hash(&self.proofs[0])?;
        block
            .fields
            .insert("proof_hash".into(), N::Bytes(best.to_vec()));
        let chain = transitions::proof_chain(p, prev, &best);
        block
            .fields
            .insert("proof_chain".into(), N::Bytes(chain.to_vec()));
        let (challenge, fork) = transitions::next_challenge(p, prev, height, count, &best, &chain)?;
        block
            .fields
            .insert("challenge".into(), N::Bytes(challenge.to_vec()));
        block.fields.insert("is_space_fork".into(), N::Bool(fork));
        put(
            &mut block,
            "space_diff",
            if fork {
                transitions::new_space_diff(p, prev)?
            } else {
                prev.space_diff
            },
        );
        put(
            &mut block,
            "space_fork_len",
            if fork {
                count
            } else {
                prev.space_fork_len.wrapping_add(count)
            }
            .into(),
        );
        put(
            &mut block,
            "space_fork_proofs",
            if fork {
                self.proofs.len() as u32
            } else {
                prev.space_fork_proofs
                    .wrapping_add(self.proofs.len() as u32)
            }
            .into(),
        );
        let weight = transitions::block_weight(p, &block, prev)?;
        block
            .fields
            .insert("weight".into(), N::Bytes(weight.to_le_bytes().to_vec()));
        block.fields.insert(
            "total_weight".into(),
            N::Bytes(
                u128::from_le_bytes(prev.total_weight.0)
                    .wrapping_add(weight)
                    .to_le_bytes()
                    .to_vec(),
            ),
        );
        let mut reference = prev.clone();
        for _ in 0..100 {
            if reference.height == 0 {
                break;
            }
            reference = self
                .parent
                .get_header(&reference.prev.0)?
                .ok_or_else(|| error("missing time difficulty ancestor"))?;
        }
        if prev.height >= 100 && reference.height < prev.height {
            let delta = prev.time_stamp.wrapping_sub(reference.time_stamp);
            if delta <= 0 {
                return Err(error("non-positive difficulty time span"));
            }
            let factor = (p
                .block_interval_ms
                .wrapping_mul(i64::from(prev.height - reference.height)))
                as f64
                / delta as f64;
            let delayed = |mut h: mmx::BlockHeader| -> Result<mmx::BlockHeader> {
                for _ in 0..p.commit_delay.wrapping_add(p.infuse_delay) {
                    if h.height == 0 {
                        break;
                    }
                    h = self
                        .parent
                        .get_header(&h.prev.0)?
                        .ok_or_else(|| error("missing delayed difficulty ancestor"))?;
                }
                Ok(h)
            };
            let begin = delayed(reference)?;
            let end = delayed(prev.clone())?;
            let avg = begin.time_diff.wrapping_add(end.time_diff) / 2;
            let next = ((avg as f64 * factor + 0.5) as i64).max(p.time_diff_divider as i64) as u64;
            let max = (prev.time_diff >> p.max_diff_adjust).max(1);
            put(
                &mut block,
                "time_diff",
                next.max(prev.time_diff.saturating_sub(max))
                    .min(prev.time_diff.wrapping_add(max)),
            );
        }
        let delta = self
            .now
            .wrapping_sub(prev.time_stamp)
            .min(
                i64::from(count)
                    .wrapping_mul(p.block_interval_ms)
                    .wrapping_mul(2),
            )
            .max(i64::from(count).wrapping_mul(p.block_interval_ms) / 2);
        block.fields.insert(
            "time_stamp".into(),
            N::Int(prev.time_stamp.wrapping_add(delta)),
        );
        optional(
            &mut block,
            "vdf_reward_payout",
            self.parent.vdf_payout(height, prev)?,
        );
        if self.proofs[0].name == "mmx.ProofOfSpaceNFT" {
            let contract = hash(&self.proofs[0], "contract")?;
            optional(&mut block, "reward_contract", Some(contract));
            let mut any = [0; 32];
            use rand::RngCore;
            rand::rngs::OsRng.fill_bytes(&mut any);
            let target = nft::target(self.parent.storage(), p, prev.height, contract, Some(any))?;
            if target != any {
                optional(&mut block, "reward_addr", Some(target));
            }
        }
        block
            .fields
            .insert("reward_vote".into(), N::Int(self.reward_vote.into()));
        let reset = height.is_multiple_of(p.reward_adjust_interval);
        let base = if reset {
            economics::new_base_reward(p, prev)?
        } else {
            prev.base_reward
        };
        put(&mut block, "base_reward", base);
        block.fields.insert(
            "reward_vote_sum".into(),
            N::Int(
                if reset {
                    self.reward_vote as i32
                } else {
                    prev.reward_vote_sum.wrapping_add(self.reward_vote as i32)
                }
                .into(),
            ),
        );
        put(
            &mut block,
            "reward_vote_count",
            if reset {
                u32::from(self.reward_vote != 0)
            } else {
                prev.reward_vote_count
                    .wrapping_add(u32::from(self.reward_vote != 0))
            }
            .into(),
        );
        let txs = block::assemble::transactions(
            self.parent.storage(),
            p,
            height,
            &self.pending,
            Instant::now() + Duration::from_millis((p.block_interval_ms / 2).max(1) as u64),
        )?;
        let mut static_cost = 0u64;
        let mut total_cost = 0u64;
        let mut fees = 0u64;
        let mut hashes = vec![];
        for tx in &txs {
            static_cost += tx.field("static_cost")?.uint()?;
            let receipt = tx.field("exec_result")?.optional()?.unwrap().record()?;
            total_cost += receipt.field("total_cost")?.uint()?;
            fees += receipt.field("total_fee")?.uint()?;
            hashes.push(hash(tx, "content_hash")?);
        }
        for (key, n) in [
            ("static_cost", static_cost),
            ("total_cost", total_cost),
            ("tx_fees", fees),
            ("tx_count", txs.len() as u64),
            (
                "reward_amount",
                economics::block_reward(p, prev, height, true, fees)?,
            ),
        ] {
            put(&mut block, key, n);
        }
        block.fields.insert(
            "tx_list".into(),
            N::Sequence(txs.into_iter().map(N::Record).collect()),
        );
        block.fields.insert(
            "tx_hash".into(),
            N::Bytes(block::transaction_root(&hashes).to_vec()),
        );
        block.fields.insert(
            "hash".into(),
            N::Bytes(header::calc_hash(&block, p)?.to_vec()),
        );
        Ok(block)
    }
    pub fn with_signature(&self, unsigned: &Record, signed: &Record) -> Result<Candidate> {
        let allowed = [
            "nonce",
            "reward_addr",
            "reward_account",
            "hash",
            "farmer_sig",
            "content_hash",
        ];
        for (key, value) in &unsigned.fields {
            if key != "tx_list"
                && !allowed.contains(&key.as_str())
                && signed.fields.get(key) != Some(value)
            {
                return Err(error(format!("farmer changed block field {key}")));
            }
        }
        let mut block = unsigned.clone();
        for key in allowed {
            block.fields.insert(key.into(), signed.field(key)?.clone());
        }
        let mut snapshot = mmx_vm::StorageDb::new(
            self.parent
                .storage()
                .database()
                .fork(self.parent.storage().database().version())?,
        );
        let context = block::BlockContext {
            params: self.parent.params().clone(),
            previous: self.previous.clone(),
            expected_vdf_payout: self
                .parent
                .vdf_payout(number(&block, "height")?, &self.previous)?,
        };
        block::prepare_with_vdf(&mut snapshot, &context, &block, &self.parent, &self.points)?;
        Ok(Candidate {
            block,
            anchor: self.anchor,
            work: self
                .points
                .iter()
                .map(|p| Record::from_generated(p.proof()))
                .collect::<mmx_consensus::Result<Vec<_>>>()?,
        })
    }
}

pub struct Worker {
    thread: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}
impl Worker {
    pub fn start(
        node: Arc<Mutex<Node>>,
        endpoint: vnx::net::Endpoint,
        stop: Arc<AtomicBool>,
        reward_vote: i8,
    ) -> Result<Self> {
        if !(-1..=1).contains(&reward_vote) {
            return Err(error("invalid reward vote"));
        }
        let running = stop.clone();
        let thread = thread::Builder::new()
            .name("mmx-farming".into())
            .spawn(move || {
                let run = || -> Result<()> {
                    let farmer = NetworkClient::connect(
                        &endpoint,
                        "Farmer",
                        mmx_types::vnx_registry()?,
                        Duration::from_secs(5),
                    )?;
                    let mut keys = BTreeSet::new();
                    let mut refreshed = Instant::now() - Duration::from_secs(60);
                    while !running.load(Ordering::Relaxed) {
                        if refreshed.elapsed() >= Duration::from_secs(60) {
                            let raw = farmer.call_value("mmx.Farmer.get_farmer_keys", json!({}))?;
                            let public: Vec<vnx::Fixed<u8, 33>> =
                                serde_json::from_value(raw["_ret_0"].clone())?;
                            keys = public.into_iter().map(|p| p.0).collect();
                            refreshed = Instant::now();
                        }
                        let now = vnx::util::get_wall_time_millis();
                        let plan = node
                            .lock()
                            .map_err(|_| error("node lock poisoned"))?
                            .farming_plan(&keys, now, reward_vote)?;
                        if let Some(plan) = plan {
                            let produce = || -> Result<Candidate> {
                                let block = plan.build()?;
                                let mut request = block.clone();
                                request.name = "mmx.BlockHeader".into();
                                request.fields.remove("tx_list");
                                let signed = farmer.call_value(
                                    "mmx.Farmer.sign_block",
                                    json!({"block":crate::rpc::to_json_record(&request)?}),
                                )?;
                                let signed = crate::rpc::from_json_record(&signed["_ret_0"])?;
                                plan.with_signature(&block, &signed)
                            };
                            match produce() {
                                Ok(candidate) => {
                                    let height =
                                        number(candidate.record(), "height").unwrap_or_default();
                                    if node
                                        .lock()
                                        .map_err(|_| error("node lock poisoned"))?
                                        .accept_produced(&candidate)?
                                    {
                                        println!("Created and broadcast block at height {height}");
                                    }
                                }
                                Err(e) => eprintln!("Block production failed: {e}"),
                            }
                        }
                        let votes = node
                            .lock()
                            .map_err(|_| error("node lock poisoned"))?
                            .farming_votes(&keys, now)?;
                        for mut vote in votes {
                            let result = farmer.call_value(
                                "mmx.Farmer.sign_vote",
                                json!({"vote":crate::rpc::to_json_record(&vote)?}),
                            )?;
                            let signature: vnx::Fixed<u8, 64> =
                                serde_json::from_value(result["_ret_0"].clone())?;
                            vote.fields
                                .insert("farmer_sig".into(), N::Bytes(signature.0.to_vec()));
                            vote.fields.insert(
                                "content_hash".into(),
                                N::Bytes(vote::content_hash(&vote)?.to_vec()),
                            );
                            let mut n = node.lock().map_err(|_| error("node lock poisoned"))?;
                            n.receive(&vote)?;
                            n.broadcast(&vote)?;
                        }
                        for _ in 0..10 {
                            if running.load(Ordering::Relaxed) {
                                break;
                            }
                            thread::sleep(Duration::from_millis(20));
                        }
                    }
                    farmer.close();
                    Ok(())
                };
                while !running.load(Ordering::Relaxed) {
                    if let Err(e) = run() {
                        eprintln!("Farming connection: {e}");
                    }
                    for _ in 0..50 {
                        if running.load(Ordering::Relaxed) {
                            break;
                        }
                        thread::sleep(Duration::from_millis(100));
                    }
                }
            })?;
        Ok(Self {
            thread: Some(thread),
            stop,
        })
    }
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.shutdown();
    }
}
