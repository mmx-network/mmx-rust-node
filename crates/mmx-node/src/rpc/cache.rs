//! Final public REST JSON responses. Selected-chain events push already-
//! loaded blocks; hits never query the ledger, retain DB views or convert VNX.
use crate::{
    node::{hash, number},
    Node, Result,
};
use mmx_consensus::Record;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};
#[cfg(test)]
mod tests;

// Boxed payloads have exact lengths. These conservative charges cover all three
// ordered indexes, their sparsely occupied nodes, entry metadata and allocator
// overhead. The budget bounds retained cache memory, not temporary RPC replies
// or the process's allocator/RSS high-water mark.
const BASE_BYTES: usize = 4096;
const ENTRY_BYTES: usize = 2048;
const MAX_KEY_BYTES: usize = 64 * 1024;
pub const DEFAULT_BYTES: usize = 64 * 1024 * 1024;
pub(crate) type Shared = Arc<Mutex<Cache>>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub limit_bytes: usize,
    pub used_bytes: usize,
    pub entries: usize,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}
// None depends on the whole selected state; Some(height) on the selected
// prefix through that height. Only publication events invalidate these indexes.
type Dependency = Option<u32>;
struct Entry {
    value: Box<[u8]>,
    dependency: Dependency,
    frequency: u64,
    touched: u64,
    charge: usize,
    owners: Box<[[u8; 32]]>,
    web_history: Option<Box<crate::webapi::Watch>>,
}
type Rank = (u64, u64, Box<[u8]>);
pub(crate) struct Cache {
    entries: BTreeMap<Box<[u8]>, Entry>,
    order: BTreeSet<Rank>,
    dependencies: BTreeMap<Dependency, BTreeSet<Box<[u8]>>>,
    history_owners: BTreeMap<[u8; 32], BTreeSet<Box<[u8]>>>,
    stats: Stats,
    clock: u64,
    generation: u64,
    tip_height: Option<u32>,
    publishing: bool,
    synced_since: Option<u32>,
    sync_delay: u32,
}
impl Cache {
    pub(super) fn new(limit_bytes: usize, tip_height: u32) -> Self {
        Self {
            entries: BTreeMap::new(),
            order: BTreeSet::new(),
            dependencies: BTreeMap::new(),
            history_owners: BTreeMap::new(),
            stats: Stats {
                limit_bytes,
                ..Default::default()
            },
            clock: 0,
            generation: 0,
            tip_height: Some(tip_height),
            publishing: false,
            synced_since: None,
            sync_delay: 18,
        }
    }
    pub(super) fn stats(&self) -> Stats {
        self.stats
    }
    fn remove(&mut self, key: &[u8]) {
        if let Some(entry) = self.entries.remove(key) {
            self.order
                .remove(&(entry.frequency, entry.touched, key.into()));
            if let Some(keys) = self.dependencies.get_mut(&entry.dependency) {
                keys.remove(key);
                if keys.is_empty() {
                    self.dependencies.remove(&entry.dependency);
                }
            }
            for owner in entry.owners.iter() {
                if let Some(keys) = self.history_owners.get_mut(owner) {
                    keys.remove(key);
                    if keys.is_empty() {
                        self.history_owners.remove(owner);
                    }
                }
            }
            self.stats.used_bytes -= entry.charge;
            self.stats.entries -= 1;
            if self.entries.is_empty() {
                self.stats.used_bytes = 0;
            }
        }
    }
    fn reset(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.dependencies.clear();
        self.history_owners.clear();
        self.stats.used_bytes = 0;
        self.stats.entries = 0;
        self.generation = self.generation.wrapping_add(1);
        self.tip_height = None;
        self.publishing = false;
    }
    fn selected(&mut self, common: u32, tip: u32) {
        self.publishing = true;
        self.generation = self.generation.wrapping_add(1);
        let extension = self.tip_height == Some(common);
        self.tip_height = Some(tip);
        let state = self.dependencies.remove(&None).unwrap_or_default();
        let replaced = if common == u32::MAX {
            BTreeMap::new()
        } else {
            self.dependencies.split_off(&Some(common + 1))
        };
        for key in state {
            if extension
                && self
                    .entries
                    .get(&key)
                    .is_some_and(|e| e.web_history.is_some())
            {
                self.dependencies.entry(None).or_default().insert(key);
            } else {
                self.remove(&key);
            }
        }
        for key in replaced.into_values().flatten() {
            self.remove(&key);
        }
    }
    fn tick(&mut self) -> u64 {
        // This also prevents rank collisions on an eventual u64 wrap.
        if self.clock == u64::MAX {
            self.reset();
            self.clock = 0;
        }
        self.clock += 1;
        self.clock
    }
    fn get(&mut self, key: &[u8]) -> Option<Vec<u8>> {
        let touched = self.tick();
        if let Some(entry) = self.entries.get_mut(key) {
            self.order
                .remove(&(entry.frequency, entry.touched, key.into()));
            entry.frequency = entry.frequency.saturating_add(1);
            entry.touched = touched;
            self.order.insert((entry.frequency, touched, key.into()));
            self.stats.hits = self.stats.hits.saturating_add(1);
            Some(entry.value.to_vec())
        } else {
            self.stats.misses = self.stats.misses.saturating_add(1);
            None
        }
    }
    fn insert(&mut self, key: Box<[u8]>, value: Box<[u8]>, dependency: Dependency) {
        self.insert_watched(key, value, dependency, None);
    }
    fn insert_watched(
        &mut self,
        key: Box<[u8]>,
        value: Box<[u8]>,
        dependency: Dependency,
        web_history: Option<Box<crate::webapi::Watch>>,
    ) {
        if value.len() > super::MAX_RESPONSE_BYTES {
            return;
        }
        let owners = match &web_history {
            Some(watch) => watch.owners(),
            None => Ok(Vec::new()),
        };
        let Ok(mut owners) = owners else {
            return;
        };
        owners.sort_unstable();
        owners.dedup();
        let owner_charge = owners.len().saturating_mul(key.len().saturating_add(256));
        let watch_charge = web_history.as_ref().map_or(0, |watch| watch.charge());
        let charge = ENTRY_BYTES
            .saturating_add(key.len().saturating_mul(3))
            .saturating_add(value.len())
            .saturating_add(owner_charge)
            .saturating_add(watch_charge);
        if BASE_BYTES.saturating_add(charge) > self.stats.limit_bytes {
            return;
        }
        // Another worker may have completed the identical request first. Do
        // not reset its popularity or let an older snapshot replace it.
        if self.entries.contains_key(key.as_ref()) {
            return;
        }
        while self.stats.used_bytes.max(BASE_BYTES).saturating_add(charge) > self.stats.limit_bytes
        {
            let Some((_, _, key)) = self.order.first().cloned() else {
                return;
            };
            self.remove(&key);
            self.stats.evictions = self.stats.evictions.saturating_add(1);
        }
        let touched = self.tick();
        self.order.insert((1, touched, key.clone()));
        self.dependencies
            .entry(dependency)
            .or_default()
            .insert(key.clone());
        self.stats.used_bytes = self.stats.used_bytes.max(BASE_BYTES) + charge;
        self.stats.entries += 1;
        for owner in &owners {
            self.history_owners
                .entry(*owner)
                .or_default()
                .insert(key.clone());
        }
        self.entries.insert(
            key,
            Entry {
                value,
                dependency,
                frequency: 1,
                touched,
                charge,
                owners: owners.into_boxed_slice(),
                web_history,
            },
        );
    }
    fn preserve_frequency(&mut self, key: &[u8], frequency: u64) {
        if let Some(entry) = self.entries.get_mut(key) {
            self.order
                .remove(&(entry.frequency, entry.touched, key.into()));
            entry.frequency = frequency;
            self.order.insert((frequency, entry.touched, key.into()));
        }
    }
}

pub(crate) struct Fill {
    cache: Shared,
    key: Box<[u8]>,
    dependency: Dependency,
    generation: u64,
}
impl Fill {
    pub(crate) fn finish_web_history(self, value: &[u8], watch: crate::webapi::Watch) {
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if cache.generation == self.generation {
            cache.insert_watched(self.key, value.into(), None, Some(Box::new(watch)));
        }
    }
    pub(crate) fn at_height(mut self, height: u32) -> Self {
        self.dependency = Some(height);
        self
    }
    pub(crate) fn finish_json(self, value: &[u8]) {
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if cache.stats.limit_bytes < BASE_BYTES + ENTRY_BYTES {
            return;
        }
        if cache.generation == self.generation {
            cache.insert(self.key, value.into(), self.dependency);
        }
    }
}

/// Fully rendered public REST JSON uses one bounded cache and publication epoch.
/// Hits never acquire the node mutex; internal RPC methods bypass this cache.
pub(crate) struct WebProbe {
    pub ready: bool,
    pub since: u32,
    pub value: Option<Vec<u8>>,
    pub fill: Option<Fill>,
    pub epoch: u64,
}
fn web_ready(state: &Cache) -> bool {
    state.synced_since.is_some_and(|since| {
        state
            .tip_height
            .is_some_and(|height| height.saturating_sub(since) >= state.sync_delay)
    })
}
pub(crate) fn web_probe(cache: &Shared, key: Option<&[u8]>) -> WebProbe {
    let mut state = cache.lock().unwrap_or_else(|p| p.into_inner());
    let mut probe = WebProbe {
        ready: web_ready(&state),
        since: state.synced_since.unwrap_or(0),
        value: None,
        fill: None,
        epoch: state.generation,
    };
    if !probe.ready || state.publishing || state.stats.limit_bytes < BASE_BYTES + ENTRY_BYTES {
        return probe;
    }
    if let Some(key) = key.filter(|key| key.len() <= MAX_KEY_BYTES) {
        probe.value = state.get(key);
        if probe.value.is_none() {
            probe.fill = Some(Fill {
                cache: cache.clone(),
                key: key.into(),
                dependency: None,
                generation: state.generation,
            });
        }
    }
    probe
}
pub(crate) fn web_current(cache: &Shared, epoch: u64) -> bool {
    let state = cache.lock().unwrap_or_else(|p| p.into_inner());
    state.generation == epoch && !state.publishing && web_ready(&state)
}
pub(crate) fn web_sync(node: &Node) {
    if let Some(cache) = &node.rpc_cache {
        let mut cache = cache.lock().unwrap_or_else(|p| p.into_inner());
        if node.synced {
            if cache.synced_since.is_none() {
                cache.synced_since = cache.tip_height;
            }
        } else {
            cache.synced_since = None;
        }
    }
}
#[cfg(test)]
pub(crate) fn web_ready_for_test(cache: &Shared) {
    let mut cache = cache.lock().unwrap();
    cache.synced_since = Some(0);
    cache.sync_delay = 0;
}

impl Node {
    /// Public REST cache budget; defaults to 64 MiB. Zero releases cached data.
    /// VNX, JSON-RPC, embedded and peer calls always bypass this layer.
    pub fn set_rpc_cache_limit(&mut self, bytes: usize) {
        let tip = self
            .chain
            .selected()
            .storage()
            .database()
            .version()
            .saturating_sub(1);
        if let Some(shared) = &self.rpc_cache {
            let mut cache = shared.lock().unwrap_or_else(|p| p.into_inner());
            let generation = cache.generation.wrapping_add(1);
            let since = cache.synced_since;
            let delay = cache.sync_delay;
            *cache = Cache::new(bytes, tip);
            cache.generation = generation;
            cache.synced_since = since;
            cache.sync_delay = delay;
        } else {
            self.rpc_cache = Some(Arc::new(Mutex::new(Cache::new(bytes, tip))));
        }
        self.chain
            .set_observer(self.rpc_cache.as_ref().map(|cache| {
                Arc::new(Observer {
                    cache: cache.clone(),
                    params: self.chain.selected().params().clone(),
                }) as Arc<dyn mmx_execution::chain::forks::Observer>
            }));
    }
    pub fn rpc_cache_stats(&self) -> Stats {
        self.rpc_cache
            .as_ref()
            .map(|c| c.lock().unwrap_or_else(|p| p.into_inner()).stats())
            .unwrap_or_default()
    }
}

struct Observer {
    cache: Shared,
    params: mmx_types::mmx::ChainParams,
}
impl mmx_execution::chain::forks::Observer for Observer {
    fn reset(&self) {
        self.cache.lock().unwrap_or_else(|p| p.into_inner()).reset();
    }
    fn selected(&self, common: u32, tip: u32) {
        self.cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .selected(common, tip);
    }
    fn applied(&self, block: &Record) {
        // Notifications contain already-loaded, successfully selected bodies.
        // Cache failure must never affect chain publication.
        if self.push(block).is_err() {
            self.reset();
            self.cache
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .publishing = true;
        }
    }
    fn finished(&self) {
        self.cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .publishing = false;
    }
}
impl Observer {
    fn push(&self, block: &Record) -> Result<()> {
        let height = number(block, "height")?;
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if cache.stats.limit_bytes < BASE_BYTES + ENTRY_BYTES {
            return Ok(());
        }
        // Kept history pages advance from their complete prior response plus
        // this selected block's native activity. A branch replacement discards
        // them in selected(); no old-branch page can be incrementally repaired.
        if block.memory_size() > super::MAX_RESPONSE_BYTES {
            let watched: BTreeSet<_> = cache.history_owners.values().flatten().cloned().collect();
            for key in watched {
                cache.remove(&key);
            }
        }
        if !cache.history_owners.is_empty() {
            let rows = mmx_execution::chain::history::entries(block, &self.params)?;
            let mut watched = BTreeSet::new();
            for row in &rows {
                if let Some(keys) = cache.history_owners.get(&hash(row, "address")?) {
                    watched.extend(keys.iter().cloned());
                }
            }
            for key in watched {
                let Some(entry) = cache.entries.get(&key) else {
                    continue;
                };
                let frequency = entry.frequency;
                if let Some(watch) = entry.web_history.clone() {
                    let result = watch.advance(&entry.value, &rows);
                    match result {
                        Ok(Some(response)) => {
                            cache.remove(&key);
                            cache.insert_watched(
                                key.clone(),
                                response.into_boxed_slice(),
                                None,
                                Some(watch),
                            );
                            cache.preserve_frequency(&key, frequency);
                        }
                        Ok(None) => (),
                        Err(_) => cache.remove(&key),
                    }
                }
            }
        }
        if block.memory_size() <= super::MAX_RESPONSE_BYTES {
            if let Ok(entries) =
                crate::webapi::pushed(block, &self.params, cache.tip_height.unwrap_or(height))
            {
                for entry in entries {
                    cache.insert(
                        entry.key.into_boxed_slice(),
                        entry.value.into_boxed_slice(),
                        entry.height,
                    );
                }
            }
        }
        Ok(())
    }
}
