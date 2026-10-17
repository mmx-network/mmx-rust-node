use mmx_consensus::{vdf, Record};
use mmx_execution::chain::{
    genesis::{self, Assets},
    ChainState,
};
use mmx_types::{mmx, VnxType};
use mmx_vm::{Address, Storage, Var, MEM_STATIC};
use std::fs;
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
fn record(v: &Value) -> Record {
    Record::decode(&bytes(v)).unwrap()
}
fn rows() -> Vec<Value> {
    include_str!("fixtures/chain.jsonl")
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}
fn params(row: &Value) -> mmx::ChainParams {
    mmx::ChainParams::decode(&bytes(&row["params"])).unwrap()
}
fn assets(row: &Value) -> Assets {
    let optional = |v: &Value| (!v.as_str().unwrap().is_empty()).then(|| record(v));
    Assets {
        binaries: row["binaries"]
            .as_array()
            .unwrap()
            .iter()
            .map(record)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap(),
        testnet_rewards: optional(&row["rewards"]),
        project_relay: optional(&row["relay"]),
    }
}
fn verify_state(chain: &ChainState, state: &Value) {
    for b in state["balances"].as_array().unwrap() {
        assert_eq!(
            chain
                .storage()
                .get_balance(&address(&b["address"]), &address(&b["currency"]))
                .unwrap(),
            Some(b["amount"].as_str().unwrap().parse().unwrap())
        );
    }
    for s in state["supplies"].as_array().unwrap() {
        assert_eq!(
            chain
                .storage()
                .get_total_supply(&address(&s["currency"]))
                .unwrap(),
            Some(s["amount"].as_str().unwrap().parse().unwrap())
        );
    }
    assert!(!chain.storage().database().has_pending());
}
fn verify_step(chain: &ChainState, step: &Value) {
    let block = record(&step["block"]);
    let hash = block
        .field("hash")
        .unwrap()
        .bytes()
        .unwrap()
        .try_into()
        .unwrap();
    let tip = chain.tip().unwrap().unwrap();
    assert_eq!(tip.hash.0, hash);
    assert_eq!(chain.hash_at(tip.height).unwrap(), Some(hash));
    assert_eq!(chain.block(&hash).unwrap(), Some(block.clone()));
    assert_eq!(chain.storage().database().version(), tip.height + 1);
    for tx in block.field("tx_list").unwrap().seq().unwrap() {
        let tx = tx.record().unwrap();
        let id = tx.field("id").unwrap().bytes().unwrap().try_into().unwrap();
        assert_eq!(chain.transaction(&id).unwrap().as_ref(), Some(tx));
        assert_eq!(
            chain
                .storage()
                .get_transaction_index(&id)
                .unwrap()
                .unwrap()
                .height,
            tip.height
        );
    }
    verify_state(chain, &step["state"]);
    if let Some(cells) = step["cells"].as_array() {
        for c in cells {
            let (expected, _) = Var::decode(&bytes(&c["value"]), false, true).unwrap();
            assert_eq!(
                chain
                    .storage()
                    .read(
                        &address(&c["contract"]),
                        c["address"].as_u64().unwrap(),
                        None
                    )
                    .unwrap()
                    .unwrap()
                    .value,
                expected.value
            );
        }
    }
}

#[test]
fn native_mainnet_testnet_genesis_hashes_bootstrap_state_and_failure_rules() {
    let mut count = 0;
    let mut accepted = 0;
    for row in rows().into_iter().filter(|r| r["kind"] == "genesis") {
        let p = params(&row);
        let a = assets(&row);
        let name = row["name"].as_str().unwrap();
        let mut chain = ChainState::memory(p.clone());
        if row["result"] == "OK" {
            let expected = record(&row["block"]);
            assert_eq!(
                genesis::build(&p, &a).unwrap(),
                expected,
                "{name}: exact native genesis"
            );
            chain
                .initialize(&a)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            verify_step(&chain, &row);
            assert!(chain.initialize(&a).is_err(), "{name}: initialized twice");
            for c in row["contracts"].as_array().unwrap() {
                let data = chain
                    .storage()
                    .get_contract(&address(&c["id"]))
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    Record::decode(&data.bytes).unwrap(),
                    record(&c["wire"]),
                    "{name}: deployed bytes"
                );
                assert_eq!(data.read_cost, c["cost"].as_u64().unwrap());
                assert!(
                    chain
                        .storage()
                        .find_range(&address(&c["id"]), MEM_STATIC, u64::MAX, None)
                        .unwrap()
                        .is_empty(),
                    "{name}: genesis executed a constructor"
                );
            }
            for i in row["indexes"].as_array().unwrap() {
                assert_eq!(
                    chain
                        .storage()
                        .get_transaction_index(&address(&i["id"]))
                        .unwrap()
                        .unwrap()
                        .contract_read_cost,
                    i["cost"].as_u64().unwrap()
                );
            }
            if p.network == "mainnet" {
                let relay = a.project_relay.unwrap();
                let id = relay
                    .field("id")
                    .unwrap()
                    .bytes()
                    .unwrap()
                    .try_into()
                    .unwrap();
                assert!(chain.storage().get_contract(&id).unwrap().is_none());
                assert!(chain.transaction(&id).unwrap().is_none());
            }
            accepted += 1;
        } else {
            assert!(
                chain.initialize(&a).is_err(),
                "{name}: native {}",
                row["result"]
            );
            assert_eq!(chain.storage().database().version(), 0);
            assert!(chain.tip().unwrap().is_none());
            assert!(!chain.storage().database().has_pending());
        }
        count += 1;
    }
    assert_eq!((count, accepted), (16, 9));
}

#[test]
fn network_bootstrap_reopens_and_rejects_parameter_changes() {
    for row in rows()
        .into_iter()
        .filter(|r| r["name"] == "mainnet" || r["name"] == "testnet13")
    {
        let dir = tempfile::tempdir().unwrap();
        let p = params(&row);
        let mut chain = ChainState::open(dir.path(), p.clone()).unwrap();
        chain.initialize(&assets(&row)).unwrap();
        drop(chain);
        let chain = ChainState::open(dir.path(), p.clone()).unwrap();
        verify_step(&chain, &row);
        drop(chain);
        let mut wrong = p.clone();
        wrong.min_txfee += 1;
        assert!(ChainState::open(dir.path(), wrong).is_err());
        ChainState::open(dir.path(), p).unwrap();
    }
}
fn synthetic() -> (Value, Vec<Value>) {
    let all = rows();
    let genesis = all
        .iter()
        .find(|r| r["name"] == "synthetic")
        .unwrap()
        .clone();
    let steps = all.iter().find(|r| r["kind"] == "chain").unwrap()["steps"]
        .as_array()
        .unwrap()
        .clone();
    (genesis, steps)
}
fn append(chain: &mut ChainState, step: &Value) {
    let point = vdf::verify(&record(&step["vdf"]), chain.params(), chain).unwrap();
    chain.append(&record(&step["block"]), &[point]).unwrap();
    verify_step(chain, step);
}
#[test]
fn verified_blocks_vm_balances_archives_tip_reopen_and_replacement_branch_are_atomic() {
    let (g, steps) = synthetic();
    let p = params(&g);
    let a = assets(&g);
    let dir = tempfile::tempdir().unwrap();
    let mut chain = ChainState::open(dir.path(), p.clone()).unwrap();
    assert!(chain.append(&record(&steps[0]["block"]), &[]).is_err());
    chain.initialize(&a).unwrap();
    for step in &steps[..2] {
        let old_tip = chain.tip().unwrap();
        let candidate = record(&step["block"]);
        assert!(chain.append(&candidate, &[]).is_err());
        let mut corrupt = candidate.clone();
        corrupt.fields.insert(
            "content_hash".into(),
            mmx_consensus::Value::Bytes(vec![0; 32]),
        );
        assert!(chain.append(&corrupt, &[]).is_err());
        assert_eq!(chain.tip().unwrap(), old_tip);
        assert!(!chain.storage().database().has_pending());
        append(&mut chain, step);
        drop(chain);
        chain = ChainState::open(dir.path(), p.clone()).unwrap();
        verify_step(&chain, step);
    }
    let old = record(&steps[1]["block"]);
    let old_hash = old
        .field("hash")
        .unwrap()
        .bytes()
        .unwrap()
        .try_into()
        .unwrap();
    let old_tx = old.field("tx_list").unwrap().seq().unwrap()[0]
        .record()
        .unwrap();
    let old_id = old_tx
        .field("id")
        .unwrap()
        .bytes()
        .unwrap()
        .try_into()
        .unwrap();
    chain.revert(2).unwrap();
    verify_step(&chain, &steps[0]);
    assert!(chain.block(&old_hash).unwrap().is_none());
    assert!(chain.transaction(&old_id).unwrap().is_none());
    assert!(chain
        .storage()
        .get_transaction_index(&old_id)
        .unwrap()
        .is_none());
    drop(chain);
    chain = ChainState::open(dir.path(), p.clone()).unwrap();
    verify_step(&chain, &steps[0]);
    append(&mut chain, &steps[2]);
    assert_ne!(chain.tip().unwrap().unwrap().hash.0, old_hash);
    assert!(chain.transaction(&old_id).unwrap().is_none());
    drop(chain);
    chain = ChainState::open(dir.path(), p.clone()).unwrap();
    verify_step(&chain, &steps[2]);
    chain.revert(0).unwrap();
    assert!(chain.tip().unwrap().is_none());
    drop(chain);
    chain = ChainState::open(dir.path(), p).unwrap();
    chain.initialize(&a).unwrap();
    verify_step(&chain, &g);
}

#[test]
fn torn_extension_recovers_ledger_tip_and_archives_together() {
    let (g, steps) = synthetic();
    let p = params(&g);
    let dir = tempfile::tempdir().unwrap();
    let mut chain = ChainState::open(dir.path(), p.clone()).unwrap();
    chain.initialize(&assets(&g)).unwrap();
    append(&mut chain, &steps[0]);
    let prefix = fs::metadata(dir.path().join("journal.mmx")).unwrap().len() as usize;
    append(&mut chain, &steps[1]);
    drop(chain);
    let data = fs::read(dir.path().join("journal.mmx")).unwrap();
    for cut in [
        prefix,
        prefix + 1,
        prefix + 7,
        prefix + 11,
        prefix + 12,
        (prefix + data.len()) / 2,
        data.len() - 1,
        data.len(),
    ] {
        let copy = tempfile::tempdir().unwrap();
        fs::write(copy.path().join("journal.mmx"), &data[..cut]).unwrap();
        let recovered = ChainState::open(copy.path(), p.clone()).unwrap();
        verify_step(&recovered, &steps[usize::from(cut == data.len())]);
    }
}

#[test]
fn invalid_ledger_effects_cannot_stage_chain_metadata_or_lose_existing_pending_writes() {
    use mmx_db::Write;
    use mmx_vm::{
        storage::{BalanceWrite, StorageWrite, TransactionBatch},
        StorageDb, Value, MEM_STACK,
    };
    let mut db = StorageDb::memory();
    db.set_balance(&[1; 32], &[0; 32], 100).unwrap();
    db.database_mut().commit(1).unwrap();
    db.database_mut()
        .insert("unrelated", b"pending", b"original")
        .unwrap();
    let invalid = TransactionBatch {
        writes: vec![(
            [1; 32],
            StorageWrite {
                address: MEM_STATIC,
                key: None,
                var: Var::new(Value::Ref(MEM_STACK)),
            },
        )],
        balances: vec![BalanceWrite {
            contract: [1; 32],
            currency: [0; 32],
            amount: 200,
        }],
        ..Default::default()
    };
    assert!(db
        .commit_transaction(&invalid, [Write::put("chain_meta", b"tip", [9; 32])], 2)
        .is_err());
    assert_eq!(db.database().version(), 1);
    assert_eq!(db.get_balance(&[1; 32], &[0; 32]).unwrap(), Some(100));
    assert_eq!(
        db.database().get("unrelated", b"pending", None),
        Some(b"original".as_slice())
    );
    assert!(db.database().get("chain_meta", b"tip", None).is_none());
}

#[test]
fn reopen_rejects_a_tip_that_does_not_match_the_journal_version() {
    let (g, _) = synthetic();
    let p = params(&g);
    let dir = tempfile::tempdir().unwrap();
    let mut chain = ChainState::open(dir.path(), p.clone()).unwrap();
    chain.initialize(&assets(&g)).unwrap();
    drop(chain);
    // Simulate a foreign writer advancing the shared DB outside ChainState.
    let mut raw = mmx_vm::StorageDb::open(dir.path()).unwrap();
    raw.database_mut().commit(2).unwrap();
    drop(raw);
    assert!(ChainState::open(dir.path(), p).is_err());
}
