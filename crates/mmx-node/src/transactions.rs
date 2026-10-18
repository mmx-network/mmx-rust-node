//! Pending transactions stay in RAM. Validation reads a pinned native snapshot;
//! only block execution can publish ledger changes.
use crate::{
    error,
    node::{hash, number},
    Node, Result,
};
use mmx_consensus::{Record, Value};
use mmx_execution::transaction::{self, TransactionContext, TransactionResult};
use mmx_vm::{storage::StorageDb, Storage};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

type BalanceRows = BTreeMap<([u8; 32], [u8; 32]), u128>;

const MAX_POOL_BYTES: usize = 64 * 1024 * 1024;
const MAX_POOL_ITEMS: usize = 4096;
const MAX_RELAY_TX_BYTES: usize = 256 * 1024;
#[derive(Clone, Default)]
pub(crate) struct Relay {
    state: Arc<Mutex<RelayState>>,
    latest: Arc<AtomicU64>,
}
#[derive(Default)]
struct RelayState {
    entries: BTreeMap<u64, ([u8; 32], Arc<Record>, usize)>,
    ids: BTreeMap<[u8; 32], u64>,
}
impl Relay {
    fn publish(&self, id: [u8; 32], record: Arc<Record>, size: usize) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| error("transaction relay poisoned"))?;
        if let Some(seq) = state.ids.remove(&id) {
            state.entries.remove(&seq);
        }
        let seq = self
            .latest
            .load(Ordering::Relaxed)
            .checked_add(1)
            .ok_or_else(|| error("transaction relay sequence overflow"))?;
        state.ids.insert(id, seq);
        state.entries.insert(seq, (id, record, size));
        self.latest.store(seq, Ordering::Release);
        Ok(())
    }
    fn remove(&self, id: &[u8; 32]) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| error("transaction relay poisoned"))?;
        if let Some(seq) = state.ids.remove(id) {
            state.entries.remove(&seq);
        }
        Ok(())
    }
    pub fn since(&self, cursor: u64) -> Result<(u64, Vec<Arc<Record>>)> {
        let latest = self.latest.load(Ordering::Acquire);
        if latest == cursor {
            return Ok((cursor, vec![]));
        }
        let state = self
            .state
            .lock()
            .map_err(|_| error("transaction relay poisoned"))?;
        let mut out = Vec::new();
        let mut bytes = 0;
        let mut next = latest;
        for (seq, (_, record, size)) in state.entries.range((
            std::ops::Bound::Excluded(cursor),
            std::ops::Bound::Unbounded,
        )) {
            if !out.is_empty() && (out.len() >= 16 || bytes + size > 256 * 1024) {
                next = seq - 1;
                break;
            }
            bytes += size;
            out.push(record.clone());
        }
        Ok((next, out))
    }
}
pub(crate) struct PoolEntry {
    pub record: Arc<Record>,
    fee: u32,
    size: usize,
}
#[derive(Default)]
pub(crate) struct Pool {
    pub entries: BTreeMap<[u8; 32], PoolEntry>,
    bytes: usize,
    tip: Option<[u8; 32]>,
    pub relay: Relay,
}
impl Pool {
    fn remove(&mut self, id: &[u8; 32]) -> Result<()> {
        if let Some(entry) = self.entries.remove(id) {
            self.bytes -= entry.size;
            self.relay.remove(id)?;
        }
        Ok(())
    }
    pub fn clear(&mut self) -> Result<()> {
        for id in self.entries.keys() {
            self.relay.remove(id)?;
        }
        self.entries.clear();
        self.bytes = 0;
        self.tip = None;
        Ok(())
    }
}
impl Node {
    pub fn validate_transaction(&self, tx: &Record) -> Result<TransactionResult> {
        if tx.name != "mmx.Transaction" {
            return Err(error("expected transaction"));
        }
        if tx.field("exec_result")?.optional()?.is_some() {
            return Err(error("exec_result not null"));
        }
        let selected = self.chain.selected();
        let params = selected.params();
        let height = selected
            .tip()?
            .ok_or_else(|| error("missing tip"))?
            .height
            .checked_add(1)
            .ok_or_else(|| error("height overflow"))?;
        if height < params.transaction_activation {
            return Err(error("transactions not live yet"));
        }
        let db = selected.storage().database();
        let mut snapshot = StorageDb::new(db.fork(db.version())?);
        let context = TransactionContext {
            height,
            params: params.clone(),
            locked_contracts: None,
        };
        Ok(transaction::prepare(&mut snapshot, &context, tx)?
            .result()
            .clone())
    }
    pub fn pending_transactions(&self) -> usize {
        self.tx_pool.entries.len()
    }
    pub(crate) fn transaction_relay(&self) -> Relay {
        self.tx_pool.relay.clone()
    }
    /// Native add_transaction: reset execution receipts before admission;
    /// pre-validation rejects dynamic failures for local wallet submissions.
    pub fn add_transaction(&mut self, tx: &Record, pre_validate: bool) -> Result<bool> {
        if !self.synced {
            return Err(error("not synced"));
        }
        let params = self.chain.selected().params();
        let mut tx = tx.clone();
        if tx.name != "mmx.Transaction" {
            return Err(error("expected transaction"));
        }
        if tx.field("exec_result")?.optional()?.is_some() {
            tx.fields
                .insert("exec_result".into(), Value::Optional(None));
            tx.fields.insert(
                "static_cost".into(),
                Value::Uint(tx.calc_cost(&params.into())?),
            );
            tx.fields.insert(
                "content_hash".into(),
                Value::Bytes(tx.calc_hash(true, 0)?.to_vec()),
            );
        }
        let size = tx.encode_compact()?.len();
        if size > params.max_block_size as usize || size > MAX_RELAY_TX_BYTES {
            return Err(error("transaction exceeds pool size limit"));
        }
        let id = hash(&tx, "id")?;
        if self
            .tx_pool
            .entries
            .get(&id)
            .is_some_and(|e| e.record.as_ref() == &tx)
        {
            return Ok(false);
        }
        let current = self
            .chain
            .selected()
            .tip()?
            .ok_or_else(|| error("missing tip"))?
            .height;
        if number(&tx, "expires")? < current.saturating_add(1) {
            return Err(error("transaction expired"));
        }
        let result = self.validate_transaction(&tx)?;
        if pre_validate && result.did_fail {
            return Err(error("transaction execution would fail"));
        }
        let sender = tx
            .field("sender")?
            .optional()?
            .ok_or_else(|| error("missing sender"))?
            .bytes()?;
        let sender: [u8; 32] = sender.try_into()?;
        let mut fees = u64::from(result.total_fee);
        for (other_id, entry) in &self.tx_pool.entries {
            if *other_id != id
                && entry
                    .record
                    .field("sender")?
                    .optional()?
                    .is_some_and(|v| v.bytes().is_ok_and(|a| a == sender))
            {
                fees = fees
                    .checked_add(entry.fee.into())
                    .ok_or_else(|| error("pending fee overflow"))?;
            }
        }
        if u128::from(fees)
            > self
                .chain
                .selected()
                .storage()
                .get_balance(&sender, &[0; 32])?
                .unwrap_or(0)
        {
            return Err(error("insufficient funds for pending transaction fees"));
        }
        let old = self.tx_pool.entries.get(&id).map_or(0, |e| e.size);
        let bytes = self.tx_pool.bytes - old + size;
        if bytes > MAX_POOL_BYTES || (self.tx_pool.entries.len() >= MAX_POOL_ITEMS && old == 0) {
            return Err(error("transaction pool full"));
        }
        let tx = Arc::new(tx);
        self.tx_pool.relay.publish(id, tx.clone(), size)?;
        self.tx_pool.entries.insert(
            id,
            PoolEntry {
                record: tx,
                fee: result.total_fee,
                size,
            },
        );
        self.tx_pool.bytes = bytes;
        Ok(true)
    }
    pub(crate) fn prune_transactions(&mut self) -> Result<()> {
        if self.tx_pool.entries.is_empty() {
            return Ok(());
        }
        let tip = self
            .chain
            .selected()
            .tip()?
            .ok_or_else(|| error("missing tip"))?;
        if self.tx_pool.tip == Some(tip.hash.0) {
            return Ok(());
        }
        let mut remove = BTreeSet::new();
        for (id, e) in &self.tx_pool.entries {
            if number(&e.record, "expires")? < tip.height.saturating_add(1)
                || self
                    .chain
                    .selected()
                    .storage()
                    .get_transaction_index(id)?
                    .is_some()
            {
                remove.insert(*id);
            }
        }
        for id in remove {
            self.tx_pool.remove(&id)?;
        }
        self.tx_pool.tip = Some(tip.hash.0);
        Ok(())
    }
}

impl Node {
    /// Native balance RPCs deduplicate owners. A whitelist uses point reads;
    /// otherwise scan only the selected address prefix, never the whole table.
    pub(crate) fn all_balances(&self, request: &Record) -> Result<BalanceRows> {
        let owners = request
            .field("addresses")?
            .seq()?
            .iter()
            .map(|v| Ok(v.bytes()?.try_into()?))
            .collect::<Result<BTreeSet<[u8; 32]>>>()?;
        let whitelist = request
            .field("whitelist")?
            .seq()?
            .iter()
            .map(|v| Ok(v.bytes()?.try_into()?))
            .collect::<Result<BTreeSet<[u8; 32]>>>()?;
        if owners.len() > 1024 || whitelist.len() > 1024 {
            return Err(error("balance query exceeds 1024 owners/currencies"));
        }
        let limit = match request.field("limit")? {
            Value::Int(v) => *v,
            Value::Uint(v) => i64::try_from(*v)?,
            _ => return Err(error("invalid balance limit")),
        };
        let mut out = BTreeMap::new();
        let storage = self.chain.selected().storage();
        for owner in owners {
            if whitelist.is_empty() {
                let rows = storage.database().scan("balances", &owner, None)?;
                for row in rows.into_iter().take(if limit < 0 {
                    usize::MAX
                } else {
                    limit as usize
                }) {
                    if row.key.len() != 64 {
                        return Err(error("invalid balance key length"));
                    }
                    {
                        let amount = u128::from_le_bytes(row.value.as_slice().try_into()?);
                        if amount > 0 {
                            out.insert((owner, row.key[32..].try_into()?), amount);
                        }
                    }
                }
            } else {
                for currency in &whitelist {
                    let amount = storage.get_balance(&owner, currency)?.unwrap_or(0);
                    if amount > 0 {
                        out.insert((owner, *currency), amount);
                    }
                }
            }
            if out.len() > 10000 {
                return Err(error("balance response exceeds 10000 rows"));
            }
        }
        Ok(out)
    }
}

#[derive(Default)]
pub(crate) struct RelayPeer {
    cursor: u64,
    seen: BTreeSet<[u8; 32]>,
    order: std::collections::VecDeque<[u8; 32]>,
}
impl RelayPeer {
    pub fn note(&mut self, record: &Record) {
        if record.name != "mmx.Transaction" {
            return;
        }
        if let Ok(id) = hash(record, "content_hash") {
            if self.seen.insert(id) {
                self.order.push_back(id);
                if self.order.len() > 4096 {
                    self.seen.remove(&self.order.pop_front().unwrap());
                }
            }
        }
    }
    pub fn flush(
        &mut self,
        relay: &Relay,
        mut send: impl FnMut(&Record) -> Result<()>,
    ) -> Result<()> {
        let (next, records) = relay.since(self.cursor)?;
        for r in records {
            if !self.seen.contains(&hash(&r, "content_hash")?) {
                send(&r)?;
                self.note(&r);
            }
        }
        self.cursor = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::record;

    #[test]
    fn relay_deduplicates_echoes_and_retries_failed_sends_without_copying_bodies() {
        let relay = Relay::default();
        let tx = Arc::new(record(
            "mmx.Transaction",
            [("content_hash".into(), Value::Bytes(vec![1; 32]))],
        ));
        relay.publish([2; 32], tx.clone(), 100).unwrap();
        assert!(Arc::ptr_eq(&relay.since(0).unwrap().1[0], &tx));
        let mut peer = RelayPeer::default();
        assert!(peer.flush(&relay, |_| Err(error("disconnected"))).is_err());
        let mut count = 0;
        peer.flush(&relay, |_| {
            count += 1;
            Ok(())
        })
        .unwrap();
        peer.flush(&relay, |_| {
            count += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 1);
        let mut echo = RelayPeer::default();
        echo.note(&tx);
        echo.flush(&relay, |_| panic!("must not echo incoming content"))
            .unwrap();
        relay.remove(&[2; 32]).unwrap();
        assert!(relay.since(0).unwrap().1.is_empty());
    }
}
