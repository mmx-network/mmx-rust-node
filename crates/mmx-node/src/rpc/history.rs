//! Native get_history/get_history_memo over a pinned selected ledger view.
use crate::{
    error,
    node::{hash, number, record},
    Node, Result,
};
use mmx_consensus::{Hash, Record, Value};
use mmx_execution::chain::{forks::BlockSnapshot, history as index};
use mmx_types::mmx;
use mmx_vm::StorageDb;
use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{Duration, Instant},
};
#[cfg(test)]
mod tests;

const MAX_ROWS: usize = 10_000;
const MAX_LEGACY_BLOCKS: usize = 1000;
pub(crate) fn is_method(name: &str) -> bool {
    matches!(name, "mmx.Node.get_history" | "mmx.Node.get_history_memo")
}

pub(super) struct Filter {
    owners: BTreeSet<Hash>,
    currencies: BTreeSet<Hash>,
    kind: Option<u64>,
    memo: Option<Vec<u8>>,
    since: u32,
    until: u32,
    limit: i64,
    max_search: usize,
    pending: bool,
}
impl Filter {
    fn parse(request: &Record) -> Result<Self> {
        let filter = request.field("filter")?.record()?;
        let owners = addresses(request.field("addresses")?)?;
        let currencies = addresses(filter.field("currency")?)?;
        let limit = match filter.field("limit")? {
            Value::Int(v) => *v,
            Value::Uint(v) => i64::try_from(*v)?,
            _ => return Err(error("invalid history limit")),
        };
        let memo = if request.name == "mmx.Node.get_history_memo" {
            Some(request.field("memo")?.text()?.to_vec())
        } else {
            filter
                .field("memo")?
                .optional()?
                .map(|v| v.text().map(Vec::from))
                .transpose()?
        };
        if memo.as_ref().is_some_and(|m| m.len() > 64) {
            return Err(error("history memo exceeds 64 bytes"));
        }
        Ok(Self {
            owners,
            currencies,
            memo,
            limit,
            kind: filter
                .field("type")?
                .optional()?
                .map(Value::uint)
                .transpose()?,
            since: number(filter, "since")?,
            until: number(filter, "until")?,
            max_search: usize::try_from(filter.field("max_search")?.uint()?)?,
            pending: filter.field("with_pending")?.boolean()?,
        })
    }
    fn matches(&self, row: &Record) -> Result<bool> {
        Ok(self.owners.contains(&hash(row, "address")?)
            && (self.currencies.is_empty() || self.currencies.contains(&hash(row, "contract")?))
            && self
                .kind
                .is_none_or(|kind| row.field("type").and_then(Value::uint).ok() == Some(kind))
            && match &self.memo {
                Some(memo) => row
                    .field("memo")?
                    .optional()?
                    .is_some_and(|v| v.text().ok() == Some(memo.as_slice())),
                None => true,
            })
    }
}
fn addresses(value: &Value) -> Result<BTreeSet<Hash>> {
    let values = value.seq()?;
    if values.len() > 1024 {
        return Err(error("history query exceeds 1024 addresses/currencies"));
    }
    values.iter().map(|v| Ok(v.bytes()?.try_into()?)).collect()
}
// Database keys sort by length first, then by bytes. Keep range cursors at the
// exact key width; appending a byte would skip every remaining key in a table.
fn successor(mut key: Vec<u8>) -> Option<Vec<u8>> {
    for byte in key.iter_mut().rev() {
        if *byte != u8::MAX {
            *byte += 1;
            return Some(key);
        }
        *byte = 0;
    }
    None
}
pub(super) struct Task {
    storage: StorageDb,
    blocks: Option<BlockSnapshot>,
    params: mmx::ChainParams,
    pending: Vec<Arc<Record>>,
    start: u32,
    floor: u32,
    _permit: super::ReadPermit,
}
impl Task {
    pub(super) fn capture(node: &Node, request: &Record) -> Result<Self> {
        let filter = Filter::parse(request)?;
        let permit = super::ReadPermit::acquire(&node.rpc_workers)?;
        let selected = node.chain.selected();
        let db = selected.storage().database();
        // Old ledgers are read in place. New publications begin an atomic
        // index suffix; bounded fallback covers the earlier retained prefix.
        let start = match db.get("chain_meta", index::START_KEY, None)? {
            Some(raw) => u32::from_be_bytes(raw.as_slice().try_into()?),
            None => db.version(),
        };
        Ok(Self {
            storage: StorageDb::new(db.fork(db.version())?),
            blocks: (filter.since < start)
                .then(|| node.chain.block_snapshot())
                .transpose()?,
            params: selected.params().clone(),
            pending: if filter.pending {
                node.tx_pool
                    .entries
                    .values()
                    .map(|e| e.record.clone())
                    .collect()
            } else {
                Vec::new()
            },
            start,
            floor: node.earliest_height(),
            _permit: permit,
        })
    }
    pub(super) fn call(&self, request: &Record, end: Instant) -> Result<Record> {
        let mut filter = Filter::parse(request)?;
        filter.since = filter.since.max(self.floor);
        let mut rows = Vec::new();
        let mut scanned = 0usize;
        let mut memory = 0usize;
        let mut quota = mmx_db::ReadBudget {
            rows: 100,
            keys: 100_000,
            bytes: super::MAX_RESPONSE_BYTES,
        };
        let mut counts = BTreeMap::<Hash, (usize, u32)>::new();
        let mut push = |row: Record| -> Result<()> {
            memory = memory.saturating_add(row.memory_size());
            if memory > super::MAX_RESPONSE_BYTES || rows.len() == MAX_ROWS {
                return Err(error("history response exceeds row/byte budget"));
            }
            rows.push(row);
            Ok(())
        };
        if filter.limit != 0 && !filter.owners.is_empty() && filter.since <= filter.until {
            for owner in &filter.owners {
                let prefix = index::prefix(owner, filter.memo.as_deref());
                let mut cursor = [
                    prefix.as_slice(),
                    &(!filter.until).to_be_bytes(),
                    &0u32.to_be_bytes(),
                ]
                .concat();
                let lower = filter.since.max(self.start);
                if lower > filter.until {
                    continue;
                }
                let stop = successor(
                    [
                        prefix.as_slice(),
                        &(!lower).to_be_bytes(),
                        &u32::MAX.to_be_bytes(),
                    ]
                    .concat(),
                );
                let table = if filter.memo.is_some() {
                    index::MEMO_TABLE
                } else {
                    index::TABLE
                };
                loop {
                    if Instant::now() >= end {
                        return Err(error("RPC request deadline"));
                    }
                    if filter.max_search > 0 && scanned >= filter.max_search {
                        break;
                    }
                    if scanned >= MAX_ROWS {
                        return Err(error("history query exceeds 10000 searched rows"));
                    }
                    quota.rows = 100.min(MAX_ROWS - scanned);
                    if filter.max_search > 0 {
                        quota.rows = quota.rows.min(filter.max_search - scanned);
                    }
                    let chunk = self.storage.database().scan_range_bounded(
                        table,
                        &cursor,
                        stop.as_deref(),
                        None,
                        &mut quota,
                    )?;
                    if chunk.is_empty() {
                        break;
                    }
                    let chunk_len = chunk.len();
                    let mut done = false;
                    for item in chunk {
                        scanned += 1;
                        let next = successor(item.key);
                        if let Some(next) = next {
                            cursor = next;
                        } else {
                            done = true;
                        }
                        let wire = if filter.memo.is_some() {
                            self.storage
                                .database()
                                .get_bounded(index::TABLE, &item.value, None, &mut quota)?
                                .ok_or_else(|| error("missing history memo row"))?
                        } else {
                            item.value
                        };
                        let row = Record::decode_compact(&wire)?;
                        let height = number(&row, "height")?;
                        let (count, cutoff) = counts.entry(*owner).or_default();
                        if filter.limit > 0 && *count >= filter.limit as usize && height < *cutoff {
                            done = true;
                            break;
                        }
                        if filter.matches(&row)? {
                            *count += 1;
                            *cutoff = height;
                            push(row)?;
                        }
                    }
                    if done || chunk_len < quota.rows {
                        break;
                    }
                }
            }
            if let Some(blocks) = &self.blocks {
                let upper = filter
                    .until
                    .min(self.start.saturating_sub(1))
                    .min(self.storage.database().version().saturating_sub(1));
                if filter.since < self.start && filter.since <= upper {
                    for (visited, height) in (filter.since..=upper).rev().enumerate() {
                        if filter.owners.iter().all(|a| {
                            counts.get(a).is_some_and(|(n, _)| {
                                filter.limit > 0 && *n >= filter.limit as usize
                            })
                        }) {
                            break;
                        }
                        if filter.max_search > 0 && scanned >= filter.max_search {
                            break;
                        }
                        if visited >= MAX_LEGACY_BLOCKS || scanned >= MAX_ROWS {
                            return Err(error(
                                "legacy history query exceeds search budget; narrow since/until",
                            ));
                        }
                        if Instant::now() >= end {
                            return Err(error("RPC request deadline"));
                        }
                        quota.rows = 1;
                        let id = self
                            .storage
                            .database()
                            .get_bounded("chain_height", &height.to_be_bytes(), None, &mut quota)?
                            .ok_or_else(|| error("missing history block height"))?;
                        let block = blocks
                            .block(&id.as_slice().try_into()?)?
                            .ok_or_else(|| error("missing history block"))?;
                        quota.bytes = quota
                            .bytes
                            .checked_sub(block.memory_size())
                            .ok_or_else(|| error("legacy history byte budget"))?;
                        scanned += 1; // Empty blocks still consume search work.
                        for row in index::entries(&block, &self.params)? {
                            if scanned >= MAX_ROWS {
                                return Err(error("history query exceeds 10000 searched rows"));
                            }
                            if filter.max_search > 0 && scanned >= filter.max_search {
                                break;
                            }
                            scanned += 1;
                            let owner = hash(&row, "address")?;
                            let (count, cutoff) = counts.entry(owner).or_default();
                            if filter.limit > 0
                                && *count >= filter.limit as usize
                                && height < *cutoff
                            {
                                continue;
                            }
                            if filter.matches(&row)? {
                                *count += 1;
                                *cutoff = height;
                                push(row)?;
                            }
                        }
                    }
                }
            }
        }
        if filter.limit != 0 && filter.pending {
            for tx in &self.pending {
                if Instant::now() >= end {
                    return Err(error("RPC request deadline"));
                }
                for row in index::transaction_entries(tx, u32::MAX, Value::Int(0), true)? {
                    if filter.memo.is_some()
                        && row.field("type")?.uint()? != u64::from(mmx::tx_type_e::RECEIVE.value)
                    {
                        continue;
                    }
                    if filter.matches(&row)? {
                        push(row)?;
                    }
                }
            }
        }
        let rows = finish(rows, &filter)?;
        Ok(record(
            &format!("{}.return", request.name),
            [(
                "_ret_0".into(),
                Value::Sequence(rows.into_iter().map(Value::Record).collect()),
            )],
        ))
    }
}
type SortKey = (u32, Hash, u64, Hash, Hash, Option<Vec<u8>>, u128);
fn sort_key(row: &Record) -> Result<SortKey> {
    Ok((
        number(row, "height")?,
        hash(row, "txid")?,
        row.field("type")?.uint()?,
        hash(row, "contract")?,
        hash(row, "address")?,
        row.field("memo")?
            .optional()?
            .map(|v| v.text().map(Vec::from))
            .transpose()?,
        u128::from_le_bytes(row.field("amount")?.bytes()?.try_into()?),
    ))
}
fn finish(rows: Vec<Record>, filter: &Filter) -> Result<Vec<Record>> {
    let mut keyed = rows
        .into_iter()
        .map(|row| Ok((Reverse(sort_key(&row)?), row)))
        .collect::<Result<Vec<_>>>()?;
    keyed.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let mut rows: Vec<_> = keyed.into_iter().map(|(_, row)| row).collect();
    if filter.limit > 0 && rows.len() > filter.limit as usize {
        let cutoff = number(&rows[filter.limit as usize - 1], "height")?;
        let keep = rows
            .iter()
            .take_while(|r| number(r, "height").is_ok_and(|h| h >= cutoff))
            .count();
        rows.truncate(keep);
    }
    Ok(rows)
}
pub(crate) fn can_advance(request: &Record) -> Result<bool> {
    if !is_method(&request.name) {
        return Ok(false);
    }
    let filter = Filter::parse(request)?;
    Ok(!filter.pending
        && (filter.max_search == 0
            || (filter.owners.len() == 1
                && filter.currencies.is_empty()
                && filter.kind.is_none()
                && filter.memo.is_none()
                && filter.limit >= 0
                && filter.limit as usize <= filter.max_search)))
}
pub(crate) fn affected(request: &Record, delta: &[Record]) -> Result<bool> {
    let filter = Filter::parse(request)?;
    if filter.limit == 0 {
        return Ok(false);
    }
    for row in delta {
        let height = number(row, "height")?;
        if height >= filter.since && height <= filter.until && filter.matches(row)? {
            return Ok(true);
        }
    }
    Ok(false)
}
pub(crate) fn advance(
    request: &Record,
    mut response: Record,
    delta: &[Record],
) -> Result<Option<Record>> {
    let filter = Filter::parse(request)?;
    let mut added = Vec::new();
    for row in delta {
        let height = number(row, "height")?;
        if height >= filter.since && height <= filter.until && filter.matches(row)? {
            added.push(row.clone());
        }
    }
    if added.is_empty() || filter.limit == 0 {
        return Ok(None);
    }
    for value in response.field("_ret_0")?.seq()? {
        added.push(value.record()?.clone());
    }
    let rows = finish(added, &filter)?;
    if rows.len() > MAX_ROWS || (filter.max_search > 0 && rows.len() > filter.max_search) {
        return Err(error("history cache exceeds row budget"));
    }
    response.fields.insert(
        "_ret_0".into(),
        Value::Sequence(rows.into_iter().map(Value::Record).collect()),
    );
    if response.memory_size() > super::MAX_RESPONSE_BYTES {
        return Err(error("history cache exceeds byte budget"));
    }
    Ok(Some(response))
}
pub(crate) fn response(node: &Node, request: &Record) -> Result<Record> {
    Task::capture(node, request)?.call(request, Instant::now() + Duration::from_secs(5))
}
