//! Independently generated C++ VM/interface/cache traces, using native contract
//! envelopes. Compare state bytes, balances, outputs, costs and outer error sites.
use mmx_execution::{execute, Context, Invocation};
use mmx_vm::{
    storage::{self, AtomicStorage, BalanceWrite, ContractData, StorageWrite, TransactionBatch},
    Address, InputValue, Storage, StorageDb, Value, Var,
};
use std::collections::BTreeMap;
use vnx::serde_json::{self as json, Value as Json};
fn bytes(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks_exact(2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect()
}
fn address(value: &Json) -> Address {
    bytes(value.as_str().unwrap()).try_into().unwrap()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
struct Recorder {
    storage: StorageDb,
    batch: TransactionBatch,
}
impl Storage for Recorder {
    fn read(&self, c: &Address, a: u64, k: Option<u64>) -> storage::Result<Option<Var>> {
        self.storage.read(c, a, k)
    }
    fn lookup(&self, c: &Address, v: &Value) -> storage::Result<Option<u64>> {
        self.storage.lookup(c, v)
    }
    fn get_balance(&self, c: &Address, k: &Address) -> storage::Result<Option<u128>> {
        self.storage.get_balance(c, k)
    }
    fn get_contract(&self, a: &Address) -> storage::Result<Option<ContractData>> {
        self.storage.get_contract(a)
    }
    fn apply(&mut self, c: &Address, w: &[StorageWrite]) -> storage::Result<()> {
        self.storage.apply(c, w)
    }
}
impl AtomicStorage for Recorder {
    fn apply_transaction(&mut self, b: &TransactionBatch) -> storage::Result<()> {
        self.storage.apply_transaction(b)?;
        self.batch = b.clone();
        Ok(())
    }
}
fn seed(seed: &Json, limit: u64) -> (Recorder, Context, Vec<Invocation>) {
    let mut storage = StorageDb::memory();
    let mut context = Context {
        txid: address(&seed["txid"]),
        height: seed["height"].as_u64().unwrap() as u32,
        hardfork2_height: seed["hf2"].as_u64().unwrap() as u32,
        protocol_version: seed["version"].as_u64().unwrap() as u32,
        gas_limit: limit,
        locked_contracts: Some(
            seed["locks"]
                .as_array()
                .unwrap()
                .iter()
                .map(address)
                .collect(),
        ),
        ..Default::default()
    };
    let mut batch = TransactionBatch::default();
    for c in seed["contracts"].as_array().unwrap() {
        let a = address(&c["address"]);
        let c = ContractData {
            bytes: bytes(c["wire"].as_str().unwrap()),
            read_cost: c["cost"].as_u64().unwrap(),
        };
        if seed["deploy"] == true && a == context.txid {
            context.deployment = Some(c);
        } else {
            batch.contracts.push((a, c));
        }
    }
    for b in seed["balances"].as_array().unwrap() {
        batch.balances.push(BalanceWrite {
            contract: address(&b["contract"]),
            currency: address(&b["currency"]),
            amount: b["amount"].as_str().unwrap().parse().unwrap(),
        });
    }
    for w in seed["initial"].as_array().unwrap() {
        let raw = bytes(w["value"].as_str().unwrap());
        batch.writes.push((
            address(&w["contract"]),
            StorageWrite {
                address: w["address"].as_u64().unwrap(),
                key: w["key"].as_u64(),
                var: Var::decode(&raw, false, true).unwrap().0,
            },
        ));
    }
    storage.apply_transaction(&batch).unwrap();
    storage.database_mut().commit(1).unwrap();
    let calls = seed["calls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let mut call = Invocation::new(
                address(&c["address"]),
                bytes(c["method_hex"].as_str().unwrap()),
            );
            call.args = c["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| InputValue::try_from(v).unwrap())
                .collect();
            call.user = (!c["user"].is_null()).then(|| address(&c["user"]));
            call.deposit = (!c["currency"].is_null()).then(|| {
                (
                    address(&c["currency"]),
                    c["amount"].as_str().unwrap().parse().unwrap(),
                )
            });
            call
        })
        .collect();
    (
        Recorder {
            storage,
            batch: TransactionBatch::default(),
        },
        context,
        calls,
    )
}
fn ordered(mut rows: Vec<Json>) -> Json {
    rows.sort_by_key(Json::to_string);
    Json::Array(rows)
}
#[test]
fn native_contract_loading_calls_atomicity_and_failure_gas() {
    let seeds: BTreeMap<String, Json> = include_str!("fixtures/seeds.jsonl")
        .lines()
        .map(|line| {
            let v: Json = json::from_str(line).unwrap();
            (v["name"].as_str().unwrap().to_owned(), v)
        })
        .collect();
    let mut count = 0;
    for line in include_str!("fixtures/traces.jsonl").lines() {
        let trace: Json = json::from_str(line).unwrap();
        let name = trace["name"].as_str().unwrap();
        let limit = trace["limit"].as_u64().unwrap();
        if name.starts_with("limits_original_") {
            assert_eq!(trace["ok"], true, "original suite {name}");
        }
        let (mut backend, context, calls) = seed(&seeds[name], limit);
        let result = execute(&mut backend, &context, &calls);
        let diagnostic = format!("{name} gas={limit}: {result:?}");
        assert_eq!(
            result.is_ok(),
            trace["ok"].as_bool().unwrap(),
            "{diagnostic}"
        );
        let cost = match &result {
            Ok(r) => r.total_cost,
            Err(e) => e.total_cost,
        };
        assert_eq!(cost, trace["cost"].as_u64().unwrap(), "cost {diagnostic}");
        if let Err(failure) = &result {
            assert_eq!(
                failure.info.code,
                trace["code"].as_u64().unwrap() as u32,
                "code {diagnostic}"
            );
            assert_eq!(
                failure.info.address.unwrap_or(u32::MAX),
                trace["pc"].as_u64().unwrap() as u32,
                "PC {diagnostic}"
            );
            assert_eq!(
                failure.info.operation.unwrap_or(u32::MAX),
                trace["operation"].as_u64().unwrap() as u32,
                "operation {diagnostic}"
            );
            assert_eq!(
                failure.info.line.map(u64::from),
                trace["line"].as_u64(),
                "line {diagnostic}"
            );
            assert!(
                !backend.storage.database().has_pending(),
                "failed batch published state: {diagnostic}"
            );
        }
        let writes=backend.batch.writes.iter().map(|(c,w)| json::json!({"contract":hex(c),"address":w.address,"key":w.key,"value":hex(&w.var.encode(false,true).unwrap())})).collect();
        assert_eq!(
            ordered(writes),
            ordered(trace["writes"].as_array().unwrap().clone()),
            "writes {diagnostic}"
        );
        let balances=backend.batch.balances.iter().map(|w| json::json!({"contract":hex(&w.contract),"currency":hex(&w.currency),"amount":w.amount.to_string()})).collect();
        assert_eq!(
            ordered(balances),
            ordered(trace["balances"].as_array().unwrap().clone()),
            "balances {diagnostic}"
        );
        if let Ok(receipt) = result {
            let outputs:Vec<_>=receipt.outputs.iter().map(|o|json::json!({"contract":hex(&o.contract),"address":hex(&o.address),"amount":o.amount.to_string(),"memo":o.memo.as_deref().map(hex)})).collect();
            assert_eq!(
                outputs,
                *trace["outputs"].as_array().unwrap(),
                "outputs {diagnostic}"
            );
            let spend=receipt.spend.iter().map(|((c,k),a)|json::json!({"contract":hex(c),"currency":hex(k),"amount":a.to_string()})).collect();
            assert_eq!(
                ordered(spend),
                ordered(trace["spend"].as_array().unwrap().clone()),
                "spend {diagnostic}"
            );
        }
        count += 1;
    }
    assert!(count > 2000);
}
