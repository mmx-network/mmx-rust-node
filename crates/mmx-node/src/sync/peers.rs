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
// Preserve the previous eight-peer worst-case bound while allowing smaller
// authenticated pools to fill a complete body-request pipeline as well.
const MAX_BODY_INFLIGHT: usize = 32;
type Missing = BTreeSet<(u32, Hash)>;
type QualifiedPeer = (Hash, u32, Client);
mod body;
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
    /// Deliver each correlated body as it completes, retaining successful
    /// replies even when another request in this batch subsequently fails.
    fn fetch_each(
        &mut self,
        hashes: &[Hash],
        emit: &mut dyn FnMut(usize, Option<Record>) -> Result<()>,
    ) -> Result<()> {
        let values = self.fetch(hashes)?;
        if values.len() != hashes.len() {
            return Err(error("incomplete body response batch"));
        }
        for (index, value) in values.into_iter().enumerate() {
            emit(index, value)?;
        }
        Ok(())
    }
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
    fn fetch_each(
        &mut self,
        hashes: &[Hash],
        emit: &mut dyn FnMut(usize, Option<Record>) -> Result<()>,
    ) -> Result<()> {
        self.blocks_by_hash_each(hashes, emit)
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
    replacement: Option<S>,
    inflight: Option<metadata::Job<S>>,
    body: Option<body::Job<S>>,
    active: bool,
    qualified: bool,
    needs_status: bool,
    height: u32,
    latency: Duration,
    backpressure: u64,
}
pub struct PeerPool<S> {
    peers: Vec<Peer<S>>,
    minimum: usize,
    maximum: usize,
    query_interval: Duration,
    timeout: Duration,
    // Complete content responses (including invalid ones) are never fetched
    // twice within a height job, even when the driver repeats a tip probe.
    completed: BTreeMap<(u32, Hash), Option<Record>>,
    refreshed: Instant,
    body_requests: u64,
    body_retries: u64,
    retired_backpressure: u64,
    metadata: metadata::Queue,
    bodies: body::Queue,
    retained_heights: std::ops::RangeInclusive<u32>,
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
    pub fn new(peers: Vec<(Hash, S)>) -> Self {
        let config = crate::router::RouterConfig::default();
        Self::with_config(peers, &config)
    }
    pub fn with_config(mut peers: Vec<(Hash, S)>, config: &crate::router::RouterConfig) -> Self {
        peers.shuffle(&mut rand::thread_rng());
        let mut ids = BTreeSet::new();
        let peers = peers
            .into_iter()
            .filter(|(id, _)| ids.insert(*id))
            .take(config.pool_capacity())
            .map(|(id, source)| Peer {
                id,
                source: Some(source),
                replacement: None,
                inflight: None,
                body: None,
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
            minimum: config.min_sync_peers,
            maximum: config.pool_capacity(),
            query_interval: config.query_interval,
            timeout: config.body_timeout,
            completed: BTreeMap::new(),
            refreshed: Instant::now() - config.query_interval,
            body_requests: 0,
            body_retries: 0,
            retired_backpressure: 0,
            metadata: metadata::Queue::new(config.pool_capacity()),
            bodies: body::Queue::new(),
            retained_heights: 0..=0,
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
            .filter(|p| {
                !p.active && p.replacement.is_none() && p.inflight.is_none() && p.body.is_none()
            })
            .map(|p| p.backpressure)
            .sum::<u64>();
        self.peers.retain(|p| {
            p.active || p.replacement.is_some() || p.inflight.is_some() || p.body.is_some()
        });
    }
    fn backpressure_retries(&self) -> u64 {
        self.retired_backpressure + self.peers.iter().map(|p| p.backpressure).sum::<u64>()
    }
    pub fn add(&mut self, id: Hash, source: S) -> bool {
        if self.peers.len() >= self.maximum || self.peers.iter().any(|p| p.id == id) {
            return false;
        }
        self.peers.push(Peer {
            id,
            source: Some(source),
            replacement: None,
            inflight: None,
            body: None,
            active: true,
            qualified: false,
            needs_status: true,
            height: 0,
            latency: Duration::MAX,
            backpressure: 0,
        });
        self.refreshed = Instant::now() - self.query_interval;
        true
    }
    fn make_room_for_ahead_peer(
        &mut self,
        id: Hash,
        height: u32,
        connected: &mut BTreeMap<Hash, String>,
        fixed: &BTreeSet<String>,
    ) {
        if self.peers.len() < self.maximum || self.peers.iter().any(|peer| peer.id == id) {
            return;
        }
        let replace = self
            .peers
            .iter()
            .enumerate()
            .filter(|(_, peer)| {
                peer.source.is_some()
                    && peer.inflight.is_none()
                    && peer.body.is_none()
                    && peer.height < height
                    && connected
                        .get(&peer.id)
                        .is_some_and(|address| !fixed.contains(address))
            })
            .min_by_key(|(_, peer)| peer.height)
            .map(|(index, _)| index);
        if let Some(index) = replace {
            let old = self.peers.remove(index);
            connected.remove(&old.id);
            self.retired_backpressure += old.backpressure;
        }
    }
    /// Native identity replacement has already shut down the old socket. Keep
    /// its metadata owner until it returns, then install the authenticated
    /// replacement in the same slot. It never counts as a second observer.
    pub fn install_authenticated(&mut self, id: Hash, source: S) -> bool {
        let Some(peer) = self.peers.iter_mut().find(|peer| peer.id == id) else {
            return self.add(id, source);
        };
        if peer.inflight.is_some() || peer.body.is_some() {
            peer.replacement = Some(source);
            peer.active = false;
            peer.qualified = false;
        } else {
            self.retired_backpressure += peer.backpressure;
            peer.source = Some(source);
            peer.replacement = None;
            peer.active = true;
            peer.qualified = false;
            peer.needs_status = true;
            peer.height = 0;
            peer.latency = Duration::MAX;
            peer.backpressure = 0;
        }
        self.refreshed = Instant::now() - self.query_interval;
        true
    }
    pub fn poll(&mut self) -> Result<()> {
        self.drain_control()?;
        self.start_polls()?;
        self.poll_metadata()
    }
    fn next_query(&mut self) -> u64 {
        self.query = self.query.wrapping_add(1);
        self.query
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
        // Keep completed bodies in bounded RAM even if the consumer cancels.
        // The active view still ends at sync_pos; only fully executed branches
        // enter the durable archive. Unvalidated prefetch expires on restart.
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
        self.retained_heights =
            start.saturating_sub(2 * BATCH_BLOCKS)..=start.saturating_add(2 * BATCH_BLOCKS);
        self.completed
            .retain(|(height, _), _| self.retained_heights.contains(height));
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
        let params = Arc::new(context.params.clone());
        self.inherit_body_leases(&mut jobs);
        let mut output = Window {
            start,
            hash_time,
            ..Default::default()
        };
        let body_started = Instant::now();
        let mut unsupported = BTreeMap::new();
        let mut bytes = 0usize;
        let mut height = start;
        loop {
            for (key, failure) in self.drain_bodies()? {
                if jobs.contains_key(&key) {
                    unsupported.insert(key, failure);
                }
            }
            if stop.load(Ordering::Relaxed) {
                return Err(error("sync cancelled"));
            }
            self.expire_bodies();
            // Consume ready heights in order, but let every provider continue
            // filling the shared request budget while another owner stalls.
            while height < start + count {
                let range = (height, [0; 32])..=(height, [255; 32]);
                if jobs
                    .range(range.clone())
                    .any(|(key, _)| !self.completed.contains_key(key))
                {
                    break;
                }
                let mut values = Vec::new();
                for (key, _) in jobs.range(range) {
                    if let Some(block) = self.completed.get(key).and_then(Option::as_ref) {
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
                height += 1;
                if bytes >= BATCH_BYTES {
                    break;
                }
            }
            if height == start + count || bytes >= BATCH_BYTES {
                break;
            }
            let now = Instant::now();
            let mut slots = MAX_BODY_INFLIGHT.saturating_sub(self.pending_bodies());
            let mut assigned: Vec<Vec<body::Request>> =
                (0..self.peers.len()).map(|_| Vec::new()).collect();
            let mut exhausted = Vec::new();
            for (key, job) in &mut jobs {
                if self.completed.contains_key(key)
                    || job.heights.first() != Some(&key.0)
                    || job.retry_at.is_some_and(|deadline| now < deadline)
                {
                    continue;
                }
                job.providers.shuffle(&mut rand::thread_rng());
                if let Some(&(peer, block_id)) = job
                    .providers
                    .iter()
                    .filter(|&&(p, _)| self.peers[p].active && !job.tried.contains(&p))
                    .min_by_key(|&&(p, _)| self.peers[p].latency)
                {
                    // A busy fast claimant keeps priority for unattempted
                    // content; slower claimants are used only after a lease.
                    if slots > 0
                        && self.peers[peer].source.is_some()
                        && assigned[peer].len() < MAX_BODY_REQUESTS
                    {
                        job.tried.insert(peer);
                        self.body_requests += 1;
                        self.body_retries += u64::from(job.tried.len() > 1);
                        slots -= 1;
                        assigned[peer].push(body::Request {
                            key: *key,
                            block: block_id,
                            heights: job.heights.clone(),
                        });
                    }
                } else {
                    exhausted.push(*key);
                }
            }
            for (peer, requests) in assigned.into_iter().enumerate() {
                if !requests.is_empty() {
                    let keys: Vec<_> = requests.iter().map(|request| request.key).collect();
                    let requested = self.start_bodies(peer, requests, params.clone())?;
                    for key in keys {
                        jobs.get_mut(&key).unwrap().retry_at = Some(requested + self.timeout);
                    }
                }
            }
            let exhausted_any = !exhausted.is_empty();
            for key in exhausted {
                if let Some(failure) = unsupported.remove(&key) {
                    return Err(failure);
                }
                complete(&mut self.completed, &jobs[&key], None);
            }
            if exhausted_any {
                continue;
            }
            let wait = jobs
                .values()
                .filter_map(|job| job.retry_at)
                .filter(|deadline| *deadline > Instant::now())
                .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                .min()
                .unwrap_or(Duration::from_millis(50))
                .min(Duration::from_millis(50));
            if let Some((key, failure)) = self.wait_body(wait)? {
                if jobs.contains_key(&key) {
                    unsupported.insert(key, failure);
                }
            }
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
    stop: &Arc<AtomicBool>,
    once: bool,
) -> Result<()> {
    let (genesis, own_id, policy, book) = {
        let local = node.lock().map_err(|_| error("node lock poisoned"))?;
        let genesis = local
            .chain
            .selected()
            .hash_at(0)?
            .ok_or_else(|| error("missing genesis"))?;
        let info = local.call(&crate::node::record("mmx.Router.get_id", []))?;
        let own_id = info.field("_ret_0")?.bytes()?.try_into()?;
        let state = local.peer_state();
        (genesis, own_id, state.router, state.book)
    };
    let mut policy = (*policy).clone();
    policy.fixed_peers = policy
        .fixed_peers
        .iter()
        .map(|address| crate::router::endpoint(address, port))
        .collect::<Result<_>>()?;
    addresses.extend(policy.fixed_peers.iter().cloned());
    addresses.retain(|address| !policy.blocks(address));
    addresses.sort();
    addresses.dedup();
    let minimum = policy.min_sync_peers;
    let maximum = policy.pool_capacity();
    let explicit = addresses.clone();
    let mut directory_revision = 0;
    let mut pool = PeerPool::<Client>::with_config(vec![], &policy);
    let mut connected = BTreeMap::<Hash, String>::new();
    let mut retry_at = BTreeMap::<String, Instant>::new();
    let mut connections = connect::Connections::<QualifiedPeer>::with_config(&policy);
    let mut changed = Instant::now();
    let mut last_height = 0;
    let mut reported_peers = 0;
    let mut maintain = |pool: &mut PeerPool<Client>| -> Result<()> {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        if let Some((revision, discovered)) = book.candidates(directory_revision)? {
            directory_revision = revision;
            addresses = explicit.clone();
            addresses.extend(discovered);
            addresses.sort();
            addresses.dedup();
            retry_at.retain(|address, _| {
                addresses.binary_search(address).is_ok() || connections.pending(address)
            });
        }
        pool.remove_failed();
        connected.retain(|id, address| {
            let retained = pool.peers.iter().any(|peer| peer.id == *id);
            if !retained {
                retry_at.insert(address.clone(), Instant::now() + policy.peer_retry_interval);
            }
            retained
        });
        for (address, result) in connections.finished() {
            retry_at.insert(address.clone(), Instant::now() + policy.peer_retry_interval);
            match result {
                Ok((id, height, client)) => {
                    // Higher advertised height only affects peer selection;
                    // the new peer's chain/data still requires full validation.
                    pool.make_room_for_ahead_peer(id, height, &mut connected, &policy.fixed_peers);
                    if pool.install_authenticated(id, client) {
                        println!(
                            "Sync peer {address}: authenticated {}, synced height {height}",
                            vnx::util::to_hex_string(&id[..8], true, false)
                        );
                        connected.insert(id, address);
                    }
                }
                Err(e) => eprintln!("Sync peer {address}: {e}"),
            }
        }
        let connect_node = node.clone();
        let connect_stop = stop.clone();
        connections.refill(
            pool.peers.len(),
            &addresses,
            &connected,
            &mut retry_at,
            move |address| {
                let mut client = Client::connect_stoppable(
                    address,
                    connect_node.clone(),
                    Some(connect_stop.clone()),
                )?;
                let (id, height) = client.qualify(genesis, own_id)?;
                client.enable_discovery(true);
                Ok((id, height, client))
            },
        )?;
        // Avoid the ledger mutex on ordinary prefetch windows, so network I/O
        // continues to overlap block validation when the pool is unchanged.
        if reported_peers != pool.len() {
            reported_peers = pool.len();
            node.lock()
                .map_err(|_| error("node lock poisoned"))?
                .sync_peers = reported_peers;
            if reported_peers >= minimum {
                println!("Sync using {reported_peers} distinct peers (minimum {minimum}, maximum {maximum})");
            }
        }
        Ok(())
    };
    while !stop.load(Ordering::Relaxed) {
        maintain(&mut pool)?;
        if pool.len() < minimum {
            node.lock()
                .map_err(|_| error("node lock poisoned"))?
                .begin_resync();
            eprintln!("Sync waiting for peers: {} / {minimum} minimum", pool.len());
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
            if remote > current && changed.elapsed() > policy.sync_loss_delay {
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
