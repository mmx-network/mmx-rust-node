//! Pending transactions stay in RAM. Validation reads a pinned native snapshot;
//! only block execution can publish ledger changes.
pub(crate) use crate::relay::{Relay, RelayPeer};

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
    sync::Arc,
};

type BalanceRows = BTreeMap<([u8; 32], [u8; 32]), u128>;

const MAX_POOL_BYTES: usize = 64 * 1024 * 1024;
const MAX_POOL_ITEMS: usize = 4096;
const MAX_RELAY_TX_BYTES: usize = 256 * 1024;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::record;

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
}
