//! Response cache shared by public HTTP and VNX dispatch. Selected-chain events
//! push already-loaded blocks; hits never query the ledger or retain a DB view.
use crate::{
    node::{hash, number},
    Node, Result,
};
use mmx_consensus::{Record, Value};
use mmx_vm::Storage;
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

#[derive(Clone, Copy, Debug, Default)]
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
    advance_history: bool,
}
type Rank = (u64, u64, Box<[u8]>);
pub(crate) struct Cache {
    entries: BTreeMap<Box<[u8]>, Entry>,
    order: BTreeSet<Rank>,
    dependencies: BTreeMap<Dependency, BTreeSet<Box<[u8]>>>,
    stats: Stats,
    clock: u64,
    generation: u64,
    tip_height: Option<u32>,
}
impl Cache {
    pub(super) fn new(limit_bytes: usize, tip_height: u32) -> Self {
        Self {
            entries: BTreeMap::new(),
            order: BTreeSet::new(),
            dependencies: BTreeMap::new(),
            stats: Stats {
                limit_bytes,
                ..Default::default()
            },
            clock: 0,
            generation: 0,
            tip_height: Some(tip_height),
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
        self.stats.used_bytes = 0;
        self.stats.entries = 0;
        self.generation = self.generation.wrapping_add(1);
        self.tip_height = None;
    }
    fn selected(&mut self, common: u32, tip: u32) {
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
            if extension && self.entries.get(&key).is_some_and(|e| e.advance_history) {
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
    fn insert(
        &mut self,
        key: Box<[u8]>,
        value: Box<[u8]>,
        dependency: Dependency,
        advance_history: bool,
    ) {
        if value.len() > super::MAX_RESPONSE_BYTES {
            return;
        }
        let charge = ENTRY_BYTES
            .saturating_add(key.len().saturating_mul(3))
            .saturating_add(value.len());
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
        self.entries.insert(
            key,
            Entry {
                value,
                dependency,
                frequency: 1,
                touched,
                charge,
                advance_history,
            },
        );
    }
}

fn dependency(node: &Node, request: &Record) -> Result<Option<Dependency>> {
    let selected = node.chain.selected();
    let at = |height| -> Result<_> {
        Ok(if height < node.earliest_height() {
            None
        } else {
            selected.hash_at(height)?.map(|_| Some(height))
        })
    };
    Ok(match request.name.as_str() {
        "mmx.Node.get_block_at"
        | "mmx.Node.get_header_at"
        | "mmx.Node.get_block_hash"
        | "mmx.Node.get_block_hash_ex"
        | "mmx.Node.get_tx_ids_at" => at(number(request, "height")?)?,
        "mmx.Node.get_block" | "mmx.Node.get_header" => {
            let id = hash(request, "hash")?;
            // Orphan/staged bodies may still change as admission completes.
            match node.chain.header(&id)? {
                Some(header) => {
                    let height = number(&header, "height")?;
                    if selected.hash_at(height)? == Some(id) {
                        at(height)?
                    } else {
                        None
                    }
                }
                None => None,
            }
        }
        "mmx.Node.get_transaction" if request.field("pending")?.boolean()? => None,
        "mmx.Node.get_transaction" | "mmx.Node.get_tx_height" => {
            match selected
                .storage()
                .get_transaction_index(&hash(request, "id")?)?
            {
                Some(index) => at(index.height)?,
                None => None,
            }
        }
        "mmx.Node.get_transactions"
        | "mmx.Node.get_contract"
        | "mmx.Node.get_contract_for"
        | "mmx.Node.get_balance"
        | "mmx.Node.get_balances"
        | "mmx.Node.get_all_balances"
        | "mmx.Node.get_total_balance"
        | "mmx.Node.get_total_balances" => Some(None),
        "mmx.Node.get_history" | "mmx.Node.get_history_memo"
            if !request
                .field("filter")?
                .record()?
                .field("with_pending")?
                .boolean()? =>
        {
            Some(None)
        }
        _ => None,
    })
}
pub(super) struct Fill {
    cache: Shared,
    key: Box<[u8]>,
    dependency: Dependency,
    generation: u64,
    advance_history: bool,
}
pub(super) fn probe(node: &Node, request: &Record) -> Result<(Option<Vec<u8>>, Option<Fill>)> {
    let Some(cache) = &node.rpc_cache else {
        return Ok((None, None));
    };
    // Cheaply reject validation, submission, status and pending requests before
    // encoding potentially large transaction arguments.
    if !matches!(
        request.name.as_str(),
        "mmx.Node.get_block"
            | "mmx.Node.get_block_at"
            | "mmx.Node.get_header"
            | "mmx.Node.get_header_at"
            | "mmx.Node.get_block_hash"
            | "mmx.Node.get_block_hash_ex"
            | "mmx.Node.get_tx_ids_at"
            | "mmx.Node.get_transaction"
            | "mmx.Node.get_transactions"
            | "mmx.Node.get_tx_height"
            | "mmx.Node.get_contract"
            | "mmx.Node.get_contract_for"
            | "mmx.Node.get_balance"
            | "mmx.Node.get_balances"
            | "mmx.Node.get_all_balances"
            | "mmx.Node.get_total_balance"
            | "mmx.Node.get_total_balances"
            | "mmx.Node.get_history"
            | "mmx.Node.get_history_memo"
    ) || request.memory_size() > MAX_KEY_BYTES
    {
        return Ok((None, None));
    }
    let key = request.encode_compact()?.into_boxed_slice();
    if key.len() > MAX_KEY_BYTES {
        return Ok((None, None));
    }
    let (value, generation) = {
        let mut cache = cache.lock().unwrap_or_else(|p| p.into_inner());
        (cache.get(&key), cache.generation)
    };
    if let Some(value) = value {
        return Ok((Some(value), None));
    }
    if request.name == "mmx.Node.get_transactions" {
        let ids = request.field("ids")?.seq()?;
        if ids.len() > 128 {
            return Err(crate::error("transaction query exceeds 128 ids"));
        }
        let mut values = Vec::new();
        let mut size = 0usize;
        let mut complete = true;
        for id in ids {
            let single = crate::node::record(
                "mmx.Node.get_transaction",
                [
                    ("id".into(), id.clone()),
                    ("pending".into(), Value::Bool(false)),
                ],
            )
            .encode_compact()?;
            let wire = cache.lock().unwrap_or_else(|p| p.into_inner()).get(&single);
            let Some(wire) = wire else {
                complete = false;
                break;
            };
            let response = Record::decode(&wire)?;
            let tx = response.field("_ret_0")?.record()?;
            size = size.saturating_add(tx.memory_size());
            if size > super::MAX_RESPONSE_BYTES {
                return Err(crate::error("transaction query exceeds 32 MiB"));
            }
            values.push(Value::Record(tx.clone()));
        }
        if complete {
            let response = crate::node::record(
                "mmx.Node.get_transactions.return",
                [("_ret_0".into(), Value::Sequence(values))],
            );
            if response.memory_size() > super::MAX_RESPONSE_BYTES {
                return Err(crate::error("RPC response exceeds byte budget"));
            }
            let response = response.encode()?;
            cache.lock().unwrap_or_else(|p| p.into_inner()).insert(
                key,
                response.clone().into_boxed_slice(),
                None,
                false,
            );
            return Ok((Some(response), None));
        }
    }
    let advance_history = super::history::can_advance(request)?;
    let fill = dependency(node, request)?.map(|dependency| Fill {
        cache: cache.clone(),
        key,
        dependency,
        generation,
        advance_history,
    });
    Ok((None, fill))
}
impl Fill {
    #[cfg(test)]
    pub(super) fn finish(self, response: &Record) -> Result<()> {
        self.finish_wire(&response.encode()?);
        Ok(())
    }
    pub(super) fn finish_wire(self, value: &[u8]) {
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if cache.stats.limit_bytes < BASE_BYTES + ENTRY_BYTES {
            return;
        }
        if cache.generation == self.generation {
            cache.insert(
                self.key,
                value.into(),
                self.dependency,
                self.advance_history,
            );
        }
    }
}

impl Node {
    /// Public RPC cache budget; defaults to 64 MiB. Zero releases cached data.
    /// Embedded and peer calls through `Node::call` always bypass this layer.
    pub fn set_rpc_cache_limit(&mut self, bytes: usize) {
        self.rpc_cache = (bytes > 0).then(|| {
            Arc::new(Mutex::new(Cache::new(
                bytes,
                self.chain
                    .selected()
                    .storage()
                    .database()
                    .version()
                    .saturating_sub(1),
            )))
        });
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
        }
    }
}
impl Observer {
    fn push(&self, block: &Record) -> Result<()> {
        let height = number(block, "height")?;
        let id = hash(block, "hash")?;
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if cache.stats.limit_bytes < BASE_BYTES + ENTRY_BYTES {
            return Ok(());
        }
        // Kept history pages advance from their complete prior response plus
        // this selected block's native activity. A branch replacement discards
        // them in selected(); no old-branch page can be incrementally repaired.
        let watched: Vec<_> = cache
            .dependencies
            .get(&None)
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        if !watched.is_empty() {
            let rows = mmx_execution::chain::history::entries(block, &self.params)?;
            for key in watched {
                let Some(entry) = cache.entries.get(&key) else {
                    continue;
                };
                let frequency = entry.frequency;
                let result = (|| -> Result<_> {
                    let request = Record::decode_compact(&key)?;
                    if !super::history::affected(&request, &rows)? {
                        return Ok(None);
                    }
                    let response = Record::decode(&entry.value)?;
                    super::history::advance(&request, response, &rows)
                })();
                match result {
                    Ok(Some(response)) => {
                        cache.remove(&key);
                        cache.insert(
                            key.clone(),
                            response.encode()?.into_boxed_slice(),
                            None,
                            true,
                        );
                        if let Some(entry) = cache.entries.get_mut(&key) {
                            let old = (entry.frequency, entry.touched, key.clone());
                            entry.frequency = frequency;
                            let new = (entry.frequency, entry.touched, key.clone());
                            cache.order.remove(&old);
                            cache.order.insert(new);
                        }
                    }
                    Ok(None) => (),
                    Err(_) => cache.remove(&key),
                }
            }
        }
        let mut put = |method: &str, args: Vec<(&str, Value)>, value: Value| -> Result<()> {
            let request = crate::node::record(
                &format!("mmx.Node.{method}"),
                args.into_iter().map(|(k, v)| (k.into(), v)),
            );
            let response = crate::node::record(
                &format!("mmx.Node.{method}.return"),
                [("_ret_0".into(), value)],
            );
            if response.memory_size() > super::MAX_RESPONSE_BYTES {
                return Ok(());
            }
            cache.insert(
                request.encode_compact()?.into_boxed_slice(),
                response.encode()?.into_boxed_slice(),
                Some(height),
                false,
            );
            Ok(())
        };
        let h = || Value::Uint(height.into());
        let b = |v: &[u8]| Value::Bytes(v.to_vec());
        // The ordinary miss path still enforces the decoded response budget.
        // Do not admit a pushed body that the RPC would otherwise reject.
        if block.memory_size() <= super::MAX_RESPONSE_BYTES {
            put(
                "get_block_at",
                vec![("height", h())],
                Value::Record(block.clone()),
            )?;
            put(
                "get_block",
                vec![("hash", b(&id))],
                Value::Record(block.clone()),
            )?;
        }
        let mut header = block.clone();
        header.name = "mmx.BlockHeader".into();
        header.fields.remove("tx_list");
        put(
            "get_header_at",
            vec![("height", h())],
            Value::Record(header.clone()),
        )?;
        put("get_header", vec![("hash", b(&id))], Value::Record(header))?;
        put(
            "get_block_hash",
            vec![("height", h())],
            Value::Optional(Some(Box::new(b(&id)))),
        )?;
        put(
            "get_block_hash_ex",
            vec![("height", h())],
            Value::Optional(Some(Box::new(Value::Sequence(vec![
                b(&id),
                b(&hash(block, "content_hash")?),
            ])))),
        )?;
        let mut ids = Vec::new();
        for tx in block.field("tx_list")?.seq()? {
            let tx = tx.record()?;
            let id = hash(tx, "id")?;
            ids.push(b(&id));
            put(
                "get_transaction",
                vec![("id", b(&id)), ("pending", Value::Bool(false))],
                Value::Record(tx.clone()),
            )?;
            put(
                "get_tx_height",
                vec![("id", b(&id))],
                Value::Optional(Some(Box::new(h()))),
            )?;
        }
        put("get_tx_ids_at", vec![("height", h())], Value::Sequence(ids))?;
        Ok(())
    }
}
