//! Original C++ Node::validate and both Node::execute overloads are extracted by
//! the generator without editing their bodies. Compare against that implementation.
use mmx_consensus::Record;
use mmx_execution::transaction::{self, TransactionContext, TransactionResult, ValidationError};
use mmx_types::{mmx, VnxType};
use mmx_vm::{
    storage::{AtomicStorage, BalanceWrite, ContractData, TransactionBatch, TransactionIndex},
    Address, Storage, StorageDb,
};
use std::collections::{BTreeMap, BTreeSet};
use vnx::serde_json::{self, Value};
fn bytes(text: &str) -> Vec<u8> {
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect()
}
fn address(v: &Value) -> Address {
    bytes(v.as_str().unwrap()).try_into().unwrap()
}
fn record(v: &Value) -> Record {
    Record::decode(&bytes(v.as_str().unwrap())).unwrap()
}
fn hex(v: &[u8]) -> String {
    v.iter().map(|b| format!("{b:02x}")).collect()
}
fn context(seed: &Value, tx: &Record) -> TransactionContext {
    let mut locks: BTreeSet<Address> = seed["contracts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| address(&c["address"]))
        .collect();
    locks.insert(tx.field("id").unwrap().bytes().unwrap().try_into().unwrap());
    TransactionContext {
        height: seed["height"].as_u64().unwrap() as u32,
        params: mmx::ChainParams::decode(&bytes(seed["params"].as_str().unwrap())).unwrap(),
        locked_contracts: Some(locks),
    }
}
fn setup(storage: &mut StorageDb, seed: &Value, next: u32) {
    let mut batch = TransactionBatch::default();
    for b in seed["balances"].as_array().unwrap() {
        batch.balances.push(BalanceWrite {
            contract: address(&b["address"]),
            currency: address(&b["currency"]),
            amount: b["amount"].as_str().unwrap().parse().unwrap(),
        });
    }
    for c in seed["contracts"].as_array().unwrap() {
        batch.contracts.push((
            address(&c["address"]),
            ContractData {
                bytes: bytes(c["wire"].as_str().unwrap()),
                read_cost: c["cost"].as_u64().unwrap(),
            },
        ));
    }
    for i in seed["indexes"].as_array().unwrap() {
        batch.transactions.push((
            address(&i["address"]),
            TransactionIndex {
                height: i["height"].as_u64().unwrap() as u32,
                contract_read_cost: i["cost"].as_u64().unwrap(),
            },
        ));
    }
    storage.apply_transaction(&batch).unwrap();
    storage.database_mut().commit(next).unwrap();
}
fn is_static(error: &ValidationError) -> bool {
    matches!(error, ValidationError::Static(_))
        || matches!(error,ValidationError::Authorization(e)if e.is_static_failure())
}
#[test]
fn native_transaction_validation_results_state_and_settlement() {
    let mut seeds = BTreeMap::<String, Value>::new();
    let mut count = 0;
    for line in include_str!("fixtures/transactions.jsonl").lines() {
        let trace: Value = serde_json::from_str(line).unwrap();
        let name = trace["name"].as_str().unwrap();
        let seed = if let Some(name) = trace["seed"].as_str() {
            seeds[name].clone()
        } else {
            seeds.insert(name.into(), trace.clone());
            trace.clone()
        };
        let mut tx = record(&seed["tx"]);
        let ctx = context(&seed, &tx);
        if !trace["submitted_result"].is_null() {
            let supplied =
                TransactionResult::from_record(&record(&trace["submitted_result"])).unwrap();
            tx = transaction::with_result(&tx, &supplied, &ctx.params).unwrap();
            assert_eq!(
                tx.field("static_cost").unwrap().uint().unwrap(),
                trace["static_cost"].as_u64().unwrap(),
                "{name}: updated static cost"
            );
            assert_eq!(
                hex(tx.field("content_hash").unwrap().bytes().unwrap()),
                trace["content_hash"].as_str().unwrap(),
                "{name}: content hash"
            );
        }
        let mut storage = StorageDb::memory();
        setup(&mut storage, &seed, 1);
        let prepared = transaction::prepare(&mut storage, &ctx, &tx);
        if !trace["accepted"].as_bool().unwrap() {
            let rejection = match prepared {
                Err(e) => e,
                Ok(p) => panic!("{name}: accepted; {:?}", p.result()),
            };
            let class = if is_static(&rejection.error) {
                "static"
            } else {
                "dynamic"
            };
            assert_eq!(
                class,
                trace["class"].as_str().unwrap(),
                "{name}: {}",
                rejection.error
            );
            assert!(
                !storage.database().has_pending(),
                "{name}: rejected transaction published writes"
            );
        } else {
            let prepared = prepared.unwrap_or_else(|e| {
                panic!(
                    "{name}: rejected {} ({:?}), cost={}",
                    e, e.info, e.total_cost
                )
            });
            assert!(
                !prepared.storage().database().has_pending(),
                "{name}: prepare published writes"
            );
            let expected =
                TransactionResult::from_record(&record(if trace["submitted_result"].is_null() {
                    &trace["result"]
                } else {
                    &trace["submitted_result"]
                }))
                .unwrap();
            let actual = prepared.result();
            assert_eq!(
                (actual.did_fail, actual.total_cost, actual.total_fee),
                (expected.did_fail, expected.total_cost, expected.total_fee),
                "{name}: result"
            );
            assert_eq!(actual.inputs, expected.inputs, "{name}: execution inputs");
            assert_eq!(
                actual.outputs, expected.outputs,
                "{name}: execution outputs"
            );
            assert_eq!(
                actual.error.as_ref().map(|e| &e.info),
                expected.error.as_ref().map(|e| &e.info),
                "{name}: error info"
            );
            assert!(actual.error.as_ref().is_none_or(|e| e.message.len() <= 160));
            let actual_writes: BTreeMap<_, _> = prepared
                .batch()
                .writes
                .iter()
                .map(|(c, w)| {
                    (
                        (*c, w.address, w.key),
                        hex(&w.var.encode(false, true).unwrap()),
                    )
                })
                .collect();
            let expected_writes: BTreeMap<_, _> = trace["writes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|w| {
                    (
                        (
                            address(&w["contract"]),
                            w["address"].as_u64().unwrap(),
                            w["key"].as_u64(),
                        ),
                        w["value"].as_str().unwrap().to_owned(),
                    )
                })
                .collect();
            assert_eq!(actual_writes, expected_writes, "{name}: VM state writes");
            let result = prepared.commit().unwrap();
            for b in trace["settled"].as_array().unwrap() {
                assert_eq!(
                    storage
                        .get_balance(&address(&b["address"]), &address(&b["currency"]))
                        .unwrap(),
                    Some(b["amount"].as_str().unwrap().parse().unwrap()),
                    "{name}: settlement {b}"
                );
            }
            let id = tx.field("id").unwrap().bytes().unwrap().try_into().unwrap();
            assert_eq!(
                storage.get_transaction_index(&id).unwrap().unwrap().height,
                ctx.height,
                "{name}: transaction index"
            );
            assert_eq!(
                storage
                    .get_transaction_index(&id)
                    .unwrap()
                    .unwrap()
                    .contract_read_cost,
                trace["index_cost"].as_u64().unwrap(),
                "{name}: index read cost"
            );
            if !result.did_fail {
                if let Some(deploy) = prepared_deploy(&tx) {
                    let stored =
                        Record::decode(&storage.get_contract(&id).unwrap().unwrap().bytes).unwrap();
                    assert_eq!(
                        stored
                            .calc_hash(false, tx.field("version").unwrap().uint().unwrap() as u32)
                            .unwrap(),
                        deploy
                            .calc_hash(false, tx.field("version").unwrap().uint().unwrap() as u32)
                            .unwrap(),
                        "{name}: stored contract hash"
                    );
                }
            }
            if result.did_fail {
                assert!(prepared_deploy(&tx)
                    .is_none_or(|_| storage.get_contract(&id).unwrap().is_none()));
            } else if prepared_deploy(&tx).is_some() {
                assert!(
                    storage.get_contract(&id).unwrap().is_some(),
                    "{name}: deployment missing"
                );
            }
        }
        count += 1;
    }
    assert_eq!(count, 2016);
}
fn prepared_deploy(tx: &Record) -> Option<&Record> {
    tx.field("deploy")
        .unwrap()
        .optional()
        .unwrap()
        .map(|v| v.record().unwrap())
}
#[test]
fn transaction_commit_abort_reopen_rollback_and_duplicate_index() {
    let seed: Value = include_str!("fixtures/transactions.jsonl")
        .lines()
        .map(|s| serde_json::from_str::<Value>(s).unwrap())
        .find(|v| v["name"] == "write_v1")
        .unwrap();
    let tx = record(&seed["tx"]);
    let ctx = context(&seed, &tx);
    let id = tx.field("id").unwrap().bytes().unwrap().try_into().unwrap();
    let dir = tempfile::tempdir().unwrap();
    {
        let mut db = StorageDb::open(dir.path()).unwrap();
        setup(&mut db, &seed, 10);
        let p = transaction::prepare(&mut db, &ctx, &tx).unwrap();
        assert!(!p.batch().writes.is_empty());
        drop(p);
        assert!(!db.database().has_pending());
        assert!(db.get_transaction_index(&id).unwrap().is_none());
        transaction::execute(&mut db, &ctx, &tx).unwrap();
        assert!(db.get_transaction_index(&id).unwrap().is_some());
        assert!(
            matches!(transaction::prepare(&mut db,&ctx,&tx),Err(e)if matches!(e.error,ValidationError::Static(_)))
        );
        db.database_mut().commit(11).unwrap();
    }
    {
        let mut db = StorageDb::open(dir.path()).unwrap();
        assert!(db.get_transaction_index(&id).unwrap().is_some());
        assert!(db
            .read(
                &[
                    1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0
                ],
                mmx_vm::MEM_STATIC,
                None
            )
            .unwrap()
            .is_some());
        db.database_mut().revert(10).unwrap();
        assert!(db.get_transaction_index(&id).unwrap().is_none());
        transaction::execute(&mut db, &ctx, &tx).unwrap();
        db.database_mut().commit(11).unwrap();
    }
    let db = StorageDb::open(dir.path()).unwrap();
    assert!(db.get_transaction_index(&id).unwrap().is_some());
}

#[test]
fn automatic_dependency_lock_failures_reject_without_fee_publication() {
    let seed: Value = include_str!("fixtures/transactions.jsonl")
        .lines()
        .map(|s| serde_json::from_str::<Value>(s).unwrap())
        .find(|v| v["name"] == "write_v1")
        .unwrap();
    let tx = record(&seed["tx"]);
    let mut ctx = context(&seed, &tx);
    ctx.locked_contracts = None;
    let mut db = StorageDb::memory();
    setup(&mut db, &seed, 10);
    let prepared = transaction::prepare(&mut db, &ctx, &tx).unwrap();
    assert!(!prepared.result().did_fail);
    drop(prepared);
    ctx.params.max_rcall_width = 0;
    assert!(transaction::prepare(&mut db, &ctx, &tx).is_err());
    assert!(!db.database().has_pending());
    assert!(db
        .get_transaction_index(&tx.field("id").unwrap().bytes().unwrap().try_into().unwrap())
        .unwrap()
        .is_none());
}

#[test]
fn fee_rounding_and_overflow_boundaries() {
    use transaction::{cost_to_fee, fee_to_cost};
    assert_eq!(cost_to_fee(1023, 1025).unwrap(), 1023);
    assert_eq!(cost_to_fee(1024, 1025).unwrap(), 1025);
    assert_eq!(fee_to_cost(1024, 1025).unwrap(), 1023);
    assert_eq!(cost_to_fee(u64::MAX, 1024).unwrap(), u64::MAX);
    assert_eq!(fee_to_cost(u64::MAX, 1024).unwrap(), u64::MAX);
    assert!(cost_to_fee(u64::MAX, u32::MAX).is_err());
    assert!(fee_to_cost(u64::MAX, 1).is_err());
    assert!(fee_to_cost(1, 0).is_err());
}
