use mmx_consensus::Record;
use mmx_execution::{
    block::{self, BlockContext},
    nft,
};
use mmx_types::{mmx, VnxType};
use mmx_vm::{
    storage::{AtomicStorage, BalanceWrite, ContractData, StorageWrite, TransactionBatch},
    Address, Storage, StorageDb, Var,
};
use std::{cell::RefCell, collections::BTreeMap};
use vnx::serde_json::{self, Value};

fn bytes(v: &Value) -> Vec<u8> {
    v.as_str()
        .unwrap()
        .as_bytes()
        .chunks_exact(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}
fn address(v: &Value) -> Address {
    bytes(v).try_into().unwrap()
}
fn fixtures() -> Vec<(Value, Value)> {
    let mut seeds = BTreeMap::<String, Value>::new();
    include_str!("fixtures/nft.jsonl")
        .lines()
        .map(|line| {
            let row: Value = serde_json::from_str(line).unwrap();
            let snapshot = if let Some(seed) = row["seed"].as_str() {
                seeds[seed].clone()
            } else {
                let snapshot = row["snapshot"].clone();
                seeds.insert(row["name"].as_str().unwrap().into(), snapshot.clone());
                snapshot
            };
            (row, snapshot)
        })
        .collect()
}
fn setup(snapshot: &Value) -> StorageDb {
    let mut db = StorageDb::memory();
    let mut batch = TransactionBatch::default();
    for c in snapshot["contracts"].as_array().unwrap() {
        // Native read_cost is absent during resolution: even these large
        // metadata costs must not consume the call's VM gas budget.
        batch.contracts.push((
            address(&c["address"]),
            ContractData {
                bytes: bytes(&c["wire"]),
                read_cost: u64::MAX,
            },
        ));
    }
    for c in snapshot["cells"].as_array().unwrap() {
        let (var, _) = Var::decode(&bytes(&c["value"]), false, true).unwrap();
        batch.writes.push((
            address(&c["contract"]),
            StorageWrite {
                address: c["address"].as_u64().unwrap(),
                key: c["key"].as_u64(),
                var,
            },
        ));
    }
    for b in snapshot["balances"].as_array().unwrap() {
        batch.balances.push(BalanceWrite {
            contract: address(&b["contract"]),
            currency: address(&b["currency"]),
            amount: b["amount"].as_str().unwrap().parse().unwrap(),
        });
    }
    db.apply_transaction(&batch).unwrap();
    db.database_mut().commit(1).unwrap();
    db
}
fn unchanged(db: &StorageDb, snapshot: &Value) {
    assert!(!db.database().has_pending(), "resolution published writes");
    for c in snapshot["cells"].as_array().unwrap() {
        let (expected, _) = Var::decode(&bytes(&c["value"]), false, true).unwrap();
        assert_eq!(
            db.read(
                &address(&c["contract"]),
                c["address"].as_u64().unwrap(),
                c["key"].as_u64()
            )
            .unwrap()
            .unwrap()
            .value,
            expected.value
        );
    }
    for b in snapshot["balances"].as_array().unwrap() {
        assert_eq!(
            db.get_balance(&address(&b["contract"]), &address(&b["currency"]))
                .unwrap(),
            Some(b["amount"].as_str().unwrap().parse().unwrap())
        );
    }
}
struct Observed<'a> {
    db: &'a StorageDb,
    calls: RefCell<Vec<Address>>,
    fail_on: usize,
}
impl Storage for Observed<'_> {
    fn read(&self, c: &Address, a: u64, k: Option<u64>) -> mmx_vm::storage::Result<Option<Var>> {
        self.db.read(c, a, k)
    }
    fn lookup(&self, c: &Address, v: &mmx_vm::Value) -> mmx_vm::storage::Result<Option<u64>> {
        self.db.lookup(c, v)
    }
    fn get_balance(&self, c: &Address, k: &Address) -> mmx_vm::storage::Result<Option<u128>> {
        self.db.get_balance(c, k)
    }
    fn get_contract(&self, a: &Address) -> mmx_vm::storage::Result<Option<ContractData>> {
        self.calls.borrow_mut().push(*a);
        if self.fail_on == self.calls.borrow().len() {
            return Err(mmx_vm::storage::Error::Invalid(
                "injected contract read failure",
            ));
        }
        self.db.get_contract(a)
    }
    fn apply(&mut self, _: &Address, _: &[StorageWrite]) -> mmx_vm::storage::Result<()> {
        panic!("target resolution attempted to publish writes")
    }
}
#[test]
fn native_plot_nft_calls_fallback_address_conversion_gas_and_isolation() {
    let mut count = 0;
    let mut accepted = 0;
    for (row, snapshot) in fixtures() {
        if row["kind"] != "target" {
            continue;
        }
        let name = row["name"].as_str().unwrap();
        let params = mmx::ChainParams::decode(&bytes(&snapshot["params"])).unwrap();
        let db = setup(&snapshot);
        let observed = Observed {
            db: &db,
            calls: RefCell::default(),
            fail_on: row["fail_on"].as_u64().unwrap() as usize,
        };
        let farmer = (!row["farmer"].as_str().unwrap().is_empty()).then(|| address(&row["farmer"]));
        let result = nft::target(
            &observed,
            &params,
            row["height"].as_u64().unwrap() as u32,
            address(&row["contract"]),
            farmer,
        );
        if row["result"] == "OK" {
            assert_eq!(
                result.unwrap_or_else(|e| panic!("{name}: {e}")),
                address(&row["target"]),
                "{name}"
            );
            accepted += 1;
        } else {
            assert!(result.is_err(), "{name}: native {}", row["result"]);
            if observed.fail_on == 1 {
                assert!(
                    matches!(result, Err(mmx_vm::Error::Storage(_))),
                    "{name}: initial lookup must precede resolver wrapping"
                );
            } else {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .starts_with("contract execution: mmx_reward_target() failed with:"),
                    "{name}: resolver error boundary"
                );
            }
        }
        assert_eq!(
            *observed.calls.borrow(),
            row["lookups"]
                .as_array()
                .unwrap()
                .iter()
                .map(address)
                .collect::<Vec<_>>(),
            "{name}: lookup order"
        );
        unchanged(&db, &snapshot);
        count += 1;
    }
    assert_eq!((count, accepted), (185, 161));
}
#[test]
fn nft_reward_fields_and_parent_height_are_checked_before_block_settlement() {
    let mut count = 0;
    let mut accepted = 0;
    for (row, snapshot) in fixtures() {
        if row["kind"] != "block" {
            continue;
        }
        let name = row["name"].as_str().unwrap();
        let block = Record::decode(&bytes(&row["block"])).unwrap();
        let context = BlockContext {
            params: mmx::ChainParams::decode(&bytes(&snapshot["params"])).unwrap(),
            previous: mmx::BlockHeader::decode(&bytes(&row["parent"])).unwrap(),
            expected_vdf_payout: None,
        };
        let mut db = setup(&snapshot);
        let prepared = block::prepare(&mut db, &context, &block);
        if row["result"] == "OK" {
            drop(prepared.unwrap_or_else(|e| panic!("{name}: {e}")));
            accepted += 1;
        } else {
            let err = prepared
                .err()
                .unwrap_or_else(|| panic!("{name}: native {}", row["result"]));
            assert_eq!(err.to_string(), row["result"].as_str().unwrap(), "{name}");
        }
        unchanged(&db, &snapshot);
        count += 1;
    }
    assert_eq!((count, accepted), (27, 12));
}
