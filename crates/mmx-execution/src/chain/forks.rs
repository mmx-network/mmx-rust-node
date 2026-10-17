//! Durable orphan archive, authenticated validator voting and native fork
//! updates. Root advancement, sync scheduling and archive pruning are separate.
use super::{
    choice::{self, Anchor, Candidate},
    codec, genesis, invalid, voting, ChainError, ChainResult, ChainState, ValidationMode,
};
use crate::{
    block,
    transaction::{bytes, uint, ValidationError},
};
use mmx_consensus::{
    proof::{ProofError, ProofResult},
    transitions::{self, history::HeaderSource},
    vdf, Hash, Record,
};
use mmx_db::{Store as Database, Write};
use mmx_types::{mmx, VnxType};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Default)]
pub struct Update {
    pub best: Option<Hash>,
    pub common_ancestor: Option<Hash>,
    pub applied: Vec<Hash>,
    pub invalid: BTreeSet<Hash>,
    pub waiting: BTreeSet<Hash>,
    pub votes: voting::Report,
    pub verification_time: Duration,
    pub replay_time: Duration,
}
struct Entry {
    block: Record,
    header: Arc<mmx::BlockHeader>,
    proofs: Vec<Record>,
    invalid: bool,
}

/// An owned, consistent view of archived bodies. Indexed storage shares its
/// committed MVCC prefix, so readers do not hold the node's ledger mutex.
pub struct BlockSnapshot {
    archive: Database,
}
impl BlockSnapshot {
    pub fn block(&self, hash: &Hash) -> ChainResult<Option<Record>> {
        archive_block(&self.archive, hash)
    }
}
pub struct ForkChain {
    chain: ChainState,
    archive: Database,
    voting: voting::Voting,
    // Native proof flags survive the transition out of historical sync, but
    // reset on restart/explicit rewind. Never promote a new live block here.
    sync_verified: BTreeSet<Hash>,
    verified: BTreeMap<Hash, Vec<vdf::VerifiedPoint>>,
    verified_work: BTreeMap<Hash, vdf::VerifiedPoint>,
    evidence: BTreeMap<Hash, transitions::VerifiedHeader>,
    connected: BTreeSet<Hash>,
    tip_limit: Option<u32>,
    headers: super::cache::Headers,
}
impl ForkChain {
    pub fn memory(params: mmx::ChainParams) -> Self {
        Self {
            chain: ChainState::memory(params),
            archive: Database::memory(),
            voting: voting::Voting::default(),
            sync_verified: BTreeSet::new(),
            verified: BTreeMap::new(),
            verified_work: BTreeMap::new(),
            evidence: BTreeMap::new(),
            connected: BTreeSet::new(),
            tip_limit: None,
            headers: Default::default(),
        }
    }
    pub fn open(path: impl AsRef<Path>, params: mmx::ChainParams) -> ChainResult<Self> {
        Self::with_storage(
            ChainState::open(path.as_ref().join("selected"), params)?,
            Database::open(path.as_ref().join("archive"))?,
        )
    }
    pub fn open_indexed(path: impl AsRef<Path>, params: mmx::ChainParams) -> ChainResult<Self> {
        Self::with_storage(
            ChainState::open_indexed(path.as_ref().join("selected"), params)?,
            Database::open_indexed(path.as_ref().join("archive"))?,
        )
    }
    fn with_storage(selected: ChainState, archive: Database) -> ChainResult<Self> {
        let chain = Self {
            chain: selected,
            archive,
            voting: voting::Voting::default(),
            sync_verified: BTreeSet::new(),
            verified: BTreeMap::new(),
            verified_work: BTreeMap::new(),
            evidence: BTreeMap::new(),
            connected: BTreeSet::new(),
            tip_limit: None,
            headers: Default::default(),
        };
        if chain.archive.version() != 0
            && chain.archive.get("fork_meta", b"params", None)?
                != Some(chain.chain.params.encode(&mmx_types::vnx_registry()?)?)
        {
            return Err(invalid("fork archive parameter mismatch"));
        }
        if let Some(tip) = chain.chain.tip()? {
            if chain.block(&tip.hash.0)?.is_none() {
                return Err(invalid("selected tip missing from fork archive"));
            }
            if chain.archive.get("fork_meta", b"genesis", None)?
                != chain.chain.hash_at(0)?.map(|h| h.to_vec())
            {
                return Err(invalid("fork archive genesis mismatch"));
            }
        }
        Ok(chain)
    }
    pub fn selected(&self) -> &ChainState {
        &self.chain
    }
    /// Keep the selected block at `height` and rewind every later ledger/index
    /// version. Orphans and original genesis stay archived at every height.
    pub fn rollback_to(&mut self, height: u32) -> ChainResult<()> {
        let tip = self
            .chain
            .tip()?
            .ok_or_else(|| invalid("chain is not initialized"))?;
        if height > tip.height {
            return Err(invalid("rollback height exceeds selected tip"));
        }
        if height < tip.height {
            self.chain.revert(height + 1)?;
            self.voting.reset();
            self.sync_verified.clear();
            self.verified.clear();
            self.verified_work.clear();
            self.evidence.clear();
            self.connected.clear();
        }
        Ok(())
    }
    pub fn vote_count(&self, block: &Hash) -> u32 {
        self.voting.count(block)
    }
    /// Bound the active download view without pruning any archived branch.
    pub fn set_tip_limit(&mut self, height: Option<u32>) {
        self.tip_limit = height;
    }
    pub fn verified_vdf_inputs(&self, root_height: u32) -> ChainResult<Vec<Hash>> {
        self.connected
            .iter()
            .filter_map(|hash| {
                let header = cached_header(&self.archive, &self.headers, hash);
                match header {
                    Ok(Some(h))
                        if h.height > root_height
                            && self.tip_limit.is_none_or(|limit| h.height <= limit) =>
                    {
                        Some(Ok(h.vdf_output.0))
                    }
                    Ok(_) => None,
                    Err(e) => Some(Err(e)),
                }
            })
            .collect()
    }
    pub fn set_vote_history_limit(&mut self, max_history: u32) {
        self.voting.set_history_limit(max_history);
    }
    pub fn admit_vote(&mut self, vote: &Record, now_ms: i64) -> ChainResult<voting::Report> {
        Ok(self
            .voting
            .admit(vote, now_ms, self.chain.params().block_interval_ms)?)
    }
    pub fn process_votes(&mut self, now_ms: i64) -> voting::Report {
        self.voting
            .process(now_ms, self.chain.params().block_interval_ms)
    }
    /// Additional work-verified farmer proofs can change future vote eligibility;
    /// already counted votes remain counted. Response admission belongs to Node.
    pub fn observe_proof(&mut self, proof: &mmx_consensus::proof::VerifiedProof) {
        self.voting.observe_proof(proof);
    }
    /// Reuse CPU evidence while rebinding shape/authentication/history to this
    /// chain's parameters. Callers cannot manufacture VerifiedPoint from wire.
    pub fn cache_verified_work(&mut self, points: &[vdf::VerifiedPoint]) -> ChainResult<()> {
        let mut cache = BTreeMap::new();
        for point in points {
            let record = Record::from_generated(point.proof())?;
            vdf::validate(&record, self.chain.params(), self)?;
            cache.insert(bytes::<32>(record.field("content_hash")?)?, point.clone());
        }
        self.verified_work = cache;
        Ok(())
    }
    pub fn block(&self, hash: &Hash) -> ChainResult<Option<Record>> {
        archive_block(&self.archive, hash)
    }
    pub fn block_snapshot(&self) -> ChainResult<BlockSnapshot> {
        Ok(BlockSnapshot {
            archive: self.archive.fork(self.archive.version())?,
        })
    }

    pub fn initialize(&mut self, assets: &genesis::Assets) -> ChainResult<block::Receipt> {
        if self.chain.tip()?.is_some() {
            return Err(invalid("chain already initialized"));
        }
        let block = genesis::build(self.chain.params(), assets)?;
        let hash = bytes::<32>(block.field("hash")?)?;
        if self
            .archive
            .get("fork_meta", b"genesis", None)?
            .is_some_and(|old| old != hash)
        {
            return Err(invalid("fork archive genesis mismatch"));
        }
        self.retain(&block, &[])?; // write-ahead: selected state never references a missing archive
        self.chain.initialize(assets)
    }
    /// Prevalidate and retain an orphan even if its parent or work is missing.
    /// Repeated blocks preserve the first record and merge work in arrival order.
    /// This does not change the selected ledger. Proofs are checked again before
    /// use, including after restart; stored bytes cannot forge VerifiedPoint.
    pub fn admit(&mut self, block: &Record, proofs: &[Record]) -> ChainResult<()> {
        if self.chain.tip()?.is_none() {
            return Err(invalid("chain is not initialized"));
        }
        if uint(block, "height")? == 0 {
            return Err(invalid("cannot admit another genesis"));
        }
        block::prevalidate(block, self.chain.params())?;
        for proof in proofs {
            if !vdf::is_valid(proof)? {
                return Err(invalid("invalid archived VDF proof"));
            }
            vdf::authenticate(proof)?;
        }
        self.retain(block, proofs)
    }
    /// Atomically retain a historical download window with no attached PoTs.
    /// First-record and height/arrival ordering match sequential admission.
    pub fn admit_blocks(&mut self, blocks: &[Record]) -> ChainResult<()> {
        if self.chain.tip()?.is_none() {
            return Err(invalid("chain is not initialized"));
        }
        let mut writes = Vec::new();
        let mut headers = Vec::new();
        let mut seen = BTreeSet::new();
        let mut arrival = self.archive.version();
        for block in blocks {
            let height = uint(block, "height")? as u32;
            if height == 0 {
                return Err(invalid("cannot admit another genesis"));
            }
            block::prevalidate(block, self.chain.params())?;
            let hash = bytes::<32>(block.field("hash")?)?;
            if !seen.insert(hash) || self.archive.get("fork_blocks", &hash, None)?.is_some() {
                continue;
            }
            let key = [height.to_be_bytes(), arrival.to_be_bytes()].concat();
            arrival = arrival
                .checked_add(1)
                .ok_or_else(|| invalid("fork archive version limit"))?;
            headers.push(header(block)?);
            writes.extend([
                Write::put("fork_order", key, hash),
                Write::put("fork_work", hash, pack(&[])?),
                Write::put("fork_blocks", hash, codec::encode(block)?),
            ]);
        }
        if !writes.is_empty() {
            self.archive.commit_batch(arrival, writes)?;
            for header in headers {
                self.headers.insert(header);
            }
        }
        Ok(())
    }
    fn retain(&mut self, block: &Record, proofs: &[Record]) -> ChainResult<()> {
        let hash = bytes::<32>(block.field("hash")?)?;
        let exists = self.archive.get("fork_blocks", &hash, None)?.is_some();
        let mut work = self
            .work(&hash)?
            .into_iter()
            .map(|r| r.encode())
            .collect::<mmx_consensus::Result<Vec<_>>>()?;
        let old_len = work.len();
        for proof in proofs {
            let wire = proof.encode()?;
            if !work.contains(&wire) {
                work.push(wire)
            }
        }
        if exists && old_len == work.len() {
            return Ok(());
        }
        let mut writes = vec![Write::put("fork_work", hash, pack(&work)?)];
        let mut retained_header = None;
        if !exists {
            let height = uint(block, "height")? as u32;
            if height == 0 {
                writes.push(Write::put("fork_meta", b"genesis", hash));
            }
            let key = [height.to_be_bytes(), self.archive.version().to_be_bytes()].concat();
            writes.push(Write::put("fork_order", key, hash));
            writes.push(Write::put("fork_blocks", hash, codec::encode(block)?));
            retained_header = Some(header(block)?);
        }
        self.commit_archive(writes)?;
        if let Some(header) = retained_header {
            self.headers.insert(header);
        }
        Ok(())
    }
    fn commit_archive(&mut self, mut writes: Vec<Write>) -> ChainResult<()> {
        if self.archive.version() == 0 {
            writes.push(Write::put(
                "fork_meta",
                b"params",
                self.chain.params.encode(&mmx_types::vnx_registry()?)?,
            ));
        }
        let next = self
            .archive
            .version()
            .checked_add(1)
            .ok_or_else(|| invalid("fork archive version limit"))?;
        self.archive.commit_batch(next, writes)?;
        Ok(())
    }
    fn work(&self, hash: &Hash) -> ChainResult<Vec<Record>> {
        let Some(data) = self.archive.get("fork_work", hash, None)? else {
            return Ok(vec![]);
        };
        unpack(&data)
    }
    fn entries(&self, root_height: u32) -> ChainResult<Vec<Entry>> {
        let end = self
            .tip_limit
            .and_then(|h| h.checked_add(1))
            .map(|h| [h.to_be_bytes(), 0u32.to_be_bytes()].concat());
        self.archive
            .scan_range(
                "fork_order",
                &[
                    root_height.saturating_add(1).to_be_bytes(),
                    0u32.to_be_bytes(),
                ]
                .concat(),
                end.as_deref(),
                None,
            )?
            .into_iter()
            .map(|row| {
                let hash: Hash = row
                    .value
                    .try_into()
                    .map_err(|_| invalid("fork order hash length"))?;
                let block = self
                    .block(&hash)?
                    .ok_or_else(|| invalid("missing ordered fork"))?;
                let header = cached_header_shared(&self.archive, &self.headers, &hash)?
                    .ok_or_else(|| invalid("missing ordered header"))?;
                if row.key.len() != 8 || row.key[..4] != header.height.to_be_bytes() {
                    return Err(invalid("fork order height mismatch"));
                }
                Ok(Entry {
                    block,
                    header,
                    proofs: self.work(&hash)?,
                    invalid: self.archive.get("fork_invalid", &hash, None)?.is_some(),
                })
            })
            .collect()
    }
    /// Select and apply a branch above an explicitly supplied selected root.
    /// Root advancement and the lowest backwards-sync height are supplied by
    /// the caller, matching native policy
    /// inputs without inventing a finality/sync policy for this library.
    pub fn update(&mut self, root: Hash, sync_pending: Option<u32>) -> ChainResult<Update> {
        self.update_at(root, sync_pending, vnx::util::get_wall_time_millis())
    }
    pub fn update_at(
        &mut self,
        root: Hash,
        sync_pending: Option<u32>,
        now_ms: i64,
    ) -> ChainResult<Update> {
        self.update_mode(root, sync_pending, now_ms, ValidationMode::Live)
    }
    /// Historical sync matches native `!is_synced`: only VDF linkage is omitted.
    /// Node must apply the native proof-density root policy before committing.
    pub fn update_sync_at(
        &mut self,
        root: Hash,
        sync_pending: Option<u32>,
        now_ms: i64,
    ) -> ChainResult<Update> {
        self.update_mode(root, sync_pending, now_ms, ValidationMode::HistoricalSync)
    }
    fn update_mode(
        &mut self,
        root: Hash,
        sync_pending: Option<u32>,
        now_ms: i64,
        mode: ValidationMode,
    ) -> ChainResult<Update> {
        let root_header = self
            .chain
            .header(&root)?
            .ok_or_else(|| invalid("missing selected root"))?;
        if self.chain.hash_at(root_header.height)? != Some(root) {
            return Err(invalid("root is not selected"));
        }
        self.voting.root(root_header.height, root_header.vdf_height);
        let entries = self.entries(root_header.height)?;
        let headers: BTreeMap<_, _> = entries
            .iter()
            .map(|e| (e.header.hash.0, e.header.clone()))
            .collect();
        let history = History {
            headers: &headers,
            archive: &self.archive,
            cache: &self.headers,
        };
        self.verified.retain(|hash, _| headers.contains_key(hash));
        self.evidence.retain(|hash, _| headers.contains_key(hash));
        self.sync_verified.retain(|hash| headers.contains_key(hash));
        let mut out = Update::default();
        let mut candidates = Vec::new();
        let mut points = BTreeMap::new();
        let verification_started = Instant::now();
        let mut jobs = Vec::new();
        let mut pending_work = BTreeMap::new();
        let mut bad_context = BTreeSet::new();
        // Resolve history and VDF eligibility on the owner thread. The owned
        // contexts then allow independent CPU space checks, as in native
        // Node::verify_block_proofs(), without sharing any database connection.
        for entry in &entries {
            let h = &entry.header;
            if h.height <= root_header.height || self.verified.contains_key(&h.hash.0) {
                continue;
            }
            let Some(parent) = history.get_header(&h.prev.0)? else {
                continue;
            };
            let work: Vec<_> = if mode == ValidationMode::HistoricalSync {
                vec![]
            } else {
                entry
                    .proofs
                    .iter()
                    .filter_map(|p| {
                        self.verified_work
                            .get(&bytes::<32>(p.field("content_hash").ok()?).ok()?)
                            .cloned()
                            .or_else(|| vdf::verify(p, self.chain.params(), &history).ok())
                    })
                    .collect()
            };
            if mode == ValidationMode::HistoricalSync
                || self.sync_verified.contains(&h.hash.0)
                || vdf::link_block(&entry.block, self.chain.params(), &history, &work).is_ok()
            {
                if let Ok(context) =
                    transitions::ProofContext::resolve(&entry.block, self.chain.params(), &history)
                {
                    if context.parent != parent {
                        bad_context.insert(h.hash.0);
                    } else {
                        jobs.push(super::work::Header {
                            hash: h.hash.0,
                            record: &entry.block,
                            context,
                        });
                        pending_work.insert(h.hash.0, work);
                    }
                }
            }
        }
        let mut checked = super::work::verify_headers(&jobs, self.chain.params())?;
        for entry in &entries {
            let h = &entry.header;
            if h.height <= root_header.height {
                continue;
            }
            let mut verified = false;
            let mut bad = entry.invalid || bad_context.contains(&h.hash.0);
            if let Some(work) = self.verified.get(&h.hash.0) {
                verified = true;
                points.insert(h.hash.0, work.clone());
            } else if let Some(result) = checked.remove(&h.hash.0) {
                match result {
                    Err(error @ ProofError::Input(mmx_consensus::Error::Unsupported(_))) => {
                        return Err(error.into());
                    }
                    Err(_) => bad = true,
                    Ok(evidence) => {
                        verified = true;
                        self.evidence.insert(h.hash.0, evidence);
                        if mode == ValidationMode::HistoricalSync {
                            self.sync_verified.insert(h.hash.0);
                        }
                        points.insert(
                            h.hash.0,
                            pending_work
                                .remove(&h.hash.0)
                                .ok_or_else(|| invalid("missing prepared VDF work"))?,
                        );
                    }
                }
            }
            if verified {
                self.voting
                    .register(&entry.block, h, self.chain.params(), &history)?;
                self.verified.insert(h.hash.0, points[&h.hash.0].clone());
            }
            if bad {
                out.invalid.insert(h.hash.0);
            } else if !verified {
                out.waiting.insert(h.hash.0);
            }
            candidates.push(Candidate {
                hash: h.hash.0,
                prev: h.prev.0,
                height: h.height,
                total_weight: u128::from_le_bytes(h.total_weight.0),
                votes: self.voting.count(&h.hash.0),
                proof_verified: verified,
                invalid: bad,
                anchor: history.get_header(&h.prev.0)?.map(|p| Anchor {
                    hash: p.hash.0,
                    total_weight: u128::from_le_bytes(p.total_weight.0),
                }),
            });
        }
        out.votes = self.process_votes(now_ms);
        out.verification_time = verification_started.elapsed();
        for candidate in &mut candidates {
            candidate.votes = self.voting.count(&candidate.hash);
        }
        self.persist_invalid(&out.invalid)?;
        loop {
            let selection = choice::select(root_header.height, root, sync_pending, &candidates);
            self.connected = selection
                .states
                .iter()
                .filter(|(_, s)| s.all_proofs_verified)
                .map(|(hash, _)| *hash)
                .collect();
            out.best = selection.best;
            for (hash, state) in selection.states {
                if state.invalid {
                    out.invalid.insert(hash);
                    out.waiting.remove(&hash);
                }
            }
            let Some(best) = out.best else { return Ok(out) };
            if self.chain.tip()?.is_some_and(|tip| tip.hash.0 == best) {
                return Ok(out);
            }
            let (common, line) = self.line(best)?;
            let archive = &self.archive;
            let sync_verified = &self.sync_verified;
            let evidence = &self.evidence;
            let branch = line.iter().map(|hash| {
                let block =
                    archive_block(archive, hash)?.ok_or_else(|| invalid("missing replay block"))?;
                // Native fork_to re-verifies space proofs and bodies below a
                // replacement root without requiring historical PoTs. New live
                // blocks above the root still require verified VDF evidence.
                let validation = if uint(&block, "height")? <= u64::from(root_header.height)
                    || sync_verified.contains(hash)
                {
                    ValidationMode::HistoricalSync
                } else {
                    mode
                };
                let work = points.get(hash).cloned().unwrap_or_default();
                let evidence = evidence.get(hash).cloned();
                Ok((block, work, validation, evidence))
            });
            let replay_started = Instant::now();
            let replay = self
                .chain
                .reorganize_stream(common.height + 1, branch, false);
            out.replay_time += replay_started.elapsed();
            match replay {
                Ok(_) => {
                    out.common_ancestor = Some(common.hash.0);
                    out.applied = line;
                    return Ok(out);
                }
                Err(ChainError::Block(error)) if consensus_rejection(&error.error) => {
                    out.invalid.insert(best);
                    self.persist_invalid(&BTreeSet::from([best]))?;
                    candidates
                        .iter_mut()
                        .find(|c| c.hash == best)
                        .expect("selected candidate")
                        .invalid = true;
                }
                Err(error) => return Err(error),
            }
        }
    }
    fn persist_invalid(&mut self, hashes: &BTreeSet<Hash>) -> ChainResult<()> {
        let mut writes = Vec::new();
        for hash in hashes {
            if self.archive.get("fork_invalid", hash, None)?.is_none() {
                writes.push(Write::put("fork_invalid", *hash, [1]));
            }
        }
        if !writes.is_empty() {
            self.commit_archive(writes)?
        }
        Ok(())
    }
    fn line(&self, hash: Hash) -> ChainResult<(mmx::BlockHeader, Vec<Hash>)> {
        let mut current = hash;
        let mut line = Vec::new();
        let mut expected = None;
        loop {
            let h = cached_header(&self.archive, &self.headers, &current)?
                .ok_or_else(|| invalid("missing fork ancestor"))?;
            if expected.is_some_and(|height| height != h.height) {
                return Err(invalid("fork ancestor height mismatch"));
            }
            if self.chain.hash_at(h.height)? == Some(current) {
                line.reverse();
                return Ok((h, line));
            }
            expected = Some(
                h.height
                    .checked_sub(1)
                    .ok_or_else(|| invalid("foreign fork genesis"))?,
            );
            line.push(current);
            current = h.prev.0;
        }
    }
}
fn consensus_rejection(error: &ValidationError) -> bool {
    !error.is_unsupported()
        && !matches!(
            error,
            ValidationError::Storage(_)
                | ValidationError::Consensus(mmx_consensus::Error::Unsupported(_))
                | ValidationError::Execution(mmx_vm::Error::Storage(_))
        )
}
struct History<'a> {
    headers: &'a BTreeMap<Hash, Arc<mmx::BlockHeader>>,
    archive: &'a Database,
    cache: &'a super::cache::Headers,
}
impl HeaderSource for History<'_> {
    fn get_header(&self, hash: &Hash) -> ProofResult<Option<mmx::BlockHeader>> {
        Ok(self.get_shared_header(hash)?.map(|h| h.as_ref().clone()))
    }
    fn get_shared_header(&self, hash: &Hash) -> ProofResult<Option<Arc<mmx::BlockHeader>>> {
        if let Some(header) = self.headers.get(hash) {
            return Ok(Some(header.clone()));
        }
        cached_header_shared(self.archive, self.cache, hash)
            .map_err(|e| ProofError::Invalid(e.to_string()))
    }
}
impl HeaderSource for ForkChain {
    fn get_header(&self, hash: &Hash) -> ProofResult<Option<mmx::BlockHeader>> {
        cached_header(&self.archive, &self.headers, hash)
            .map_err(|e| ProofError::Invalid(e.to_string()))
    }
    fn get_shared_header(&self, hash: &Hash) -> ProofResult<Option<Arc<mmx::BlockHeader>>> {
        cached_header_shared(&self.archive, &self.headers, hash)
            .map_err(|e| ProofError::Invalid(e.to_string()))
    }
}
fn archive_block(archive: &Database, hash: &Hash) -> ChainResult<Option<Record>> {
    archive
        .get("fork_blocks", hash, None)?
        .map(|b| {
            let block = codec::decode(&b)?;
            if block.name != "mmx.Block" || bytes::<32>(block.field("hash")?)? != *hash {
                return Err(invalid("fork archive key mismatch"));
            }
            Ok(block)
        })
        .transpose()
}

fn cached_header(
    archive: &Database,
    cache: &super::cache::Headers,
    hash: &Hash,
) -> ChainResult<Option<mmx::BlockHeader>> {
    Ok(cached_header_shared(archive, cache, hash)?.map(|h| h.as_ref().clone()))
}
fn cached_header_shared(
    archive: &Database,
    cache: &super::cache::Headers,
    hash: &Hash,
) -> ChainResult<Option<Arc<mmx::BlockHeader>>> {
    if let Some(header) = cache.get_shared(hash) {
        return Ok(Some(header));
    }
    let header = archive_header(archive, hash)?;
    if let Some(header) = &header {
        if header.hash.0 != *hash {
            return Err(invalid("fork header key mismatch"));
        }
    }
    Ok(header.map(|h| cache.insert(h)))
}
fn archive_header(archive: &Database, hash: &Hash) -> ChainResult<Option<mmx::BlockHeader>> {
    if let Some(wire) = archive.get("fork_headers", hash, None)? {
        return Ok(Some(mmx::BlockHeader::decode(&wire)?));
    }
    archive
        .get("fork_blocks", hash, None)?
        .map(|wire| header(&codec::decode(&wire)?))
        .transpose()
}
fn header(block: &Record) -> ChainResult<mmx::BlockHeader> {
    super::block_header(block)
}
fn pack(values: &[Vec<u8>]) -> ChainResult<Vec<u8>> {
    let mut out = u32::try_from(values.len())
        .map_err(|_| invalid("work count limit"))?
        .to_le_bytes()
        .to_vec();
    for value in values {
        out.extend(
            u32::try_from(value.len())
                .map_err(|_| invalid("work size limit"))?
                .to_le_bytes(),
        );
        out.extend(value);
    }
    Ok(out)
}
fn unpack(mut input: &[u8]) -> ChainResult<Vec<Record>> {
    fn size(input: &mut &[u8]) -> ChainResult<usize> {
        let bytes = input
            .get(..4)
            .ok_or_else(|| invalid("truncated work count"))?;
        let n = u32::from_le_bytes(bytes.try_into().expect("four bytes")) as usize;
        *input = &input[4..];
        Ok(n)
    }
    let count = size(&mut input)?;
    if count > input.len() / 4 {
        return Err(invalid("work count limit"));
    }
    let mut out = Vec::new();
    for _ in 0..count {
        let n = size(&mut input)?;
        let data = input
            .get(..n)
            .ok_or_else(|| invalid("truncated archived work"))?;
        out.push(Record::decode(data)?);
        input = &input[n..];
    }
    if !input.is_empty() {
        return Err(invalid("trailing archived work"));
    }
    Ok(out)
}
