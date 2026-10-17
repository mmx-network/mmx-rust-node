//! Download complete native blocks without exposing an unvalidated rollback.
//! A competing branch is archived and replayed atomically by ForkChain; RPC
//! readers see selected ledger snapshots between download batches.
use crate::{
    error,
    node::{hash, number},
    peer::Client,
    Node, Result,
};
use mmx_consensus::{Hash, Record};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

pub trait Source {
    fn height(&mut self) -> Result<u32>;
    fn hash_at(&mut self, height: u32) -> Result<Option<Hash>>;
    fn block_at(&mut self, height: u32) -> Result<Option<Record>>;
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
}
#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub common: u32,
    pub downloaded: u32,
    pub selected: u32,
    pub target: u32,
}
pub fn synchronize(
    source: &mut impl Source,
    node: &Arc<Mutex<Node>>,
    stop: &AtomicBool,
    mut report: impl FnMut(&Progress),
) -> Result<Progress> {
    let target = source.height()?;
    let local = node.lock().map_err(|_| error("node lock poisoned"))?;
    let chain = local.chain.selected();
    let genesis = chain.hash_at(0)?.ok_or_else(|| error("missing genesis"))?;
    let tip = chain.tip()?.ok_or_else(|| error("missing tip"))?;
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
        let mut batch = Vec::new();
        let mut size = 0;
        while next <= target && batch.len() < 16 && size < 8 * 1024 * 1024 {
            if stop.load(Ordering::Relaxed) {
                return Err(error("sync cancelled"));
            }
            let block = source
                .block_at(next)?
                .ok_or_else(|| error("peer omitted advertised block"))?;
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
        let mut local = node.lock().map_err(|_| error("node lock poisoned"))?;
        if local.synced {
            return Err(error("historical download requested after live transition"));
        }
        if progress.downloaded == 0 {
            local.root = local.root.min(common);
        }
        for block in &batch {
            local.chain.admit(block, &[])?;
        }
        local.sync_pos = next;
        let result = local.update()?;
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
        report(&progress);
    }
    // Repeat the peer's tip hash to detect a mid-download reorganization. Do not
    // advertise synced merely because a weaker/invalid branch finished arriving.
    let remote = source
        .hash_at(target)?
        .ok_or_else(|| error("peer lost tip"))?;
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
    if selected.height == target && selected.hash.0 == remote {
        local.synced = true;
        local.update()?;
    }
    Ok(progress)
}
