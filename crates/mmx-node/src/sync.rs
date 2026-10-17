//! Download complete native blocks without exposing an unvalidated rollback.
//! A competing branch is archived and replayed atomically by ForkChain; RPC
//! readers see selected ledger snapshots between download batches.
mod prefetch;
use crate::{
    error,
    node::{hash, number},
    peer::Client,
    Node, Result,
};
use mmx_consensus::{Hash, Record};
pub use prefetch::synchronize_pipelined;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

// Amortize durable archive/ledger publication without retaining a whole chain
// in RAM. The byte budget and native root/lookahead still bound each window.
const BATCH_BLOCKS: u32 = 64;
const BATCH_BYTES: usize = 8 * 1024 * 1024;

pub trait Source {
    fn height(&mut self) -> Result<u32>;
    fn hash_at(&mut self, height: u32) -> Result<Option<Hash>>;
    fn block_at(&mut self, height: u32) -> Result<Option<Record>>;
    /// Optionally start the next bounded window while the caller validates the
    /// current one. Ordinary sources remain synchronous.
    fn prefetch(&mut self, _start: u32, _count: u32) -> Result<()> {
        Ok(())
    }
    fn blocks_at(&mut self, start: u32, count: u32) -> Result<Vec<Option<Record>>> {
        (0..count)
            .map(|i| {
                self.block_at(
                    start
                        .checked_add(i)
                        .ok_or_else(|| error("sync height overflow"))?,
                )
            })
            .collect()
    }
}
impl Source for Client {
    fn height(&mut self) -> Result<u32> {
        Client::height(self)
    }
    fn hash_at(&mut self, h: u32) -> Result<Option<Hash>> {
        Client::hash_at(self, h)
    }
    fn block_at(&mut self, h: u32) -> Result<Option<Record>> {
        Client::block_at(self, h)
    }
    fn blocks_at(&mut self, start: u32, count: u32) -> Result<Vec<Option<Record>>> {
        Client::blocks_at(self, start, count)
    }
}
#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub common: u32,
    pub downloaded: u32,
    pub selected: u32,
    pub target: u32,
    pub elapsed: Duration,
    pub download_time: Duration,
    pub archive_time: Duration,
    pub update_time: Duration,
    pub verification_time: Duration,
    pub replay_time: Duration,
}
pub fn synchronize(
    source: &mut impl Source,
    node: &Arc<Mutex<Node>>,
    stop: &AtomicBool,
    mut report: impl FnMut(&Progress),
) -> Result<Progress> {
    let started = Instant::now();
    let target = source.height()?;
    let local = node.lock().map_err(|_| error("node lock poisoned"))?;
    let chain = local.chain.selected();
    let genesis = chain.hash_at(0)?.ok_or_else(|| error("missing genesis"))?;
    let tip = chain.tip()?.ok_or_else(|| error("missing tip"))?;
    let interval = chain.params().block_interval_ms;
    drop(local);
    if source.hash_at(0)? != Some(genesis) {
        return Err(error("peer genesis mismatch"));
    }
    let mut low = 0;
    let mut high = target.min(tip.height);
    while low < high {
        if stop.load(Ordering::Relaxed) {
            return Err(error("sync cancelled"));
        }
        let middle = low + (high - low).div_ceil(2);
        let remote = source
            .hash_at(middle)?
            .ok_or_else(|| error("peer omitted advertised height"))?;
        let ours = node
            .lock()
            .map_err(|_| error("node lock poisoned"))?
            .chain
            .selected()
            .hash_at(middle)?;
        if ours == Some(remote) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    let common = low;
    let mut expected = node
        .lock()
        .map_err(|_| error("node lock poisoned"))?
        .chain
        .selected()
        .hash_at(common)?
        .ok_or_else(|| error("missing common ancestor"))?;
    let mut progress = Progress {
        common,
        target,
        selected: tip.height,
        ..Default::default()
    };
    let mut next = common
        .checked_add(1)
        .ok_or_else(|| error("sync height overflow"))?;
    while next <= target {
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
        let mut batch = Vec::new();
        let mut size = 0;
        let download_started = Instant::now();
        while next <= target
            && next <= limit
            && batch.len() < BATCH_BLOCKS as usize
            && size < BATCH_BYTES
        {
            if stop.load(Ordering::Relaxed) {
                return Err(error("sync cancelled"));
            }
            let count = (target - next + 1)
                .min(4)
                .min(BATCH_BLOCKS - batch.len() as u32)
                .min(limit - next + 1);
            let blocks = source.blocks_at(next, count)?;
            if blocks.len() != count as usize {
                return Err(error("peer omitted advertised block window"));
            }
            for block in blocks {
                let block = block.ok_or_else(|| error("peer omitted advertised block"))?;
                if !mmx_execution::chain::sync::allows_timestamp(
                    match block.field("time_stamp")? {
                        mmx_consensus::Value::Int(t) => *t,
                        _ => return Err(error("invalid signed block timestamp")),
                    },
                    vnx::util::get_wall_time_millis(),
                    interval,
                    100,
                ) {
                    return Err(error("peer block is too far in the future"));
                }
                if block.name != "mmx.Block"
                    || number(&block, "height")? != next
                    || hash(&block, "prev")? != expected
                {
                    return Err(error("peer changed branch or returned wrong block"));
                }
                expected = hash(&block, "hash")?;
                size += block.encode()?.len();
                batch.push(block);
                next = next
                    .checked_add(1)
                    .ok_or_else(|| error("sync height overflow"))?;
            }
        }
        progress.download_time += download_started.elapsed();
        let mut local = node.lock().map_err(|_| error("node lock poisoned"))?;
        if local.synced {
            return Err(error("historical download requested after live transition"));
        }
        // Use the existing root, before publishing this window. A producer can
        // fetch conservatively ahead without requiring the node lock during I/O.
        let lookahead = local
            .root
            .saturating_add(local.chain.selected().params().commit_delay)
            .saturating_add(1000);
        if next <= target && next <= lookahead {
            source.prefetch(
                next,
                (target - next + 1)
                    .min(BATCH_BLOCKS)
                    .min(lookahead - next + 1),
            )?;
        }
        let archive_started = Instant::now();
        local.chain.admit_blocks(&batch)?;
        progress.archive_time += archive_started.elapsed();
        local.sync_pos = next;
        let update_started = Instant::now();
        let result = local.update()?;
        progress.update_time += update_started.elapsed();
        progress.verification_time += result.verification_time;
        progress.replay_time += result.replay_time;
        if batch
            .iter()
            .any(|b| hash(b, "hash").is_ok_and(|h| result.invalid.contains(&h)))
        {
            return Err(error("peer branch failed consensus validation"));
        }
        progress.downloaded += batch.len() as u32;
        progress.selected = local
            .chain
            .selected()
            .tip()?
            .ok_or_else(|| error("missing selected tip"))?
            .height;
        drop(local);
        progress.elapsed = started.elapsed();
        report(&progress);
    }
    // Repeat the peer's tip hash to detect a mid-download reorganization. Do not
    // advertise synced merely because a weaker/invalid branch finished arriving.
    let remote = source
        .hash_at(target)?
        .ok_or_else(|| error("peer lost tip"))?;
    let current_target = source.height()?;
    let mut local = node.lock().map_err(|_| error("node lock poisoned"))?;
    // Restart loses transient native proof flags. Rebuild the selected window
    // in sync mode even when no new block needed downloading before going live.
    local.update()?;
    let selected = local
        .chain
        .selected()
        .tip()?
        .ok_or_else(|| error("missing selected tip"))?;
    progress.selected = selected.height;
    if current_target == target && selected.height == target && selected.hash.0 == remote {
        local.synced = true;
        local.update()?;
    }
    Ok(progress)
}
