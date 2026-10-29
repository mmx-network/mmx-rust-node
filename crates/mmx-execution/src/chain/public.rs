//! Derived public-RPC indexes. Their writes share the selected ledger commit,
//! so forks and rewinds revert indexes and backfill progress atomically.
use super::{bytes, invalid, uint, ChainResult, ChainState};
use mmx_consensus::{Record, Value};
use mmx_db::Write;
use mmx_vm::Storage;
use std::collections::BTreeMap;

pub const META: &str = "chain_public_meta";
pub const TRANSACTIONS: &str = "chain_public_transactions";
pub const CONTRACTS: &str = "chain_public_contracts";
pub const EXECUTIONS: &str = "chain_public_executions";
pub const TRADES: &str = "chain_public_trades";
pub const FARMERS: &str = "chain_public_farmers";
pub const FARMER_BLOCKS: &str = "chain_public_farmer_blocks";
pub const FARMER_REWARDS: &str = "chain_public_farmer_rewards";
pub const RANKING: &str = "chain_public_ranking";
pub const START: &[u8] = b"start";
pub const ADDRESS_COUNT: &[u8] = b"address_count";
pub const CURSOR: &[u8] = b"cursor";

fn value(name: &str, fields: impl IntoIterator<Item = (&'static str, Value)>) -> Record {
    Record {
        name: name.into(),
        fields: fields.into_iter().map(|(k, v)| (k.into(), v)).collect(),
    }
}
fn ordinal(height: u32, counter: u32) -> Vec<u8> {
    [(!height).to_be_bytes(), (!counter).to_be_bytes()].concat()
}
struct Builder<'a> {
    chain: &'a ChainState,
    writes: Vec<Write>,
    farmers: BTreeMap<Vec<u8>, (u64, u128)>,
    rewards: BTreeMap<Vec<u8>, u128>,
}
impl Builder<'_> {
    fn block(&mut self, block: &Record) -> ChainResult<()> {
        let height = uint(block, "height")? as u32;
        let block_id = bytes::<32>(block.field("hash")?)?;
        if height > 0 {
            let farmer = block
                .field("proof")?
                .seq()?
                .first()
                .map(|v| v.record()?.field("farmer_key")?.bytes())
                .transpose()?
                .unwrap_or(&[0; 33])
                .to_vec();
            let reward = uint(block, "reward_amount")?;
            let address = block
                .field("reward_addr")?
                .optional()?
                .map(bytes::<32>)
                .transpose()?
                .unwrap_or([0; 32]);
            let key = [farmer.as_slice(), &(!height).to_be_bytes()].concat();
            self.writes.push(Write::put(FARMER_BLOCKS, key, block_id));
            let stats = self.farmers.entry(farmer.clone()).or_default();
            stats.0 += 1;
            stats.1 += u128::from(reward);
            *self
                .rewards
                .entry([farmer.as_slice(), address.as_slice()].concat())
                .or_default() += u128::from(reward);
        }
        let mut counter = 0u32;
        for (index, tx) in block.field("tx_list")?.seq()?.iter().enumerate() {
            let tx = tx.record()?;
            let id = bytes::<32>(tx.field("id")?)?;
            self.writes.push(Write::put(
                TRANSACTIONS,
                [(!height).to_be_bytes(), (index as u32).to_be_bytes()].concat(),
                id,
            ));
            if tx.field("exec_result")?.optional()?.is_some_and(|v| {
                v.record()
                    .and_then(|r| r.field("did_fail"))
                    .and_then(Value::boolean)
                    .unwrap_or(false)
            }) {
                continue;
            }
            let deployed = tx
                .field("deploy")?
                .optional()?
                .map(Value::record)
                .transpose()?;
            if let Some(contract) = deployed {
                if contract.name == "mmx.contract.Executable" {
                    self.writes.push(Write::put(
                        CONTRACTS,
                        [
                            bytes::<32>(contract.field("binary")?)?.as_slice(),
                            &ordinal(height, counter),
                        ]
                        .concat(),
                        id,
                    ));
                }
                counter += 1;
            }
            for op in tx.field("execute")?.seq()? {
                let Some(op) = op.optional()?.map(Value::record).transpose()? else {
                    continue;
                };
                let op_id = ordinal(height, counter);
                counter += 1;
                if !matches!(
                    op.name.as_str(),
                    "mmx.operation.Execute" | "mmx.operation.Deposit"
                ) {
                    continue;
                }
                let mut address = bytes::<32>(op.field("address")?)?;
                if address == [0; 32] {
                    address = id;
                }
                let deposit = if op.name == "mmx.operation.Deposit" {
                    Some(Value::Sequence(vec![
                        op.field("currency")?.clone(),
                        op.field("amount")?.clone(),
                    ]))
                } else {
                    None
                };
                let entry = value(
                    "mmx.exec_entry_t",
                    [
                        ("height", Value::Uint(height.into())),
                        ("time_stamp", block.field("time_stamp")?.clone()),
                        ("txid", Value::Bytes(id.to_vec())),
                        ("method", op.field("method")?.clone()),
                        ("args", op.field("args")?.clone()),
                        ("user", op.field("user")?.clone()),
                        ("deposit", Value::Optional(deposit.map(Box::new))),
                    ],
                );
                self.writes.push(Write::put(
                    EXECUTIONS,
                    [address.as_slice(), &op_id].concat(),
                    entry.encode_compact()?,
                ));
                if matches!(op.field("method")?.text()?, b"trade" | b"accept")
                    && op.name == "mmx.operation.Deposit"
                {
                    let contract = if address == id {
                        deployed.cloned()
                    } else {
                        self.chain
                            .storage
                            .get_contract(&address)?
                            .map(|v| Record::decode(&v.bytes))
                            .transpose()?
                    };
                    if contract.is_some_and(|c| {
                        c.name == "mmx.contract.Executable"
                            && c.field("binary").ok().and_then(|v| bytes::<32>(v).ok())
                                == Some(self.chain.params.offer_binary.0)
                    }) {
                        let mut log = address.to_vec();
                        log.extend(entry.encode_compact()?);
                        self.writes.push(Write::put(TRADES, op_id, log));
                    }
                }
            }
        }
        Ok(())
    }
}
/// Compact trade rows use a fixed address prefix and the declared native
/// exec_entry_t schema, without inventing a VNX type in the consensus registry.
pub fn decode_trade(bytes: &[u8]) -> ChainResult<([u8; 32], Record)> {
    let (address, entry) = bytes
        .split_at_checked(32)
        .ok_or_else(|| invalid("trade index row"))?;
    let entry = Record::decode_compact(entry)?;
    if entry.name != "mmx.exec_entry_t" {
        return Err(invalid("trade index entry type"));
    }
    Ok((address.try_into().unwrap(), entry))
}

pub(super) fn writes(
    chain: &ChainState,
    block: &Record,
    batch: &mmx_vm::storage::TransactionBatch,
) -> ChainResult<Vec<Write>> {
    let db = chain.storage.database();
    let height = uint(block, "height")? as u32;
    let start = db
        .get(META, START, None)?
        .map(|v| {
            v.try_into()
                .map(u32::from_be_bytes)
                .map_err(|_| invalid("public index start"))
        })
        .transpose()?
        .unwrap_or(height);
    let cursor = db
        .get(META, CURSOR, None)?
        .map(|v| {
            v.try_into()
                .map(u32::from_be_bytes)
                .map_err(|_| invalid("public index cursor"))
        })
        .transpose()?
        .unwrap_or(0);
    let mut builder = Builder {
        chain,
        writes: vec![Write::put(META, START, start.to_be_bytes())],
        farmers: BTreeMap::new(),
        rewards: BTreeMap::new(),
    };
    // Upgrade work is bounded per publication. Cursor and derived rows share
    // the ledger commit, including when a fork replaces that publication.
    let cursor = cursor.max(db.retention_floor().saturating_sub(1));
    let mut next = cursor;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(20);
    let mut bytes = 0usize;
    while next < start && next.saturating_sub(cursor) < 256 && bytes < 4 * 1024 * 1024 {
        let Some(id) = chain.hash_at(next)? else {
            break;
        };
        let Some(old) = chain.block(&id)? else {
            break;
        };
        bytes = bytes.saturating_add(old.memory_size());
        builder.block(&old)?;
        next += 1;
        if std::time::Instant::now() >= deadline {
            break;
        }
    }
    builder.block(block)?;
    builder
        .writes
        .push(Write::put(META, CURSOR, next.to_be_bytes()));
    for (key, (count, reward)) in builder.farmers {
        let previous = db.get(FARMERS, &key, None)?;
        let (old_count, old_reward) = if let Some(value) = previous {
            if value.len() != 24 {
                return Err(invalid("farmer counter length"));
            }
            (
                u64::from_le_bytes(value[..8].try_into().unwrap()),
                u128::from_le_bytes(value[8..].try_into().unwrap()),
            )
        } else {
            (0, 0)
        };
        let count = old_count
            .checked_add(count)
            .ok_or_else(|| invalid("farmer count overflow"))?;
        let total = old_reward
            .checked_add(reward)
            .ok_or_else(|| invalid("farmer reward overflow"))?;
        if old_count > 0 {
            builder.writes.push(Write::delete(
                RANKING,
                [(!old_count).to_be_bytes().as_slice(), &key].concat(),
            ));
        }
        builder.writes.push(Write::put(
            FARMERS,
            key.as_slice(),
            [count.to_le_bytes().as_slice(), &total.to_le_bytes()].concat(),
        ));
        builder.writes.push(Write::put(
            RANKING,
            [(!count).to_be_bytes().as_slice(), &key].concat(),
            count.to_le_bytes(),
        ));
    }
    for (key, reward) in builder.rewards {
        let old = db
            .get(FARMER_REWARDS, &key, None)?
            .map(|v| {
                v.try_into()
                    .map(u128::from_le_bytes)
                    .map_err(|_| invalid("farmer reward length"))
            })
            .transpose()?
            .unwrap_or(0);
        builder.writes.push(Write::put(
            FARMER_REWARDS,
            key,
            old.checked_add(reward)
                .ok_or_else(|| invalid("farmer reward overflow"))?
                .to_le_bytes(),
        ));
    }
    builder.writes.extend(address_count(chain, batch)?);
    // Start/cursor/count often stay unchanged for millions of blocks, and
    // completed address migration has no cursor/partial rows left to delete.
    // Keep genuine transitions in the ledger transaction for historical reads
    // and reorgs, without publishing identical metadata versions each block.
    let mut writes = Vec::with_capacity(builder.writes.len());
    for write in builder.writes {
        if write.table != META || db.get(META, &write.key, None)? != write.value {
            writes.push(write);
        }
    }
    Ok(writes)
}

const ADDRESS_CURSOR: &[u8] = b"address_cursor";
const ADDRESS_PARTIAL: &[u8] = b"address_partial";
fn address_count(
    chain: &ChainState,
    batch: &mmx_vm::storage::TransactionBatch,
) -> ChainResult<Vec<Write>> {
    let db = chain.storage.database();
    let read_count = |key| -> ChainResult<Option<u64>> {
        db.get(META, key, None)?
            .map(|v| {
                v.try_into()
                    .map(u64::from_le_bytes)
                    .map_err(|_| invalid("address counter length"))
            })
            .transpose()
    };
    let complete = read_count(ADDRESS_COUNT)?;
    let mut count = complete.or(read_count(ADDRESS_PARTIAL)?).unwrap_or(0);
    let mut end = None;
    let mut done = complete.is_some() || db.version() == 0;
    if !done {
        let start = db
            .get(META, ADDRESS_CURSOR, None)?
            .unwrap_or_else(|| vec![0; 64]);
        let mut budget = mmx_db::ReadBudget {
            rows: 256,
            keys: 100000,
            bytes: 1024 * 1024,
        };
        let rows = match db.scan_range_bounded("balances", &start, None, None, &mut budget) {
            Ok(rows) => rows,
            Err(mmx_db::Error::ReadBudget) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        for row in &rows {
            if row.key.len() != 64 || row.value.len() != 16 {
                return Err(invalid("balance index row"));
            }
            if row.key[32..] == [0; 32]
                && u128::from_le_bytes(row.value.as_slice().try_into().unwrap()) >= 1_000_000
            {
                count = count
                    .checked_add(1)
                    .ok_or_else(|| invalid("address count overflow"))?;
            }
        }
        done = rows.len() < 256;
        if !done {
            let mut key = rows.last().unwrap().key.clone();
            if let Some(i) = key.iter().rposition(|v| *v != 255) {
                key[i] += 1;
                key[i + 1..].fill(0);
                end = Some(key);
            } else {
                done = true;
            }
        }
    }
    let balances: BTreeMap<_, _> = batch
        .balances
        .iter()
        .filter(|v| v.currency == [0; 32])
        .map(|v| (v.contract, v.amount))
        .collect();
    for (address, next) in balances {
        let key = [address.as_slice(), &[0; 32]].concat();
        if end.as_ref().is_some_and(|end| key >= *end) {
            continue;
        }
        let previous = chain
            .storage
            .balance_at(&address, &[0; 32], None)?
            .unwrap_or(0);
        if previous < 1_000_000 && next >= 1_000_000 {
            count = count
                .checked_add(1)
                .ok_or_else(|| invalid("address count overflow"))?;
        }
        if previous >= 1_000_000 && next < 1_000_000 {
            count = count
                .checked_sub(1)
                .ok_or_else(|| invalid("address count underflow"))?;
        }
    }
    Ok(if done {
        vec![
            Write::put(META, ADDRESS_COUNT, count.to_le_bytes()),
            Write::delete(META, ADDRESS_CURSOR),
            Write::delete(META, ADDRESS_PARTIAL),
        ]
    } else {
        vec![
            Write::put(META, ADDRESS_PARTIAL, count.to_le_bytes()),
            Write::put(META, ADDRESS_CURSOR, end.unwrap()),
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mmx_vm::storage::{AtomicStorage, BalanceWrite, TransactionBatch};
    #[test]
    fn metadata_writes_only_changes_and_rewinds_with_the_selected_ledger() {
        let mut chain = ChainState::memory(Default::default());
        for height in 0..16u32 {
            let block = value(
                "mmx.Block",
                [
                    ("height", Value::Uint(height.into())),
                    ("hash", Value::Bytes([9; 32].to_vec())),
                    ("proof", Value::Sequence(vec![])),
                    ("reward_amount", Value::Uint(0)),
                    ("reward_addr", Value::Optional(None)),
                    ("tx_list", Value::Sequence(vec![])),
                ],
            );
            let mut batch = TransactionBatch::default();
            if matches!(height, 8 | 12) {
                batch.balances.push(BalanceWrite {
                    contract: [1; 32],
                    currency: [0; 32],
                    amount: if height == 8 { 1_000_000 } else { 0 },
                });
            }
            let writes = writes(&chain, &block, &batch).unwrap();
            let metadata = writes
                .iter()
                .filter(|w| w.table == META)
                .collect::<Vec<_>>();
            if height == 0 {
                assert_eq!(metadata.len(), 3);
                assert!(metadata.iter().all(|w| w.value.is_some()));
            } else if matches!(height, 8 | 12) {
                assert_eq!(metadata.len(), 1);
                assert_eq!(metadata[0].key, ADDRESS_COUNT);
            } else {
                assert!(metadata.is_empty(), "unchanged metadata at {height}");
            }
            chain.storage.apply_transaction(&batch).unwrap();
            chain
                .storage
                .database_mut()
                .commit_batch(height + 1, writes)
                .unwrap();
        }
        for height in 0..16 {
            assert_eq!(
                chain
                    .storage
                    .database()
                    .get(META, ADDRESS_COUNT, Some(height))
                    .unwrap(),
                Some(u64::from((8..12).contains(&height)).to_le_bytes().to_vec())
            );
        }
        chain.storage.database_mut().revert(8).unwrap();
        assert_eq!(
            chain
                .storage
                .database()
                .get(META, ADDRESS_COUNT, None)
                .unwrap(),
            Some(0u64.to_le_bytes().to_vec())
        );
        assert_eq!(
            chain.storage.database().get(META, START, None).unwrap(),
            Some(0u32.to_be_bytes().to_vec())
        );
    }

    #[test]
    fn successful_offer_trade_index_round_trips_and_rewinds_with_ledger() {
        let mut chain = ChainState::memory(Default::default());
        let id = [1; 32];
        let deploy = value(
            "mmx.contract.Executable",
            [("binary", Value::Bytes(chain.params.offer_binary.0.to_vec()))],
        );
        let op = value(
            "mmx.operation.Deposit",
            [
                ("address", Value::Bytes([0; 32].to_vec())),
                ("method", Value::String(b"trade".to_vec())),
                ("args", Value::Sequence(vec![])),
                ("user", Value::Optional(None)),
                ("currency", Value::Bytes([0; 32].to_vec())),
                ("amount", Value::Bytes(2u128.to_le_bytes().to_vec())),
            ],
        );
        let tx = value(
            "mmx.Transaction",
            [
                ("id", Value::Bytes(id.to_vec())),
                ("exec_result", Value::Optional(None)),
                (
                    "deploy",
                    Value::Optional(Some(Box::new(Value::Record(deploy)))),
                ),
                (
                    "execute",
                    Value::Sequence(vec![Value::Optional(Some(Box::new(Value::Record(op))))]),
                ),
            ],
        );
        let block = value(
            "mmx.Block",
            [
                ("height", Value::Uint(0)),
                ("hash", Value::Bytes([9; 32].to_vec())),
                ("time_stamp", Value::Int(42)),
                ("tx_list", Value::Sequence(vec![Value::Record(tx)])),
            ],
        );
        let writes = writes(&chain, &block, &TransactionBatch::default()).unwrap();
        let row = writes.iter().find(|v| v.table == TRADES).unwrap();
        let (owner, entry) = decode_trade(row.value.as_ref().unwrap()).unwrap();
        assert_eq!(owner, id);
        assert_eq!(entry.field("method").unwrap().text().unwrap(), b"trade");
        assert!(decode_trade(&[0; 31]).is_err());
        let key = row.key.clone();
        chain
            .storage
            .database_mut()
            .commit_batch(1, writes)
            .unwrap();
        assert!(chain
            .storage
            .database()
            .get(TRADES, &key, None)
            .unwrap()
            .is_some());
        chain.storage.database_mut().revert(0).unwrap();
        assert!(chain
            .storage
            .database()
            .get(TRADES, &key, None)
            .unwrap()
            .is_none());
    }
    #[test]
    fn address_migration_is_bounded_tracks_changes_and_rewinds_atomically() {
        let mut chain = ChainState::memory(Default::default());
        let address = |i: u32| {
            let mut out = [0; 32];
            out[..4].copy_from_slice(&i.to_be_bytes());
            out
        };
        let mut initial = TransactionBatch::default();
        for i in 0..600 {
            initial.balances.push(BalanceWrite {
                contract: address(i),
                currency: [0; 32],
                amount: 1_000_000,
            });
        }
        chain.storage.apply_transaction(&initial).unwrap();
        chain.storage.database_mut().commit(1).unwrap();
        for version in 2..=4 {
            let mut batch = TransactionBatch::default();
            if version == 3 {
                // Both the scanned and unscanned portions change during catch-up.
                for i in [1, 550] {
                    batch.balances.push(BalanceWrite {
                        contract: address(i),
                        currency: [0; 32],
                        amount: 0,
                    });
                }
                batch.balances.push(BalanceWrite {
                    contract: address(700),
                    currency: [0; 32],
                    amount: 1_000_000,
                });
            }
            let writes = address_count(&chain, &batch).unwrap();
            chain.storage.apply_transaction(&batch).unwrap();
            chain
                .storage
                .database_mut()
                .commit_batch(version, writes)
                .unwrap();
        }
        let count = chain
            .storage
            .database()
            .get(META, ADDRESS_COUNT, None)
            .unwrap()
            .unwrap();
        assert_eq!(u64::from_le_bytes(count.try_into().unwrap()), 599);
        chain.storage.database_mut().revert(2).unwrap();
        assert!(chain
            .storage
            .database()
            .get(META, ADDRESS_COUNT, None)
            .unwrap()
            .is_none());
        assert!(chain
            .storage
            .database()
            .get(META, ADDRESS_CURSOR, None)
            .unwrap()
            .is_some());
    }
}
