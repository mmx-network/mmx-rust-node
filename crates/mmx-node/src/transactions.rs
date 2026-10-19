//! Pending transactions stay in RAM. Validation reads a pinned native snapshot;
//! only block execution can publish ledger changes.
use crate::{
    error,
    node::{bytes, hash, number, record},
    Node, Result,
};
use mmx_consensus::{Record, Value};
use mmx_execution::transaction::{self, TransactionContext, TransactionResult};
use mmx_vm::{storage::StorageDb, Storage};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
};

type BalanceRows = BTreeMap<([u8; 32], [u8; 32]), u128>;
type RelayEntries = BTreeMap<u64, ([u8; 32], Arc<Record>, usize)>;

const MAX_POOL_BYTES: usize = 64 * 1024 * 1024;
const MAX_POOL_ITEMS: usize = 4096;
const MAX_RELAY_TX_BYTES: usize = 256 * 1024;
const MAX_VERIFIED_IDS: usize = 100_000;
#[derive(Clone, Default)]
pub(crate) struct Relay {
    state: Arc<Mutex<RelayState>>,
    latest: Arc<AtomicU64>,
    wake: Arc<(Mutex<()>, Condvar)>,
}
#[derive(Default)]
struct RelayState {
    entries: [RelayEntries; 2],
    ids: BTreeMap<[u8; 32], (usize, u64)>,
    verified: Seen,
}
impl Relay {
    pub(crate) fn publish(&self, id: [u8; 32], record: Arc<Record>, size: usize) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| error("transaction relay poisoned"))?;
        state
            .verified
            .remember(hash(&record, "content_hash")?, MAX_VERIFIED_IDS);
        if let Some((class, seq)) = state.ids.remove(&id) {
            state.entries[class].remove(&seq);
        }
        let seq = self
            .latest
            .load(Ordering::Relaxed)
            .checked_add(1)
            .ok_or_else(|| error("transaction relay sequence overflow"))?;
        let class = usize::from(record.name == "mmx.Transaction");
        state.ids.insert(id, (class, seq));
        state.entries[class].insert(seq, (id, record, size));
        self.latest.store(seq, Ordering::Release);
        drop(state);
        let _guard = self
            .wake
            .0
            .lock()
            .map_err(|_| error("relay notification poisoned"))?;
        self.wake.1.notify_all();
        Ok(())
    }
    /// Only ledger/proof-verified publications seed this cache. A claimed hash
    /// or ReceiveNote cannot poison another peer's validation/admission path.
    pub(crate) fn knows(&self, record: &Record) -> Result<bool> {
        let Ok(id) = hash(record, "content_hash") else {
            return Ok(false);
        };
        Ok(self
            .state
            .lock()
            .map_err(|_| error("transaction relay poisoned"))?
            .verified
            .ids
            .contains(&id))
    }
    pub(crate) fn reset_verified(&self) -> Result<()> {
        self.state
            .lock()
            .map_err(|_| error("transaction relay poisoned"))?
            .verified = Seen::default();
        Ok(())
    }
    pub(crate) fn remove(&self, id: &[u8; 32]) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| error("transaction relay poisoned"))?;
        if let Some((class, seq)) = state.ids.remove(id) {
            state.entries[class].remove(&seq);
        }
        Ok(())
    }
    pub fn since(&self, cursor: u64, class: usize) -> Result<(u64, Vec<Arc<Record>>)> {
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
        // Publish updates the sequence while holding this mutex.
        let mut next = self.latest.load(Ordering::Acquire);
        for (seq, (_, record, size)) in state.entries[class].range((
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
    pub(crate) fn sequence(&self) -> u64 {
        self.latest.load(Ordering::Acquire)
    }
    pub(crate) fn wait(&self, observed: u64) -> Result<()> {
        let guard = self
            .wake
            .0
            .lock()
            .map_err(|_| error("relay notification poisoned"))?;
        if self.sequence() == observed {
            drop(
                self.wake
                    .1
                    .wait_timeout(guard, std::time::Duration::from_millis(20))
                    .map_err(|_| error("relay notification poisoned"))?,
            );
        }
        Ok(())
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
        validate_snapshot(&mut snapshot, &context, tx)
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
        if tx.name != "mmx.Transaction" {
            return Err(error("expected transaction"));
        }
        let supplied_id = hash(tx, "id")?;
        if !pre_validate
            && self
                .tx_pool
                .entries
                .get(&supplied_id)
                .is_some_and(|e| e.record.as_ref() == tx)
        {
            return Ok(false);
        }
        let params = self.chain.selected().params();
        let mut tx = tx.clone();
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
        let duplicate = self
            .tx_pool
            .entries
            .get(&id)
            .is_some_and(|e| e.record.as_ref() == &tx);
        if duplicate && !pre_validate {
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
        if duplicate {
            return Ok(false);
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

pub(crate) fn is_balance_method(name: &str) -> bool {
    matches!(
        name,
        "mmx.Node.get_balance"
            | "mmx.Node.get_balances"
            | "mmx.Node.get_all_balances"
            | "mmx.Node.get_total_balance"
            | "mmx.Node.get_total_balances"
    )
}
fn quota() -> mmx_db::ReadBudget {
    mmx_db::ReadBudget {
        rows: 1,
        keys: 100_000,
        bytes: 32 * 1024 * 1024,
    }
}
fn read_balance(
    storage: &StorageDb,
    owner: &[u8; 32],
    currency: &[u8; 32],
    budget: &mut mmx_db::ReadBudget,
) -> Result<u128> {
    let key = [owner.as_slice(), currency.as_slice()].concat();
    storage
        .database()
        .get_bounded("balances", &key, None, budget)?
        .map(|value| Ok(u128::from_le_bytes(value.as_slice().try_into()?)))
        .transpose()
        .map(|value| value.unwrap_or(0))
}
fn all_balances(storage: &StorageDb, request: &Record) -> Result<BalanceRows> {
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
    if owners.len().saturating_mul(whitelist.len()) > 10_000 {
        return Err(error("balance query exceeds 10000 point reads"));
    }
    let limit = match request.field("limit")? {
        Value::Int(v) => *v,
        Value::Uint(v) => i64::try_from(*v)?,
        _ => return Err(error("invalid balance limit")),
    };
    let mut out = BTreeMap::new();
    let mut quota = quota();
    let mut scanned = 0usize;
    for owner in owners {
        if whitelist.is_empty() {
            quota.rows = if limit < 0 {
                usize::MAX
            } else {
                limit as usize
            }
            .min(10_001 - scanned);
            let start = [owner.as_slice(), &[0; 32]].concat();
            // Native find_range is end-exclusive, including its all-ones sentinel.
            let end = [owner.as_slice(), &[255; 32]].concat();
            let rows = storage.database().scan_range_bounded(
                "balances",
                &start,
                Some(&end),
                None,
                &mut quota,
            )?;
            scanned += rows.len();
            if scanned > 10_000 {
                return Err(error("balance query exceeds 10000 scanned rows"));
            }
            for row in rows {
                if row.key.len() != 64 || row.key[..32] != owner {
                    return Err(error("invalid balance key"));
                }
                let amount = u128::from_le_bytes(row.value.as_slice().try_into()?);
                if amount > 0 {
                    out.insert((owner, row.key[32..].try_into()?), amount);
                }
            }
        } else {
            quota.rows = 1;
            for currency in &whitelist {
                let amount = read_balance(storage, &owner, currency, &mut quota)?;
                if amount > 0 {
                    out.insert((owner, *currency), amount);
                }
            }
        }
    }
    Ok(out)
}
pub(crate) fn balance_response(storage: &StorageDb, request: &Record) -> Result<Record> {
    let value = if request.name == "mmx.Node.get_balance" {
        bytes(
            read_balance(
                storage,
                &hash(request, "address")?,
                &hash(request, "currency")?,
                &mut quota(),
            )?
            .to_le_bytes(),
        )
    } else {
        let mut query = request.clone();
        if request.name == "mmx.Node.get_balances" {
            query.fields.insert(
                "addresses".into(),
                Value::Sequence(vec![request.field("address")?.clone()]),
            );
        }
        if request.name == "mmx.Node.get_total_balance" {
            query.fields.insert(
                "whitelist".into(),
                Value::Sequence(vec![request.field("currency")?.clone()]),
            );
            query.fields.insert("limit".into(), Value::Int(-1));
        }
        let values = all_balances(storage, &query)?;
        if request.name == "mmx.Node.get_all_balances" {
            Value::Map(
                values
                    .into_iter()
                    .map(|((a, c), n)| {
                        (
                            Value::Sequence(vec![bytes(a), bytes(c)]),
                            bytes(n.to_le_bytes()),
                        )
                    })
                    .collect(),
            )
        } else {
            let mut totals = BTreeMap::<[u8; 32], u128>::new();
            for ((_, currency), amount) in values {
                let value = totals.entry(currency).or_default();
                *value = value
                    .checked_add(amount)
                    .ok_or_else(|| error("balance total overflow"))?;
            }
            if request.name == "mmx.Node.get_total_balance" {
                bytes(
                    totals
                        .get(&hash(request, "currency")?)
                        .copied()
                        .unwrap_or(0)
                        .to_le_bytes(),
                )
            } else {
                Value::Map(
                    totals
                        .into_iter()
                        .map(|(c, n)| (bytes(c), bytes(n.to_le_bytes())))
                        .collect(),
                )
            }
        }
    };
    Ok(record(
        &format!("{}.return", request.name),
        [("_ret_0".into(), value)],
    ))
}
pub(crate) fn validate_snapshot(
    storage: &mut StorageDb,
    context: &TransactionContext,
    tx: &Record,
) -> Result<TransactionResult> {
    if tx.name != "mmx.Transaction" {
        return Err(error("expected transaction"));
    }
    if tx.field("exec_result")?.optional()?.is_some() {
        return Err(error("exec_result not null"));
    }
    if context.height < context.params.transaction_activation {
        return Err(error("transactions not live yet"));
    }
    Ok(transaction::prepare(storage, context, tx)?.result().clone())
}

#[derive(Clone, Default)]
pub(crate) struct RelayPeer {
    cursors: [u64; 2],
    received: Arc<Mutex<Seen>>,
}
#[derive(Default)]
struct Seen {
    ids: BTreeSet<[u8; 32]>,
    order: std::collections::VecDeque<[u8; 32]>,
}
impl Seen {
    fn remember(&mut self, id: [u8; 32], limit: usize) {
        if self.ids.insert(id) {
            self.order.push_back(id);
            if self.order.len() > limit {
                let old = self.order.pop_front().unwrap();
                self.ids.remove(&old);
            }
        }
    }
}
impl RelayPeer {
    pub fn note(&self, record: &Record) -> Result<()> {
        let id = if record.name == "mmx.ReceiveNote" {
            Some(hash(record, "hash")?)
        } else if matches!(
            record.name.as_str(),
            "mmx.Transaction"
                | "mmx.Block"
                | "mmx.ValidatorVote"
                | "mmx.ProofResponse"
                | "mmx.ProofOfTime"
        ) {
            hash(record, "content_hash").ok()
        } else {
            None
        };
        if let Some(id) = id {
            let mut seen = self
                .received
                .lock()
                .map_err(|_| error("peer relay cache poisoned"))?;
            seen.remember(id, 20_000);
        }
        Ok(())
    }
    pub fn flush(
        &mut self,
        relay: &Relay,
        mut send: impl FnMut(&Record) -> Result<()>,
    ) -> Result<()> {
        // Independent cursors keep a transaction backlog from hiding a newly
        // completed proof, VDF or block. No cache lock is held across socket I/O.
        for class in 0..2 {
            let (next, records) = relay.since(self.cursors[class], class)?;
            for r in records {
                let id = hash(&r, "content_hash")?;
                // Only verified relay publications generate feedback. A note
                // acknowledges receipt, not consensus acceptance by the peer.
                send(&record(
                    "mmx.ReceiveNote",
                    [
                        ("hash".into(), bytes(id)),
                        ("time".into(), Value::Int(vnx::time::now_micros())),
                    ],
                ))?;
                let known = self
                    .received
                    .lock()
                    .map_err(|_| error("peer relay cache poisoned"))?
                    .ids
                    .contains(&id);
                if !known {
                    send(&r)?;
                    self.note(&r)?;
                }
            }
            self.cursors[class] = next;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::record;

    #[test]
    fn verified_replay_ids_survive_payload_eviction_and_ignore_receipt_claims() {
        let relay = Relay::default();
        let tx = Arc::new(record(
            "mmx.Transaction",
            [("content_hash".into(), bytes([7; 32]))],
        ));
        let peer = RelayPeer::default();
        peer.note(&record(
            "mmx.ReceiveNote",
            [("hash".into(), bytes([7; 32]))],
        ))
        .unwrap();
        assert!(!relay.knows(&tx).unwrap());
        relay.publish([8; 32], tx.clone(), 100).unwrap();
        relay.remove(&[8; 32]).unwrap();
        assert!(relay.knows(&tx).unwrap());
        relay.reset_verified().unwrap();
        assert!(!relay.knows(&tx).unwrap());
        let mut seen = Seen::default();
        for n in 0..=MAX_VERIFIED_IDS {
            let mut id = [0u8; 32];
            id[..8].copy_from_slice(&(n as u64).to_le_bytes());
            seen.remember(id, MAX_VERIFIED_IDS);
            if n < MAX_VERIFIED_IDS {
                seen.remember([0; 32], MAX_VERIFIED_IDS);
            }
        }
        assert_eq!(seen.ids.len(), MAX_VERIFIED_IDS);
        assert_eq!(seen.order.len(), MAX_VERIFIED_IDS);
        assert!(!seen.ids.contains(&[0; 32]));
    }

    #[test]
    fn native_receipts_suppress_only_the_acknowledging_peers_payload() {
        let relay = Relay::default();
        let tx = Arc::new(record(
            "mmx.Transaction",
            [("content_hash".into(), bytes([7; 32]))],
        ));
        relay.publish([8; 32], tx.clone(), 100).unwrap();
        let mut receiver = RelayPeer::default();
        let note = Record::from_generated(&mmx_types::mmx::ReceiveNote {
            hash: [7; 32].into(),
            time: 42,
        })
        .unwrap();
        receiver.note(&note).unwrap();
        let mut sent = Vec::new();
        receiver
            .flush(&relay, |r| {
                sent.push(r.clone());
                Ok(())
            })
            .unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].name, "mmx.ReceiveNote");
        assert_eq!(hash(&sent[0], "hash").unwrap(), [7; 32]);
        // Receipt claims are per peer and never grant global/consensus credit.
        let mut other = RelayPeer::default();
        let mut sent = Vec::new();
        other
            .flush(&relay, |r| {
                sent.push(r.clone());
                Ok(())
            })
            .unwrap();
        assert_eq!(sent.len(), 2);
        assert_eq!(&sent[1], tx.as_ref());
        assert!(receiver
            .note(&record(
                "mmx.ReceiveNote",
                [("hash".into(), bytes([0; 31]))]
            ))
            .is_err());
    }

    fn balance_query(owners: Vec<[u8; 32]>, currencies: Vec<[u8; 32]>, limit: i64) -> Record {
        record(
            "mmx.Node.get_all_balances",
            [
                (
                    "addresses".into(),
                    Value::Sequence(owners.into_iter().map(bytes).collect()),
                ),
                (
                    "whitelist".into(),
                    Value::Sequence(currencies.into_iter().map(bytes).collect()),
                ),
                ("limit".into(), Value::Int(limit)),
            ],
        )
    }
    #[test]
    fn native_balance_range_limit_zero_rows_sentinels_and_whitelist_semantics() {
        for disk in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut storage = if disk {
                StorageDb::open_native(dir.path()).unwrap()
            } else {
                StorageDb::memory()
            };
            let owner = [255; 32];
            for (currency, amount) in [([0; 32], 0), ([1; 32], 17), ([255; 32], 99)] {
                storage.set_balance(&owner, &currency, amount).unwrap();
            }
            storage.database_mut().commit(1).unwrap();
            if disk {
                storage.database_mut().checkpoint().unwrap();
            }
            assert!(
                all_balances(&storage, &balance_query(vec![owner], vec![], 0))
                    .unwrap()
                    .is_empty()
            );
            // Native counts zero-valued rows before filtering and uses an exclusive ones sentinel.
            assert!(
                all_balances(&storage, &balance_query(vec![owner], vec![], 1))
                    .unwrap()
                    .is_empty()
            );
            let rows =
                all_balances(&storage, &balance_query(vec![owner, owner], vec![], -1)).unwrap();
            assert_eq!(rows, [((owner, [1; 32]), 17)].into());
            let rows =
                all_balances(&storage, &balance_query(vec![owner], vec![[255; 32]], 0)).unwrap();
            assert_eq!(rows, [((owner, [255; 32]), 99)].into());
        }
    }
    #[test]
    fn balance_row_budget_rejects_before_returning_a_partial_total_even_for_zero_balances() {
        let mut storage = StorageDb::memory();
        for n in 0..10_001u32 {
            let mut currency = [0; 32];
            currency[..4].copy_from_slice(&n.to_be_bytes());
            storage.set_balance(&[1; 32], &currency, 0).unwrap();
        }
        assert!(
            all_balances(&storage, &balance_query(vec![[1; 32]], vec![], -1))
                .unwrap_err()
                .to_string()
                .contains("10000 scanned rows")
        );
        assert!(
            all_balances(&storage, &balance_query(vec![[1; 32]], vec![], 10_000))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn consensus_relay_precedes_a_transaction_backlog_without_dropping_transactions() {
        let relay = Relay::default();
        for i in 1..=64u8 {
            let tx = Arc::new(record(
                "mmx.Transaction",
                [("content_hash".into(), Value::Bytes(vec![i; 32]))],
            ));
            relay.publish([i; 32], tx, 65536).unwrap();
        }
        let proof = Arc::new(record(
            "mmx.ProofResponse",
            [("content_hash".into(), Value::Bytes(vec![200; 32]))],
        ));
        relay.publish([200; 32], proof, 100).unwrap();
        let mut peer = RelayPeer::default();
        let mut sent = Vec::new();
        peer.flush(&relay, |r| {
            if r.name != "mmx.ReceiveNote" {
                sent.push(r.clone());
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(sent[0].name, "mmx.ProofResponse");
        assert_eq!(sent.len(), 5);
        for _ in 0..20 {
            peer.flush(&relay, |r| {
                if r.name != "mmx.ReceiveNote" {
                    sent.push(r.clone());
                }
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(sent.len(), 65);
        assert_eq!(
            sent.iter().filter(|r| r.name == "mmx.Transaction").count(),
            64
        );
    }

    #[test]
    fn relay_deduplicates_echoes_and_retries_failed_sends_without_copying_bodies() {
        let relay = Relay::default();
        let tx = Arc::new(record(
            "mmx.Transaction",
            [("content_hash".into(), Value::Bytes(vec![1; 32]))],
        ));
        relay.publish([2; 32], tx.clone(), 100).unwrap();
        assert!(Arc::ptr_eq(&relay.since(0, 1).unwrap().1[0], &tx));
        let mut peer = RelayPeer::default();
        assert!(peer.flush(&relay, |_| Err(error("disconnected"))).is_err());
        let mut count = 0;
        peer.flush(&relay, |r| {
            count += usize::from(r.name != "mmx.ReceiveNote");
            Ok(())
        })
        .unwrap();
        peer.flush(&relay, |r| {
            count += usize::from(r.name != "mmx.ReceiveNote");
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 1);
        let mut echo = RelayPeer::default();
        echo.note(&tx).unwrap();
        echo.flush(&relay, |r| {
            assert_eq!(r.name, "mmx.ReceiveNote");
            Ok(())
        })
        .unwrap();
        relay.remove(&[2; 32]).unwrap();
        assert!(relay.since(0, 1).unwrap().1.is_empty());
    }
}
