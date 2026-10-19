//! Native Router-style height jobs: query several peers, fetch each content
//! commitment once, then let consensus choose among all valid candidates.
use super::{Progress, BATCH_BLOCKS, BATCH_BYTES};
use crate::{
    error,
    node::{hash, number},
    peer::{BlockHash, Client, MAX_BODY_REQUESTS},
    Node, Result,
};
use mmx_consensus::{Hash, Record, Value};
use rand::seq::SliceRandom;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

pub const MIN_PEERS: usize = 3;
pub const MAX_PEERS: usize = 8;
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);
// Preserve the previous eight-peer worst-case bound while allowing smaller
// authenticated pools to fill a complete body-request pipeline as well.
const MAX_GROUP_CONTENTS: usize = 32;
type Missing = BTreeSet<(u32, Hash)>;
type QualifiedPeer = (Hash, u32, Client, Vec<String>);
mod connect;
mod metadata;
mod prefetch;

trait WindowSource {
    fn refresh(&mut self) -> Result<u32>;
    fn window(&mut self, start: u32, count: u32) -> Result<Window>;
    fn prefetch(&mut self, start: u32, count: u32, local: &mut Node) -> Result<()>;
}

/// The transport surface used by the coordinator, also implemented by test
/// peers so adversarial replies exercise the production scheduler.
pub trait PeerSource: Send + 'static {
    fn hashes(&mut self, start: u32, count: u32) -> Result<Vec<Option<BlockHash>>>;
    fn fetch(&mut self, hashes: &[Hash]) -> Result<Vec<Option<Record>>>;
    fn synced_height(&mut self) -> Result<Option<u32>>;
    fn backpressure_retries(&self) -> u64 {
        0
    }
    fn poll(&mut self) -> Result<()> {
        Ok(())
    }
}
impl PeerSource for Client {
    fn hashes(&mut self, start: u32, count: u32) -> Result<Vec<Option<BlockHash>>> {
        self.hash_pairs(start, count)
    }
    fn fetch(&mut self, hashes: &[Hash]) -> Result<Vec<Option<Record>>> {
        self.blocks_by_hash(hashes)
    }
    fn synced_height(&mut self) -> Result<Option<u32>> {
        Client::synced_height(self)
    }
    fn backpressure_retries(&self) -> u64 {
        Client::backpressure_retries(self)
    }
    fn poll(&mut self) -> Result<()> {
        Client::poll(self)
    }
}
struct Peer<S> {
    id: Hash,
    source: Option<S>,
    inflight: Option<metadata::Job<S>>,
    active: bool,
    qualified: bool,
    needs_status: bool,
    height: u32,
    latency: Duration,
    backpressure: u64,
}
pub struct PeerPool<S> {
    peers: Vec<Peer<S>>,
    timeout: Duration,
    // Complete content responses (including invalid ones) are never fetched
    // twice within a height job, even when the driver repeats a tip probe.
    completed: BTreeMap<(u32, Hash), Option<Record>>,
    refreshed: Instant,
    body_requests: u64,
    body_retries: u64,
    retired_backpressure: u64,
    metadata: metadata::Queue,
    query: u64,
}
#[derive(Default)]
pub struct Window {
    pub start: u32,
    pub target: u32,
    pub heights: Vec<Vec<Record>>,
    pub hash_time: Duration,
    pub body_time: Duration,
    pub body_requests: u64,
    pub body_retries: u64,
    pub archive_time: Duration,
    pub backpressure_retries: u64,
}
struct Fetch {
    height: u32,
    heights: BTreeSet<u32>,
    hash: BlockHash,
    providers: Vec<(usize, Hash)>,
    tried: BTreeSet<usize>,
    retry_at: Option<Instant>,
}
struct Published {
    missing: Missing,
    start: u32,
    usable: Vec<bool>,
}
struct ReadContext {
    blocks: mmx_execution::chain::forks::BlockSnapshot,
    params: mmx_types::mmx::ChainParams,
}
impl ReadContext {
    fn new(local: &Node) -> Result<Self> {
        Ok(Self {
            blocks: local.chain.block_snapshot()?,
            params: local.chain.selected().params().clone(),
        })
    }
}
impl Published {
    fn empty(&self) -> Option<u32> {
        self.usable
            .iter()
            .position(|&usable| !usable)
            .map(|offset| self.start + offset as u32)
    }
}
impl<S: PeerSource> PeerPool<S> {
    pub fn new(mut peers: Vec<(Hash, S)>) -> Self {
        peers.shuffle(&mut rand::thread_rng());
        let mut ids = BTreeSet::new();
        let peers = peers
            .into_iter()
            .filter(|(id, _)| ids.insert(*id))
            .take(MAX_PEERS)
            .map(|(id, source)| Peer {
                id,
                source: Some(source),
                inflight: None,
                active: true,
                qualified: false,
                needs_status: true,
                height: 0,
                latency: Duration::MAX,
                backpressure: 0,
            })
            .collect();
        Self {
            peers,
            timeout: FETCH_TIMEOUT,
            completed: BTreeMap::new(),
            refreshed: Instant::now() - Duration::from_secs(10),
            body_requests: 0,
            body_retries: 0,
            retired_backpressure: 0,
            metadata: metadata::Queue::new(),
            query: 0,
        }
    }
    pub fn len(&self) -> usize {
        self.peers.iter().filter(|p| p.active).count()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn ids(&self) -> Vec<Hash> {
        self.peers
            .iter()
            .filter(|p| p.active)
            .map(|p| p.id)
            .collect()
    }
    pub fn remove_failed(&mut self) {
        self.retired_backpressure += self
            .peers
            .iter()
            .filter(|p| !p.active)
            .map(|p| p.backpressure)
            .sum::<u64>();
        self.peers.retain(|p| p.active);
    }
    fn backpressure_retries(&self) -> u64 {
        self.retired_backpressure + self.peers.iter().map(|p| p.backpressure).sum::<u64>()
    }
    pub fn add(&mut self, id: Hash, source: S) -> bool {
        if self.peers.len() >= MAX_PEERS || self.peers.iter().any(|p| p.id == id) {
            return false;
        }
        self.peers.push(Peer {
            id,
            source: Some(source),
            inflight: None,
            active: true,
            qualified: false,
            needs_status: true,
            height: 0,
            latency: Duration::MAX,
            backpressure: 0,
        });
        self.refreshed = Instant::now() - Duration::from_secs(10);
        true
    }
    pub fn poll(&mut self) -> Result<()> {
        if !self.peers.iter().any(|p| p.active && p.source.is_some()) {
            self.poll_metadata()?;
        }
        thread::scope(|scope| {
            let jobs: Vec<_> = self
                .peers
                .iter_mut()
                .filter(|p| p.active && p.source.is_some())
                .map(|p| {
                    scope.spawn(move || {
                        if p.source.as_mut().unwrap().poll().is_err() {
                            p.active = false;
                        }
                    })
                })
                .collect();
            for job in jobs {
                job.join().map_err(|_| error("peer worker panicked"))?;
            }
            Ok(())
        })
    }
    pub fn window(
        &mut self,
        start: u32,
        count: u32,
        node: &Arc<Mutex<Node>>,
        stop: &AtomicBool,
    ) -> Result<Window> {
        if count == 0 || count > BATCH_BLOCKS || start.checked_add(count).is_none() {
            return Err(error("invalid coordinated download window"));
        }
        let context = {
            let mut local = node.lock().map_err(|_| error("node lock poisoned"))?;
            let context = ReadContext::new(&local)?;
            local.sync_pending.extend(start..start + count);
            context
        };
        self.prepared_window(start, count, context, node, stop)
    }
    fn prepared_window(
        &mut self,
        start: u32,
        count: u32,
        context: ReadContext,
        node: &Arc<Mutex<Node>>,
        stop: &AtomicBool,
    ) -> Result<Window> {
        let initial_requests = self.body_requests;
        let initial_retries = self.body_retries;
        let initial_backpressure = self.backpressure_retries();
        let mut result = if stop.load(Ordering::Relaxed) {
            Err(error("sync cancelled"))
        } else {
            self.refresh().and_then(|target| {
                self.window_inner(start, count, &context, stop)
                    .map(|mut window| {
                        window.target = target;
                        window.body_requests = self.body_requests - initial_requests;
                        window.body_retries = self.body_retries - initial_retries;
                        window.backpressure_retries =
                            self.backpressure_retries() - initial_backpressure;
                        window
                    })
            })
        };
        // Release the archive reader before the durable writer publishes this
        // window, leaving a reader gap in which its WAL can be recycled.
        drop(context);
        let mut local = node.lock().map_err(|_| error("node lock poisoned"))?;
        // Retain completed bodies even if the consumer is cancelled before it
        // receives this window. The active view still ends at sync_pos, so this
        // write-ahead archive cannot select prefetched future blocks.
        let params = local.chain.selected().params();
        let ready: Vec<_> = self.completed.range((start, [0; 32])..(start + count, [0; 32]))
            .filter_map(|(_, block)| block.as_ref())
            .filter(|block| number(block, "height").is_ok_and(|height| height > 0))
            .filter(|block| matches!(block.field("time_stamp"), Ok(Value::Int(stamp))
                if mmx_execution::chain::sync::allows_timestamp(*stamp, vnx::util::get_wall_time_millis(), params.block_interval_ms, 100)))
            .cloned().collect();
        let archive_started = Instant::now();
        let retained = local.chain.admit_blocks(&ready);
        if let Ok(window) = &mut result {
            window.archive_time = archive_started.elapsed();
        }
        for height in start..start + count {
            local.sync_pending.remove(&height);
        }
        local.sync_peers = self.len();
        local.sync_body_requests = self.body_requests;
        local.sync_body_retries = self.body_retries;
        local.sync_backpressure_retries = self.backpressure_retries();
        retained?;
        result
    }
    fn window_inner(
        &mut self,
        start: u32,
        count: u32,
        context: &ReadContext,
        stop: &AtomicBool,
    ) -> Result<Window> {
        if stop.load(Ordering::Relaxed) {
            return Err(error("sync cancelled"));
        }
        // An exhausted provider set is not a proof that the content cannot be
        // supplied later by a newly discovered peer. Its leases have expired.
        self.completed.retain(|_, block| block.is_some());
        let hash_started = Instant::now();
        let observations = self.observations(start, count, stop);
        let hash_time = hash_started.elapsed();
        let observations = observations?;
        let mut jobs: BTreeMap<(u32, Hash), Fetch> = BTreeMap::new();
        for (peer, hashes) in observations {
            for (offset, candidate) in hashes.into_iter().enumerate() {
                if let Some(hash) = candidate {
                    let height = start + offset as u32;
                    jobs.entry((height, hash.content))
                        .or_insert_with(|| Fetch {
                            height,
                            heights: BTreeSet::new(),
                            hash,
                            providers: vec![],
                            tried: BTreeSet::new(),
                            retry_at: None,
                        })
                        .providers
                        .push((peer, hash.block));
                }
            }
        }
        // A commitment includes its actual height. A peer replaying the same
        // content at another height must not cause another body download or
        // poison the genuine height claimed by an honest peer.
        let mut groups = BTreeMap::<Hash, (BTreeSet<u32>, Vec<(usize, Hash)>)>::new();
        for job in jobs.values() {
            let group = groups.entry(job.hash.content).or_default();
            group.0.insert(job.height);
            group.1.extend(&job.providers);
        }
        for job in jobs.values_mut() {
            job.heights = groups[&job.hash.content].0.clone();
            job.providers = groups[&job.hash.content].1.clone();
        }
        // Reuse already archived content on restart, rewind and tip probes.
        // A matching chain ID alone is insufficient: legacy content can differ.
        {
            for (key, job) in &jobs {
                if !self.completed.contains_key(key) {
                    let cached = self.completed.iter().find_map(|((_, content), block)| {
                        (*content == job.hash.content)
                            .then(|| block.clone())
                            .flatten()
                    });
                    if let Some(block) = cached {
                        complete(&mut self.completed, job, Some(block));
                        continue;
                    }
                    for (_, id) in &job.providers {
                        if let Some(block) = context.blocks.block(id)? {
                            if hash(&block, "content_hash")? == job.hash.content {
                                complete(&mut self.completed, job, Some(block));
                                break;
                            }
                        }
                    }
                }
            }
        }
        let params = &context.params;
        let mut output = Window {
            start,
            target: 0,
            heights: Vec::new(),
            hash_time,
            body_time: Duration::ZERO,
            body_requests: 0,
            body_retries: 0,
            archive_time: Duration::ZERO,
            backpressure_retries: 0,
        };
        let body_started = Instant::now();
        let mut unsupported = BTreeMap::new();
        let mut bytes = 0usize;
        // Bound each group by unique content count, including competing
        // candidates. The global bound does not shrink with the peer count;
        // common content can fill one provider's complete bounded pipeline.
        let mut first = 0;
        while first < count && bytes < BATCH_BYTES {
            let mut end = start + first + 1;
            let mut contents = jobs.range((start + first, [0; 32])..(end, [0; 32])).count();
            while end < start + count {
                let additional = jobs.range((end, [0; 32])..=(end, [255; 32])).count();
                if contents + additional > MAX_GROUP_CONTENTS {
                    break;
                }
                contents += additional;
                end += 1;
            }
            let mut pending: Vec<_> = jobs
                .range((start + first, [0; 32])..(end, [0; 32]))
                .filter(|(key, job)| {
                    job.heights.first() == Some(&key.0) && !self.completed.contains_key(key)
                })
                .map(|(key, _)| *key)
                .collect();
            while !pending.is_empty() {
                if stop.load(Ordering::Relaxed) {
                    return Err(error("sync cancelled"));
                }
                let now = Instant::now();
                let mut assigned = vec![Vec::new(); self.peers.len()];
                let mut exhausted = Vec::new();
                for key in &pending {
                    let job = jobs.get_mut(key).expect("known height job");
                    if job.retry_at.is_some_and(|deadline| now < deadline) {
                        continue;
                    }
                    job.providers.shuffle(&mut rand::thread_rng());
                    if let Some(&(peer, block_id)) = job
                        .providers
                        .iter()
                        .filter(|&&(p, _)| self.peers[p].active && !job.tried.contains(&p))
                        .min_by_key(|&&(p, _)| self.peers[p].latency)
                    {
                        // Keep the fastest claimant's bounded request pipeline
                        // full instead of assigning the same common chain to
                        // slow claimants merely because its slots are occupied.
                        if assigned[peer].len() < MAX_BODY_REQUESTS {
                            job.tried.insert(peer);
                            self.body_requests += 1;
                            self.body_retries += u64::from(job.tried.len() > 1);
                            job.retry_at = Some(now + self.timeout);
                            assigned[peer].push((*key, block_id));
                        }
                    } else if !job
                        .providers
                        .iter()
                        .any(|&(p, _)| self.peers[p].active && !job.tried.contains(&p))
                    {
                        exhausted.push(*key);
                    }
                }
                for key in exhausted {
                    if let Some(failure) = unsupported.remove(&key) {
                        return Err(failure);
                    }
                    complete(&mut self.completed, &jobs[&key], None);
                }
                let profiling = std::env::var_os("MMX_PROFILE_REPLAY").is_some();
                let results = thread::scope(|scope| {
                    let tasks: Vec<_> = self
                        .peers
                        .iter_mut()
                        .zip(assigned)
                        .enumerate()
                        .filter(|(_, (_, keys))| !keys.is_empty())
                        .map(|(peer_index, (peer, keys))| {
                            let ids = keys.iter().map(|(_, id)| *id).collect::<Vec<_>>();
                            scope.spawn(move || {
                                let requested = Instant::now();
                                let source = peer
                                    .source
                                    .as_mut()
                                    .expect("hash claimant owns its connection");
                                let busy_before = source.backpressure_retries();
                                let result = source.fetch(&ids);
                                let elapsed = requested.elapsed();
                                peer.backpressure = source.backpressure_retries();
                                if profiling {
                                    let returned = result.as_ref().map_or(0, |values| {
                                        values.iter().filter(|value| value.is_some()).count()
                                    });
                                    eprintln!("Sync body: peer={} count={} returned={} metadata_ms={:.3} elapsed_ms={:.3} busy_retries={}",
                                        vnx::util::to_hex_string(&peer.id[..8], true, false),
                                        keys.len(), returned, peer.latency.as_secs_f64() * 1e3,
                                        elapsed.as_secs_f64() * 1e3,
                                        peer.backpressure.saturating_sub(busy_before));
                                }
                                if let Err(e) = &result {
                                    eprintln!(
                                        "Sync peer {}: block request failed: {e}",
                                        vnx::util::to_hex_string(&peer.id[..8], true, false)
                                    );
                                    peer.active = false;
                                }
                                (peer_index, keys, requested, result)
                            })
                        })
                        .collect();
                    tasks
                        .into_iter()
                        .map(|j| j.join().map_err(|_| error("peer worker panicked")))
                        .collect::<Result<Vec<_>>>()
                })?;
                let made_requests = !results.is_empty();
                for (_, keys, requested, result) in results {
                    for (key, _) in &keys {
                        jobs.get_mut(key).expect("known height job").retry_at =
                            Some(requested + self.timeout);
                    }
                    if let Ok(values) = result {
                        for ((key, requested_id), value) in keys.into_iter().zip(values) {
                            let job = &jobs[&key];
                            if let Some(block) = value {
                                // Never trust the echoed hash fields. prevalidate
                                // recomputes the full commitment and authenticates
                                // the farmer; contextual/CPU/ledger checks follow.
                                let matches = block.name == "mmx.Block"
                                    && number(&block, "height")
                                        .is_ok_and(|height| job.heights.contains(&height))
                                    && hash(&block, "hash").ok() == Some(requested_id)
                                    && hash(&block, "content_hash").ok() == Some(job.hash.content);
                                if matches {
                                    match mmx_execution::block::prevalidate(&block, params) {
                                        Ok(()) => complete(&mut self.completed, job, Some(block)),
                                        Err(failure) if failure.is_unsupported() => {
                                            unsupported.insert(
                                                key,
                                                Box::new(failure)
                                                    as Box<dyn std::error::Error + Send + Sync>,
                                            );
                                        }
                                        Err(_) => (),
                                    }
                                }
                            }
                        }
                    }
                }
                pending.retain(|key| !self.completed.contains_key(key));
                if !pending.is_empty() && !made_requests {
                    // Even an immediate bad/empty reply cannot trigger a second
                    // body request until the first provider's lease expires.
                    let deadline = pending
                        .iter()
                        .filter_map(|key| jobs[key].retry_at)
                        .min()
                        .unwrap_or_else(Instant::now);
                    if deadline > Instant::now() {
                        thread::sleep(
                            deadline
                                .saturating_duration_since(Instant::now())
                                .min(Duration::from_millis(50)),
                        );
                    }
                }
            }
            for height in start + first..end {
                let mut values = Vec::new();
                for (key, _) in jobs.range((height, [0; 32])..=(height, [255; 32])) {
                    let value = self.completed.get(key).expect("completed height job");
                    if let Some(block) = value {
                        let Value::Int(stamp) = block.field("time_stamp")? else {
                            continue;
                        };
                        if !mmx_execution::chain::sync::allows_timestamp(
                            *stamp,
                            vnx::util::get_wall_time_millis(),
                            params.block_interval_ms,
                            100,
                        ) {
                            continue;
                        }
                        bytes += block.encode()?.len();
                        values.push(block.clone());
                    }
                }
                output.heights.push(values);
            }
            first = end - start;
        }
        // Retain the recent tip/lookahead only; durable blocks are reused from
        // the archive. This also bounds negative-response bookkeeping.
        let low = start.saturating_sub(2 * BATCH_BLOCKS);
        let high = start.saturating_add(2 * BATCH_BLOCKS);
        self.completed
            .retain(|(height, _), _| *height >= low && *height <= high);
        output.body_time = body_started.elapsed();
        Ok(output)
    }
}

fn complete(
    completed: &mut BTreeMap<(u32, Hash), Option<Record>>,
    job: &Fetch,
    block: Option<Record>,
) {
    let actual = block.as_ref().and_then(|b| number(b, "height").ok());
    for &height in &job.heights {
        completed.insert(
            (height, job.hash.content),
            if actual == Some(height) {
                block.clone()
            } else {
                None
            },
        );
    }
}

/// The native node starts just above its soft root and walks back when a
/// candidate's parent is unknown. It admits every distinct candidate and lets
/// proof work and ledger execution select the branch, without hash voting.
pub fn synchronize_peers<S: PeerSource>(
    pool: &mut PeerPool<S>,
    node: &Arc<Mutex<Node>>,
    stop: &AtomicBool,
    report: impl FnMut(&Progress),
) -> Result<Progress> {
    synchronize_maintained(pool, node, stop, report, |_| Ok(()))
}
fn synchronize_maintained<S: PeerSource>(
    pool: &mut PeerPool<S>,
    node: &Arc<Mutex<Node>>,
    stop: &AtomicBool,
    mut report: impl FnMut(&Progress),
    maintain: impl FnMut(&mut PeerPool<S>) -> Result<()> + Send,
) -> Result<Progress> {
    let result = prefetch::with_maintenance(pool, node, stop, maintain, |source| {
        synchronize_inner(source, node, stop, &mut report)
    });
    node.lock()
        .map_err(|_| error("node lock poisoned"))?
        .sync_pending
        .clear();
    result
}
fn synchronize_inner(
    pool: &mut impl WindowSource,
    node: &Arc<Mutex<Node>>,
    stop: &AtomicBool,
    mut report: impl FnMut(&Progress),
) -> Result<Progress> {
    let started = Instant::now();
    let target = pool.refresh()?;
    let mut local = node.lock().map_err(|_| error("node lock poisoned"))?;
    local.begin_resync();
    let common = local.root;
    let earliest = local.earliest_height();
    let selected = local
        .chain
        .selected()
        .tip()?
        .ok_or_else(|| error("missing selected tip"))?
        .height;
    let mut progress = Progress {
        common,
        selected,
        target,
        ..Default::default()
    };
    let mut next = common
        .checked_add(1)
        .ok_or_else(|| error("sync height overflow"))?;
    drop(local);
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(error("sync cancelled"));
        }
        let limit = {
            let local = node.lock().map_err(|_| error("node lock poisoned"))?;
            local
                .root
                .saturating_add(local.chain.selected().params().commit_delay)
                .saturating_add(1000)
        };
        if next > limit {
            return Err(error("sync lookahead exhausted before root advancement"));
        }
        let count = BATCH_BLOCKS.min(limit - next + 1).min(u32::MAX - next);
        let download = Instant::now();
        let window = pool.window(next, count.max(1))?;
        progress.download_time += download.elapsed();
        progress.target = window.target;
        let end = next
            .checked_add(window.heights.len() as u32)
            .ok_or_else(|| error("sync height overflow"))?;
        let raw_empty = window
            .heights
            .iter()
            .position(Vec::is_empty)
            .map(|offset| next + offset as u32);
        let mut prefetched = None;
        let published = publish_with(window, node, &mut progress, |local, missing| {
            let limit = local
                .root
                .saturating_add(local.chain.selected().params().commit_delay)
                .saturating_add(1000);
            if raw_empty.is_none() && missing.is_empty() && end <= limit && end < u32::MAX {
                let count = BATCH_BLOCKS.min(limit - end + 1).min(u32::MAX - end);
                pool.prefetch(end, count, local)?;
                prefetched = Some((end, count));
            }
            Ok(())
        })?;
        let empty = published.empty();
        let mut missing = published.missing;
        if empty.is_some() {
            if let Some((start, count)) = prefetched {
                // Consensus can reject a statically valid suffix after its
                // successor was queued. Drain and retain that one bounded
                // window before the final probe, without activating it.
                let download = Instant::now();
                let retained = pool.window(start, count)?;
                progress.download_time += download.elapsed();
                account_window(&retained, &mut progress);
                progress.downloaded += retained.heights.iter().map(Vec::len).sum::<usize>() as u32;
            }
        }
        let walked_back = !missing.is_empty();
        // A peer can reorganize between replies inside one hash window. Begin
        // at the missing parent's height, including when it is in that window,
        // then walk backwards without lowering the selected root.
        let mut back = missing
            .iter()
            .map(|(height, _)| height.saturating_add(1))
            .max()
            .unwrap_or(next);
        while !missing.is_empty() && back > earliest.max(1) {
            let count = BATCH_BLOCKS.min(back - earliest.max(1));
            let download = Instant::now();
            let window = pool.window(back - count, count)?;
            progress.download_time += download.elapsed();
            let reached = window.start + window.heights.len() as u32;
            if reached != back {
                // Large early blocks exhausted the byte target.
                // Work backwards one height at a time when a forward range is
                // truncated, rather than skipping any missing ancestors.
                let download = Instant::now();
                let last = pool.window(back - 1, 1)?;
                progress.download_time += download.elapsed();
                missing.extend(
                    publish_with(last, node, &mut progress, |local, discovered| {
                        hold_missing(local, missing.iter().chain(discovered))
                    })?
                    .missing,
                );
                back -= 1;
            } else {
                back -= count;
            }
            missing.extend(
                publish_with(window, node, &mut progress, |local, discovered| {
                    hold_missing(local, missing.iter().chain(discovered))
                })?
                .missing,
            );
            let local = node.lock().map_err(|_| error("node lock poisoned"))?;
            missing = missing
                .into_iter()
                .map(|parent| Ok((!local.chain.contains_block(&parent.1)?).then_some(parent)))
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .flatten()
                .collect();
        }
        if !missing.is_empty() && earliest > 0 {
            return Err(error("peer fork requires history before the purge height"));
        }
        // Backwards jobs never prefetch forward work. Do not erase the next
        // producer window's pending heights during ordinary forward replay.
        if walked_back {
            node.lock()
                .map_err(|_| error("node lock poisoned"))?
                .sync_pending
                .clear();
        }
        progress.elapsed = started.elapsed();
        report(&progress);
        if let Some(peak) = empty {
            let mut local = node.lock().map_err(|_| error("node lock poisoned"))?;
            local.update()?;
            let selected = local
                .chain
                .selected()
                .tip()?
                .ok_or_else(|| error("missing selected tip"))?;
            progress.selected = selected.height;
            // Empty jobs before an existing tip indicate a stalled/inconsistent
            // peer set, never permission to silently truncate the ledger.
            if peak == selected.height.saturating_add(1) {
                let selected_height = selected.height;
                let selected_hash = selected.hash.0;
                drop(local);
                // Re-query all peers after publication, rather than trusting a
                // stale advertised height or requiring a majority hash. New
                // content is fetched once; completed content is reused.
                let download = Instant::now();
                let probe = pool.window(selected_height, 2)?;
                progress.download_time += download.elapsed();
                let advertised_selected = probe.heights[0]
                    .iter()
                    .any(|b| hash(b, "hash").ok() == Some(selected_hash));
                // Fresh variants must pass consensus too. Merely presenting a
                // self-consistent hash/signature cannot keep historical mode
                // open after their space work or ledger transition is rejected.
                let probe = publish_with(probe, node, &mut progress, |_, _| Ok(()))?;
                if advertised_selected
                    && probe.usable.first() == Some(&true)
                    && probe.usable.get(1) == Some(&false)
                    && probe.missing.is_empty()
                {
                    let mut local = node.lock().map_err(|_| error("node lock poisoned"))?;
                    if local
                        .chain
                        .selected()
                        .tip()?
                        .is_some_and(|t| t.hash.0 == selected_hash)
                    {
                        local.synced = true;
                        local.update()?;
                    }
                } else if probe.usable.get(1) == Some(&true) {
                    next = peak;
                    continue;
                }
            }
            progress.elapsed = started.elapsed();
            report(&progress);
            return Ok(progress);
        }
        next = end;
    }
}
fn account_window(window: &Window, progress: &mut Progress) {
    progress.hash_time += window.hash_time;
    progress.body_time += window.body_time;
    progress.body_requests += window.body_requests;
    progress.body_retries += window.body_retries;
    progress.archive_time += window.archive_time;
    progress.backpressure_retries += window.backpressure_retries;
}
fn publish_with(
    window: Window,
    node: &Arc<Mutex<Node>>,
    progress: &mut Progress,
    before_update: impl FnOnce(&mut Node, &Missing) -> Result<()>,
) -> Result<Published> {
    account_window(&window, progress);
    let mut local = node.lock().map_err(|_| error("node lock poisoned"))?;
    let params = local.chain.selected().params();
    let genesis = if window.start == 0 {
        local
            .chain
            .selected()
            .hash_at(0)?
            .map(|id| local.chain.block(&id))
            .transpose()?
            .flatten()
    } else {
        None
    };
    let mut blocks = Vec::new();
    let mut accepted = Vec::new();
    for values in &window.heights {
        let mut ids = Vec::new();
        for block in values {
            // The final tip probe includes height zero on a fresh network.
            // Recognize only our exact bootstrap; never admit peer genesis
            // variants or search for a genesis predecessor.
            if number(block, "height")? == 0 {
                if genesis.as_ref() == Some(block) {
                    ids.push(hash(block, "hash")?);
                }
                continue;
            }
            let Value::Int(stamp) = block.field("time_stamp")? else {
                continue;
            };
            if mmx_execution::chain::sync::allows_timestamp(
                *stamp,
                vnx::util::get_wall_time_millis(),
                params.block_interval_ms,
                100,
            ) {
                ids.push(hash(block, "hash")?);
                blocks.push(block.clone());
            }
        }
        accepted.push(ids);
    }
    let archive = Instant::now();
    local.chain.admit_blocks(&blocks)?;
    progress.archive_time += archive.elapsed();
    let mut missing = BTreeSet::new();
    for block in &blocks {
        let prev = hash(block, "prev")?;
        if !local.chain.contains_block(&prev)? {
            let height = number(block, "height")?
                .checked_sub(1)
                .ok_or_else(|| error("unexpected genesis in sync window"))?;
            missing.insert((height, prev));
        }
    }
    hold_missing(&mut local, &missing)?;
    // Empty replies do not authorize replay of retained future orphans. Keep
    // the active view at the contiguous prefix actually supplied by peers,
    // including when resuming an explicit rewind with a larger archive.
    let supplied = accepted
        .iter()
        .take_while(|values| !values.is_empty())
        .count() as u32;
    local.sync_pos = local.sync_pos.max(window.start.saturating_add(supplied));
    before_update(&mut local, &missing)?;
    let update = Instant::now();
    let result = local.update()?;
    progress.update_time += update.elapsed();
    progress.verification_time += result.verification_time;
    progress.replay_time += result.replay_time;
    progress.downloaded += blocks.len() as u32;
    progress.selected = local
        .chain
        .selected()
        .tip()?
        .ok_or_else(|| error("missing selected tip"))?
        .height;
    Ok(Published {
        missing,
        start: window.start,
        usable: accepted
            .into_iter()
            .map(|ids| ids.iter().any(|id| !result.invalid.contains(id)))
            .collect(),
    })
}

fn hold_missing<'a>(
    local: &mut Node,
    missing: impl IntoIterator<Item = &'a (u32, Hash)>,
) -> Result<()> {
    for (height, parent) in missing {
        if *height > 0 && !local.chain.contains_block(parent)? {
            local.sync_pending.insert(*height);
        }
    }
    Ok(())
}

/// Keep a random pool of authenticated native peers. One coordinator owns all
/// historical jobs; individual connections never start independent downloads.
pub fn run(
    mut addresses: Vec<String>,
    port: u16,
    node: &Arc<Mutex<Node>>,
    stop: &AtomicBool,
    once: bool,
) -> Result<()> {
    let (genesis, own_id) = {
        let local = node.lock().map_err(|_| error("node lock poisoned"))?;
        let genesis = local
            .chain
            .selected()
            .hash_at(0)?
            .ok_or_else(|| error("missing genesis"))?;
        let info = local.call(&crate::node::record("mmx.Router.get_id", []))?;
        let own_id = info.field("_ret_0")?.bytes()?.try_into()?;
        (genesis, own_id)
    };
    addresses.sort();
    addresses.dedup();
    let mut pool = PeerPool::<Client>::new(vec![]);
    let mut connected = BTreeMap::<Hash, String>::new();
    let mut retry_at = BTreeMap::<String, Instant>::new();
    let mut connections = connect::Connections::<QualifiedPeer>::new();
    let mut changed = Instant::now();
    let mut last_height = 0;
    let mut reported_peers = 0;
    let mut maintain = |pool: &mut PeerPool<Client>| -> Result<()> {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        pool.remove_failed();
        connected.retain(|id, _| pool.ids().contains(id));
        for (address, result) in connections.finished() {
            retry_at.insert(address.clone(), Instant::now() + Duration::from_secs(30));
            match result {
                Ok((id, height, client, discovered)) => {
                    if pool.add(id, client) {
                        println!(
                            "Sync peer {address}: authenticated {}, synced height {height}",
                            vnx::util::to_hex_string(&id[..8], true, false)
                        );
                        connected.insert(id, address);
                    }
                    for address in discovered {
                        if addresses.len() >= 512 {
                            break;
                        }
                        // Native discovery returns numeric IP addresses.
                        if let Ok(ip) = address.parse::<std::net::IpAddr>() {
                            let endpoint = std::net::SocketAddr::new(ip, port).to_string();
                            if !addresses.contains(&endpoint) {
                                addresses.push(endpoint);
                            }
                        }
                    }
                }
                Err(e) => eprintln!("Sync peer {address}: {e}"),
            }
        }
        let connect_node = node.clone();
        connections.refill(
            pool.len(),
            &addresses,
            &connected,
            &mut retry_at,
            move |address| {
                let mut client = Client::connect(address, connect_node.clone())?;
                let (id, height) = client.qualify(genesis, own_id)?;
                let discovered = client.known_peers().unwrap_or_default();
                Ok((id, height, client, discovered))
            },
        )?;
        // Avoid the ledger mutex on ordinary prefetch windows, so network I/O
        // continues to overlap block validation when the pool is unchanged.
        if reported_peers != pool.len() {
            reported_peers = pool.len();
            node.lock()
                .map_err(|_| error("node lock poisoned"))?
                .sync_peers = reported_peers;
            if reported_peers >= MIN_PEERS {
                println!("Sync using {reported_peers} distinct peers (minimum {MIN_PEERS}, maximum {MAX_PEERS})");
            }
        }
        Ok(())
    };
    while !stop.load(Ordering::Relaxed) {
        maintain(&mut pool)?;
        if pool.len() < MIN_PEERS {
            node.lock()
                .map_err(|_| error("node lock poisoned"))?
                .begin_resync();
            eprintln!(
                "Sync waiting for peers: {} / {MIN_PEERS} minimum",
                pool.len()
            );
            pool.poll()?;
            for _ in 0..10 {
                if stop.load(Ordering::Relaxed) {
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(100));
            }
            continue;
        }
        let attempt = (|| -> Result<()> {
            if !node
                .lock()
                .map_err(|_| error("node lock poisoned"))?
                .is_synced()
            {
                synchronize_maintained(
                    &mut pool,
                    node,
                    stop,
                    |p| {
                        println!("Sync: {} / {}, candidates {} since root {}, {:.1} candidates/s; download wait {:.3}s (hashes {:.3}s bodies {:.3}s, {} body requests, {} retries, {} busy retries) archive {:.3}s update {:.3}s (proofs {:.3}s replay {:.3}s)",
                        p.selected, p.target, p.downloaded, p.common,
                        f64::from(p.downloaded) / p.elapsed.as_secs_f64(), p.download_time.as_secs_f64(), p.hash_time.as_secs_f64(), p.body_time.as_secs_f64(), p.body_requests, p.body_retries, p.backpressure_retries,
                        p.archive_time.as_secs_f64(), p.update_time.as_secs_f64(), p.verification_time.as_secs_f64(), p.replay_time.as_secs_f64());
                    },
                    &mut maintain,
                )?;
                if once
                    && node
                        .lock()
                        .map_err(|_| error("node lock poisoned"))?
                        .is_synced()
                {
                    stop.store(true, Ordering::Relaxed);
                    return Ok(());
                }
            }
            pool.poll()?;
            let remote = pool.refresh()?;
            let mut local = node.lock().map_err(|_| error("node lock poisoned"))?;
            let current = local
                .chain
                .selected()
                .tip()?
                .ok_or_else(|| error("missing selected tip"))?
                .height;
            if current != last_height {
                last_height = current;
                changed = Instant::now();
            }
            if remote > current && changed.elapsed() > Duration::from_secs(60) {
                local.begin_resync();
            }
            Ok(())
        })();
        if let Err(e) = attempt {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            eprintln!("Sync: {e}");
            node.lock()
                .map_err(|_| error("node lock poisoned"))?
                .begin_resync();
            thread::sleep(Duration::from_millis(200));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
