//! Transient native vote queue, validator membership and proof ranking. Counts
//! stay fixed once accepted, and reset on process restart as in the native node.
use mmx_consensus::{
    proof::VerifiedProof,
    transitions::history::HeaderSource,
    vote::{self, FarmerKey, VerifiedVote, VoteResult},
    Hash, Record,
};
use mmx_types::mmx;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    Accepted,
    Duplicate,
    WeakerProof,
    NotValidator,
    Expired,
}
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub processed: Vec<(VerifiedVote, Disposition)>,
    pub pending: usize,
}
#[derive(Clone)]
struct Fork {
    height: u32,
    challenge: Hash,
    proof_hash: Hash,
    validators: BTreeMap<FarmerKey, bool>,
    votes: u32,
}

pub struct Voting {
    forks: BTreeMap<Hash, Fork>,
    proofs: BTreeMap<Hash, (u32, BTreeSet<Hash>)>,
    queue: Vec<(VerifiedVote, i64)>,
    queued: BTreeSet<Hash>,
    root_height: Option<u32>,
    max_history: u32,
}
impl Default for Voting {
    fn default() -> Self {
        Self {
            forks: BTreeMap::new(),
            proofs: BTreeMap::new(),
            queue: vec![],
            queued: BTreeSet::new(),
            root_height: None,
            max_history: 1000,
        }
    }
}
impl Voting {
    pub fn history_limit(&self) -> u32 {
        self.max_history
    }
    pub fn set_history_limit(&mut self, max_history: u32) {
        self.max_history = max_history;
    }
    pub fn reset(&mut self) {
        let max_history = self.max_history;
        *self = Self::default();
        self.max_history = max_history;
    }
    pub fn count(&self, hash: &Hash) -> u32 {
        self.forks.get(hash).map_or(0, |fork| fork.votes)
    }
    pub fn pending(&self) -> usize {
        self.queue.len()
    }
    pub fn best_proof(&self, challenge: &Hash) -> Option<Hash> {
        self.proofs
            .get(challenge)
            .and_then(|(_, proofs)| proofs.first().copied())
    }
    pub fn observe_proof(&mut self, proof: &VerifiedProof) {
        self.add_proof(*proof.challenge(), *proof.hash(), proof.vdf_height());
    }
    fn add_proof(&mut self, challenge: Hash, hash: Hash, height: u32) {
        self.proofs
            .entry(challenge)
            .or_insert_with(|| (height, BTreeSet::new()))
            .1
            .insert(hash);
    }
    /// Call at root advancement, before registering newly verified forks.
    pub fn root(&mut self, height: u32, vdf_height: u32) {
        if self.root_height.is_some_and(|old| height < old) {
            self.proofs.clear();
            self.forks.clear();
        }
        if self.root_height.is_some_and(|old| height > old) {
            self.proofs.retain(|_, (first, _)| *first > vdf_height);
            self.forks.retain(|_, fork| fork.height > height);
        }
        self.root_height = Some(height);
    }
    pub(crate) fn register<S: HeaderSource>(
        &mut self,
        block: &Record,
        header: &mmx::BlockHeader,
        params: &mmx::ChainParams,
        source: &S,
    ) -> VoteResult<()> {
        let hash = header.hash.0;
        if self.forks.contains_key(&hash) {
            return Ok(());
        }
        let keys = vote::validators(source, params, header, self.max_history)?;
        let proof = block
            .field("proof")?
            .seq()?
            .first()
            .ok_or_else(|| mmx_consensus::Error::Input("missing voting proof".into()))?
            .record()?;
        let challenge: Hash = proof
            .field("challenge")?
            .bytes()?
            .try_into()
            .map_err(|_| mmx_consensus::Error::Input("voting challenge length".into()))?;
        self.add_proof(challenge, header.proof_hash.0, header.vdf_height);
        self.forks.insert(
            hash,
            Fork {
                height: header.height,
                challenge,
                proof_hash: header.proof_hash.0,
                validators: keys.into_iter().map(|k| (k, false)).collect(),
                votes: 0,
            },
        );
        Ok(())
    }
    pub fn admit(&mut self, record: &Record, now_ms: i64, interval_ms: i64) -> VoteResult<Report> {
        let vote = vote::verify(record)?;
        self.queued.insert(*vote.content_hash());
        self.queue.push((vote, now_ms));
        Ok(self.process(now_ms, interval_ms))
    }
    /// Runtime admission is batched after block-proof verification, as in
    /// native Node::verify_votes. Duplicate arrivals never refresh the timeout.
    /// Keep the direct `admit` API's native fixture semantics unchanged.
    pub fn enqueue(&mut self, vote: VerifiedVote, now_ms: i64) -> bool {
        if self.queued.contains(vote.content_hash()) {
            return true;
        }
        if self.queue.len() >= 4096 {
            return false;
        }
        self.queued.insert(*vote.content_hash());
        self.queue.push((vote, now_ms));
        true
    }
    pub fn process(&mut self, now_ms: i64, interval_ms: i64) -> Report {
        let mut report = Report::default();
        let mut pending = Vec::new();
        self.queued.clear();
        for (vote, received) in std::mem::take(&mut self.queue) {
            let result = if now_ms.wrapping_sub(received) > interval_ms.wrapping_mul(3) {
                Some(Disposition::Expired)
            } else if let Some(fork) = self.forks.get_mut(vote.block_hash()) {
                let best = self
                    .proofs
                    .get(&fork.challenge)
                    .and_then(|(_, proofs)| proofs.first());
                if best != Some(&fork.proof_hash) {
                    Some(Disposition::WeakerProof)
                } else if let Some(counted) = fork.validators.get_mut(vote.farmer_key()) {
                    if *counted {
                        Some(Disposition::Duplicate)
                    } else {
                        *counted = true;
                        fork.votes = fork.votes.wrapping_add(1);
                        Some(Disposition::Accepted)
                    }
                } else {
                    Some(Disposition::NotValidator)
                }
            } else {
                None
            };
            if let Some(result) = result {
                report.processed.push((vote, result));
            } else {
                self.queued.insert(*vote.content_hash());
                pending.push((vote, received));
            }
        }
        self.queue = pending;
        report.pending = self.queue.len();
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vnx::serde_json::{self, Value};
    fn hash(n: u64) -> Hash {
        let mut h = [0; 32];
        h[..8].copy_from_slice(&n.to_le_bytes());
        h
    }
    fn unhex(v: &Value) -> Vec<u8> {
        vnx::util::from_hex_string(v.as_str().unwrap()).unwrap()
    }
    #[test]
    fn batched_runtime_votes_bound_unknown_blocks_and_never_refresh_duplicate_timeouts() {
        use k256::ecdsa::{signature::hazmat::PrehashSigner, Signature, SigningKey};
        use mmx_consensus::Value;
        let key = SigningKey::from_slice(&[1; 32]).unwrap();
        let public: FarmerKey = key
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap();
        let signed = |n| {
            let block = hash(n);
            let signature: Signature = key
                .sign_prehash(&vote::signing_hash(&block, &public))
                .unwrap();
            let mut record = Record {
                name: "mmx.ValidatorVote".into(),
                fields: [
                    ("hash".into(), Value::Bytes(block.to_vec())),
                    ("farmer_key".into(), Value::Bytes(public.to_vec())),
                    (
                        "farmer_sig".into(),
                        Value::Bytes(signature.to_bytes().to_vec()),
                    ),
                    ("content_hash".into(), Value::Bytes(vec![0; 32])),
                ]
                .into(),
            };
            let id = vote::content_hash(&record).unwrap();
            record
                .fields
                .insert("content_hash".into(), Value::Bytes(id.to_vec()));
            vote::verify(&record).unwrap()
        };
        let mut state = Voting::default();
        let first = signed(1);
        assert!(state.enqueue(first.clone(), 0));
        for n in 2..=4096 {
            assert!(state.enqueue(signed(n), 0));
        }
        assert_eq!(state.pending(), 4096);
        assert!(!state.enqueue(signed(4097), 30));
        assert!(state.enqueue(first, 30));
        assert_eq!(state.process(30, 10).pending, 4096);
        let expired = state.process(31, 10);
        assert_eq!(expired.pending, 0);
        assert_eq!(expired.processed.len(), 4096);
        assert!(expired
            .processed
            .iter()
            .all(|(_, d)| *d == Disposition::Expired));
        assert!(state.enqueue(signed(4097), 31));
        assert_eq!(state.pending(), 1);
    }
    #[test]
    fn native_vote_queue_counts_duplicates_best_proof_changes_and_timeout_boundaries() {
        let rows: Vec<Value> = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/votes.jsonl"
        ))
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
        let wires: Vec<_> = rows.iter().find(|r| r["kind"] == "queue_wires").unwrap()["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| Record::decode(&unhex(v)).unwrap())
            .collect();
        let keys: BTreeMap<u64, FarmerKey> = rows
            .iter()
            .filter(|r| r["kind"] == "crypto" && r["name"].as_str().unwrap().starts_with("valid"))
            .map(|r| {
                let id = r["name"].as_str().unwrap()[5..].parse().unwrap();
                let record = Record::decode(&unhex(&r["wire"])).unwrap();
                (
                    id,
                    record
                        .field("farmer_key")
                        .unwrap()
                        .bytes()
                        .unwrap()
                        .try_into()
                        .unwrap(),
                )
            })
            .collect();
        let mut count = 0;
        for row in rows.iter().filter(|r| r["kind"] == "queue") {
            let mut state = Voting::default();
            let mut staged = BTreeMap::<Hash, Fork>::new();
            let mut now = 0;
            let interval = row["interval"].as_i64().unwrap();
            for event in row["events"].as_array().unwrap() {
                let number = |name: &str| event[name].as_u64().unwrap();
                let mut report = None;
                match event["type"].as_str().unwrap() {
                    "register" => {
                        let h = hash(number("block"));
                        if !state.forks.contains_key(&h) && !staged.contains_key(&h) {
                            let f = Fork {
                                height: 1,
                                challenge: hash(number("challenge")),
                                proof_hash: hash(number("proof")),
                                validators: event["validators"]
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .map(|v| (keys[&v.as_u64().unwrap()], false))
                                    .collect(),
                                votes: 0,
                            };
                            if event["verified"] == true {
                                state.add_proof(f.challenge, f.proof_hash, 1);
                                state.forks.insert(h, f);
                            } else {
                                staged.insert(h, f);
                            }
                        }
                    }
                    "verify" => {
                        let h = hash(number("block"));
                        if let Some(f) = staged.remove(&h) {
                            state.forks.insert(h, f);
                        }
                        let f = state.forks.get(&h).unwrap();
                        state.add_proof(f.challenge, f.proof_hash, 1);
                    }
                    "proof" => state.add_proof(hash(number("challenge")), hash(number("proof")), 1),
                    "vote" => {
                        now = event["at"].as_i64().unwrap();
                        report = Some(
                            state
                                .admit(
                                    &wires[event["vote"].as_u64().unwrap() as usize],
                                    now,
                                    interval,
                                )
                                .unwrap(),
                        );
                    }
                    "advance" => now = event["at"].as_i64().unwrap(),
                    _ => panic!("unknown event"),
                }
                let report = report.unwrap_or_else(|| state.process(now, interval));
                assert_eq!(
                    report.pending,
                    event["pending"].as_u64().unwrap() as usize,
                    "{}",
                    row["name"]
                );
                let accepted: Vec<_> = report
                    .processed
                    .iter()
                    .filter(|(_, d)| *d == Disposition::Accepted)
                    .map(|(v, _)| *v.content_hash())
                    .collect();
                let expected: Vec<Hash> = event["accepted"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| unhex(v).try_into().unwrap())
                    .collect();
                assert_eq!(accepted, expected, "{}", row["name"]);
                for (h, n) in event["counts"].as_object().unwrap() {
                    let bytes = vnx::util::from_hex_string(h).unwrap();
                    assert_eq!(
                        state.count(&bytes.try_into().unwrap()),
                        n.as_u64().unwrap() as u32,
                        "{}",
                        row["name"]
                    );
                }
            }
            count += 1;
        }
        assert_eq!(count, 69);
    }
}
