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
        db.database().get("unrelated", b"pending", None).unwrap(),
        Some(b"original".to_vec())
    );
    assert!(db
        .database()
        .get("chain_meta", b"tip", None)
        .unwrap()
        .is_none());
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

fn block_hash(step: &Value) -> Address {
    record(&step["block"])
        .field("hash")
        .unwrap()
        .bytes()
        .unwrap()
        .try_into()
        .unwrap()
}
fn retain(chain: &mut mmx_execution::chain::forks::ForkChain, step: &Value) {
    chain
        .admit(&record(&step["block"]), &[record(&step["vdf"])])
        .unwrap();
}

#[test]
fn orphans_arrive_before_parents_reopen_and_switch_to_native_hash_winner() {
    use mmx_execution::chain::forks::ForkChain;
    let (g, steps) = synthetic();
    let p = params(&g);
    let root = block_hash(&g);
    let (winner, loser) = if block_hash(&steps[1]) < block_hash(&steps[2]) {
        (&steps[1], &steps[2])
    } else {
        (&steps[2], &steps[1])
    };
    let dir = tempfile::tempdir().unwrap();
    let mut chain = ForkChain::open(dir.path(), p.clone()).unwrap();
    chain.initialize(&assets(&g)).unwrap();
    retain(&mut chain, loser);
    let update = chain.update(root, None).unwrap();
    assert!(update.best.is_none());
    assert!(update.waiting.contains(&block_hash(loser)));
    verify_step(chain.selected(), &g);
    drop(chain);
    let mut chain = ForkChain::open(dir.path(), p.clone()).unwrap();
    assert_eq!(
        chain.block(&block_hash(loser)).unwrap(),
        Some(record(&loser["block"]))
    );
    chain.admit(&record(&steps[0]["block"]), &[]).unwrap();
    assert!(chain.update(root, None).unwrap().best.is_none());
    retain(&mut chain, &steps[0]);
    let update = chain.update(root, None).unwrap();
    assert_eq!(update.applied, [block_hash(&steps[0]), block_hash(loser)]);
    verify_step(chain.selected(), loser);
    retain(&mut chain, winner);
    retain(&mut chain, winner); // repeated bytes cannot change arrival order
    let update = chain.update(root, None).unwrap();
    assert_eq!(update.common_ancestor, Some(block_hash(&steps[0])));
    assert_eq!(update.applied, [block_hash(winner)]);
    verify_step(chain.selected(), winner);
    assert!(chain
        .selected()
        .block(&block_hash(loser))
        .unwrap()
        .is_none());
    assert_eq!(
        chain.block(&block_hash(loser)).unwrap(),
        Some(record(&loser["block"]))
    );
    let old_tx = record(&loser["block"])
        .field("tx_list")
        .unwrap()
        .seq()
        .unwrap()[0]
        .record()
        .unwrap()
        .clone();
    let id = old_tx
        .field("id")
        .unwrap()
        .bytes()
        .unwrap()
        .try_into()
        .unwrap();
    assert!(chain.selected().transaction(&id).unwrap().is_none());
    assert!(chain
        .selected()
        .storage()
        .get_transaction_index(&id)
        .unwrap()
        .is_none());
    drop(chain);
    let mut chain = ForkChain::open(dir.path(), p.clone()).unwrap();
    verify_step(chain.selected(), winner);
    assert!(chain.update(root, None).unwrap().applied.is_empty());
    assert!(chain.update(block_hash(loser), None).is_err());
    drop(chain);
    let mut wrong = p;
    wrong.network.push_str("wrong");
    assert!(ForkChain::open(dir.path(), wrong).is_err());
}

#[test]
fn invalid_branch_body_is_excluded_without_reverting_selected_state() {
    use mmx_execution::chain::forks::ForkChain;
    let (g, steps) = synthetic();
    let bad = rows()
        .into_iter()
        .find(|r| r["kind"] == "fork_rejection")
        .unwrap();
    assert_ne!(bad["result"], "OK");
    let dir = tempfile::tempdir().unwrap();
    let mut chain = ForkChain::open(dir.path(), params(&g)).unwrap();
    chain.initialize(&assets(&g)).unwrap();
    retain(&mut chain, &steps[0]);
    chain.update(block_hash(&g), None).unwrap();
    retain(&mut chain, &bad);
    let update = chain.update(block_hash(&g), None).unwrap();
    assert!(update.invalid.contains(&block_hash(&bad)));
    verify_step(chain.selected(), &steps[0]);
    drop(chain);
    let mut chain = ForkChain::open(dir.path(), params(&g)).unwrap();
    let update = chain.update(block_hash(&g), None).unwrap();
    assert!(update.invalid.contains(&block_hash(&bad)));
    assert!(update.applied.is_empty());
    retain(&mut chain, &steps[2]);
    chain.update(block_hash(&g), None).unwrap();
    verify_step(chain.selected(), &steps[2]);
}

#[test]
fn alternate_root_waits_for_live_work_and_backwards_sync_then_replays_without_old_pots() {
    use mmx_execution::chain::forks::ForkChain;
    let (g, steps) = synthetic();
    let deep = rows()
        .into_iter()
        .find(|r| r["kind"] == "deep_fork")
        .unwrap()["steps"]
        .as_array()
        .unwrap()
        .clone();
    let dir = tempfile::tempdir().unwrap();
    let mut chain = ForkChain::open(dir.path(), params(&g)).unwrap();
    chain.initialize(&assets(&g)).unwrap();
    retain(&mut chain, &steps[0]);
    retain(&mut chain, &steps[1]);
    chain.update(block_hash(&g), None).unwrap();
    verify_step(chain.selected(), &steps[1]);
    chain.admit(&record(&deep[0]["block"]), &[]).unwrap();
    chain.admit(&record(&deep[1]["block"]), &[]).unwrap();
    let update = chain.update(block_hash(&steps[0]), None).unwrap();
    assert!(update.waiting.contains(&block_hash(&deep[1])));
    assert!(!update.invalid.contains(&block_hash(&deep[1])));
    verify_step(chain.selected(), &steps[1]);
    drop(chain);
    let mut chain = ForkChain::open(dir.path(), params(&g)).unwrap();
    retain(&mut chain, &deep[1]);
    chain.update(block_hash(&steps[0]), Some(0)).unwrap();
    verify_step(chain.selected(), &steps[1]);
    let update = chain.update(block_hash(&steps[0]), None).unwrap();
    assert_eq!(update.common_ancestor, Some(block_hash(&g)));
    assert_eq!(update.applied, [block_hash(&deep[0]), block_hash(&deep[1])]);
    verify_step(chain.selected(), &deep[1]);
    assert!(chain.block(&block_hash(&steps[0])).unwrap().is_some());
    assert!(chain.block(&block_hash(&steps[1])).unwrap().is_some());
    drop(chain);
    let mut chain = ForkChain::open(dir.path(), params(&g)).unwrap();
    verify_step(chain.selected(), &deep[1]);
    assert!(chain
        .update(block_hash(&deep[0]), None)
        .unwrap()
        .applied
        .is_empty());
}

#[test]
fn archive_write_ahead_recovers_initialization_and_binds_exact_genesis() {
    use mmx_execution::chain::forks::ForkChain;
    let (g, steps) = synthetic();
    let dir = tempfile::tempdir().unwrap();
    let mut original = ForkChain::open(dir.path(), params(&g)).unwrap();
    original.initialize(&assets(&g)).unwrap();
    drop(original);
    let copy = tempfile::tempdir().unwrap();
    fs::create_dir(copy.path().join("archive")).unwrap();
    fs::copy(
        dir.path().join("archive/journal.mmx"),
        copy.path().join("archive/journal.mmx"),
    )
    .unwrap();
    let mut chain = ForkChain::open(copy.path(), params(&g)).unwrap();
    assert!(chain.selected().tip().unwrap().is_none());
    let mut different = assets(&g);
    let tx = &mut different.binaries[8];
    tx.fields
        .insert("nonce".into(), mmx_consensus::Value::Uint(9999));
    tx.fields.insert(
        "id".into(),
        mmx_consensus::Value::Bytes(tx.calc_hash(false, 0).unwrap().to_vec()),
    );
    tx.fields.insert(
        "content_hash".into(),
        mmx_consensus::Value::Bytes(tx.calc_hash(true, 0).unwrap().to_vec()),
    );
    assert!(chain.initialize(&different).is_err());
    chain.initialize(&assets(&g)).unwrap();
    verify_step(chain.selected(), &g);
    let mut tampered = record(&steps[0]["block"]);
    tampered.fields.insert(
        "content_hash".into(),
        mmx_consensus::Value::Bytes(vec![0; 32]),
    );
    assert!(chain.admit(&tampered, &[]).is_err());
    assert!(chain.block(&block_hash(&steps[0])).unwrap().is_none());
    assert!(chain.admit(&record(&g["block"]), &[]).is_err());
    verify_step(chain.selected(), &g);
}

#[test]
fn speculative_replacement_cannot_publish_a_successful_prefix_before_failure() {
    let (g, steps) = synthetic();
    let mut chain = ChainState::memory(params(&g));
    chain.initialize(&assets(&g)).unwrap();
    append(&mut chain, &steps[0]);
    append(&mut chain, &steps[1]);
    let first = vdf::verify(&record(&steps[0]["vdf"]), chain.params(), &chain).unwrap();
    let second = vdf::verify(&record(&steps[2]["vdf"]), chain.params(), &chain).unwrap();
    let mut bad = record(&steps[2]["block"]);
    bad.fields.insert(
        "content_hash".into(),
        mmx_consensus::Value::Bytes(vec![0; 32]),
    );
    assert!(chain
        .reorganize(
            1,
            &[
                (record(&steps[0]["block"]), vec![first.clone()]),
                (bad, vec![second.clone()])
            ]
        )
        .is_err());
    verify_step(&chain, &steps[1]);
    chain
        .reorganize(
            1,
            &[
                (record(&steps[0]["block"]), vec![first]),
                (record(&steps[2]["block"]), vec![second]),
            ],
        )
        .unwrap();
    verify_step(&chain, &steps[2]);
    assert!(chain.reorganize(0, &[]).is_err());
}

#[test]
fn torn_branch_switch_keeps_orphans_and_recovers_old_or_new_ledger_together() {
    use mmx_execution::chain::forks::ForkChain;
    let (g, steps) = synthetic();
    let root = block_hash(&g);
    let (winner, loser) = if block_hash(&steps[1]) < block_hash(&steps[2]) {
        (&steps[1], &steps[2])
    } else {
        (&steps[2], &steps[1])
    };
    let dir = tempfile::tempdir().unwrap();
    let mut chain = ForkChain::open(dir.path(), params(&g)).unwrap();
    chain.initialize(&assets(&g)).unwrap();
    retain(&mut chain, &steps[0]);
    retain(&mut chain, loser);
    chain.update(root, None).unwrap();
    let prefix = fs::metadata(dir.path().join("selected/journal.mmx"))
        .unwrap()
        .len() as usize;
    retain(&mut chain, winner);
    chain.update(root, None).unwrap();
    drop(chain);
    let data = fs::read(dir.path().join("selected/journal.mmx")).unwrap();
    let archive = fs::read(dir.path().join("archive/journal.mmx")).unwrap();
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
        fs::create_dir(copy.path().join("selected")).unwrap();
        fs::create_dir(copy.path().join("archive")).unwrap();
        fs::write(copy.path().join("selected/journal.mmx"), &data[..cut]).unwrap();
        fs::write(copy.path().join("archive/journal.mmx"), &archive).unwrap();
        let mut recovered = ForkChain::open(copy.path(), params(&g)).unwrap();
        verify_step(
            recovered.selected(),
            if cut == data.len() { winner } else { loser },
        );
        assert!(recovered.block(&block_hash(loser)).unwrap().is_some());
        assert!(recovered.block(&block_hash(winner)).unwrap().is_some());
        recovered.update(root, None).unwrap();
        verify_step(recovered.selected(), winner);
    }
}

fn signed_vote(step: &Value, key: u64) -> Record {
    include_str!("fixtures/votes.jsonl")
        .lines()
        .map(|l| vnx::serde_json::from_str::<Value>(l).unwrap())
        .find(|r| {
            r["kind"] == "chain_vote" && r["key"] == key && address(&r["block"]) == block_hash(step)
        })
        .map(|r| record(&r["wire"]))
        .unwrap()
}
#[test]
fn authenticated_votes_switch_tied_branches_and_reset_after_restart() {
    use mmx_execution::chain::{forks::ForkChain, voting::Disposition};
    let (g, steps) = synthetic();
    let root = block_hash(&g);
    let (winner, loser) = if block_hash(&steps[1]) < block_hash(&steps[2]) {
        (&steps[1], &steps[2])
    } else {
        (&steps[2], &steps[1])
    };
    let dir = tempfile::tempdir().unwrap();
    let mut chain = ForkChain::open(dir.path(), params(&g)).unwrap();
    chain.initialize(&assets(&g)).unwrap();
    retain(&mut chain, &steps[0]);
    retain(&mut chain, winner);
    retain(&mut chain, loser);
    chain.update_at(root, None, 100).unwrap();
    verify_step(chain.selected(), winner);
    let unauthorized = chain.admit_vote(&signed_vote(loser, 2), 100).unwrap();
    assert_eq!(unauthorized.processed[0].1, Disposition::NotValidator);
    let accepted = chain.admit_vote(&signed_vote(loser, 1), 100).unwrap();
    assert_eq!(accepted.processed[0].1, Disposition::Accepted);
    assert_eq!(
        chain
            .admit_vote(&signed_vote(loser, 1), 101)
            .unwrap()
            .processed[0]
            .1,
        Disposition::Duplicate
    );
    assert_eq!(chain.vote_count(&block_hash(loser)), 1);
    chain.update_at(root, None, 102).unwrap();
    verify_step(chain.selected(), loser);
    let mut invalid = signed_vote(winner, 1);
    invalid.fields.insert(
        "content_hash".into(),
        mmx_consensus::Value::Bytes(vec![0; 32]),
    );
    assert!(chain.admit_vote(&invalid, 103).is_err());
    assert_eq!(chain.vote_count(&block_hash(winner)), 0);
    drop(chain);
    let mut chain = ForkChain::open(dir.path(), params(&g)).unwrap();
    verify_step(chain.selected(), loser);
    assert_eq!(chain.vote_count(&block_hash(loser)), 0);
    chain.update_at(root, None, 104).unwrap();
    verify_step(chain.selected(), winner);
}
#[test]
fn votes_wait_for_verified_blocks_and_expire_only_while_pending() {
    use mmx_execution::chain::{forks::ForkChain, voting::Disposition};
    let (g, steps) = synthetic();
    let root = block_hash(&g);
    let interval = params(&g).block_interval_ms;
    let mut chain = ForkChain::memory(params(&g));
    chain.initialize(&assets(&g)).unwrap();
    assert_eq!(
        chain
            .admit_vote(&signed_vote(&steps[1], 1), 100)
            .unwrap()
            .pending,
        1
    );
    retain(&mut chain, &steps[0]);
    chain.admit(&record(&steps[1]["block"]), &[]).unwrap();
    let update = chain.update_at(root, None, 100 + 3 * interval).unwrap();
    assert_eq!(update.votes.pending, 1);
    assert_eq!(chain.vote_count(&block_hash(&steps[1])), 0);
    retain(&mut chain, &steps[1]);
    let update = chain.update_at(root, None, 100 + 3 * interval).unwrap();
    assert_eq!(update.votes.processed[0].1, Disposition::Accepted);
    assert_eq!(chain.vote_count(&block_hash(&steps[1])), 1);
    chain.process_votes(100 + 30 * interval);
    assert_eq!(chain.vote_count(&block_hash(&steps[1])), 1);
    let deep = rows()
        .into_iter()
        .find(|r| r["kind"] == "deep_fork")
        .unwrap()["steps"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(
        chain
            .admit_vote(&signed_vote(&deep[1], 1), 200 + 30 * interval)
            .unwrap()
            .pending,
        1
    );
    let expired = chain.process_votes(201 + 33 * interval);
    assert_eq!(expired.processed[0].1, Disposition::Expired);
    retain(&mut chain, &deep[0]);
    retain(&mut chain, &deep[1]);
    chain.update_at(root, None, 201 + 33 * interval).unwrap();
    assert_eq!(chain.vote_count(&block_hash(&deep[1])), 0);
}
#[test]
fn rollback_to_every_selected_height_survives_reopen_and_retains_all_orphans() {
    use mmx_execution::chain::forks::ForkChain;
    let (g, steps) = synthetic();
    let dir = tempfile::tempdir().unwrap();
    let root = block_hash(&g);
    let mut chain = ForkChain::open(dir.path(), params(&g)).unwrap();
    chain.initialize(&assets(&g)).unwrap();
    retain(&mut chain, &steps[0]);
    retain(&mut chain, &steps[1]);
    chain.update_at(root, None, 0).unwrap();
    assert!(chain.rollback_to(3).is_err());
    for height in (0..=2).rev() {
        chain.rollback_to(height).unwrap();
        if height < 2 {
            // Rewind must invalidate selected-header caches while the orphan
            // archive still retains the exact old signed record.
            assert!(chain
                .selected()
                .header(&block_hash(&steps[1]))
                .unwrap()
                .is_none());
        }
        verify_step(
            chain.selected(),
            if height == 0 {
                &g
            } else {
                &steps[(height - 1) as usize]
            },
        );
        assert!(chain.block(&block_hash(&steps[1])).unwrap().is_some());
        drop(chain);
        chain = ForkChain::open(dir.path(), params(&g)).unwrap();
        assert_eq!(chain.selected().tip().unwrap().unwrap().height, height);
    }
    chain.update_at(root, None, 1).unwrap();
    verify_step(chain.selected(), &steps[1]);
}

#[test]
fn historical_sync_checks_space_body_and_state_then_requires_vdf_for_new_live_blocks() {
    use mmx_execution::chain::forks::ForkChain;
    let (g, steps) = synthetic();
    let root = block_hash(&g);
    let mut selected = ChainState::memory(params(&g));
    selected.initialize(&assets(&g)).unwrap();
    assert!(selected.append(&record(&steps[0]["block"]), &[]).is_err());
    verify_step(&selected, &g);
    selected.append_sync(&record(&steps[0]["block"])).unwrap();
    verify_step(&selected, &steps[0]);
    let mut forks = ForkChain::memory(params(&g));
    forks.initialize(&assets(&g)).unwrap();
    forks.admit(&record(&steps[0]["block"]), &[]).unwrap();
    assert!(forks.update_at(root, None, 0).unwrap().best.is_none());
    forks.update_sync_at(root, None, 0).unwrap();
    verify_step(forks.selected(), &steps[0]);
    // A signed header with invalid declared execution cost still fails during sync.
    let bad = rows()
        .into_iter()
        .find(|r| r["kind"] == "fork_rejection")
        .unwrap();
    forks.admit(&record(&bad["block"]), &[]).unwrap();
    let out = forks.update_sync_at(root, None, 3).unwrap();
    assert!(out.invalid.contains(&block_hash(&bad)));
    verify_step(forks.selected(), &steps[0]);
    forks.admit(&record(&steps[1]["block"]), &[]).unwrap();
    let live = forks.update_at(root, None, 1).unwrap();
    assert!(live.waiting.contains(&block_hash(&steps[1])));
    verify_step(forks.selected(), &steps[0]);
    retain(&mut forks, &steps[1]);
    forks.update_at(root, None, 2).unwrap();
    verify_step(forks.selected(), &steps[1]);
}

#[test]
fn download_archive_batches_are_atomic_and_preserve_existing_work_and_first_records() {
    use mmx_execution::chain::forks::ForkChain;
    let (g, steps) = synthetic();
    let dir = tempfile::tempdir().unwrap();
    let mut chain = ForkChain::open_indexed(dir.path(), params(&g)).unwrap();
    chain.initialize(&assets(&g)).unwrap();
    let mut bad = record(&steps[1]["block"]);
    bad.fields.insert(
        "content_hash".into(),
        mmx_consensus::Value::Bytes(vec![0; 32]),
    );
    assert!(chain
        .admit_blocks(&[record(&steps[0]["block"]), bad])
        .is_err());
    assert!(chain.block(&block_hash(&steps[0])).unwrap().is_none());
    verify_step(chain.selected(), &g);
    retain(&mut chain, &steps[0]);
    chain
        .admit_blocks(&[
            record(&steps[0]["block"]),
            record(&steps[1]["block"]),
            record(&steps[0]["block"]),
        ])
        .unwrap();
    chain.update_at(block_hash(&g), None, 0).unwrap();
    verify_step(chain.selected(), &steps[0]);
    retain(&mut chain, &steps[1]);
    chain.update_at(block_hash(&g), None, 1).unwrap();
    verify_step(chain.selected(), &steps[1]);
    drop(chain);
    let mut chain = ForkChain::open_indexed(dir.path(), params(&g)).unwrap();
    verify_step(chain.selected(), &steps[1]);
    chain.rollback_to(0).unwrap();
    chain.update_sync_at(block_hash(&g), None, 2).unwrap();
    verify_step(chain.selected(), &steps[1]);
}

#[test]
fn verified_header_evidence_binds_unhashed_fields_parameters_and_ancestry_context() {
    use mmx_consensus::{
        transitions::{self, ProofContext},
        Value as V,
    };
    let (g, steps) = synthetic();
    let p = params(&g);
    let mut chain = ChainState::memory(p.clone());
    chain.initialize(&assets(&g)).unwrap();
    let block = record(&steps[0]["block"]);
    let context = ProofContext::resolve(&block, &p, &chain).unwrap();
    let evidence = transitions::verify_evidence(&block, &p, &context).unwrap();
    assert!(evidence.matches(&block, &p, &context));
    let mut changed = block.clone();
    let mut proof_chain = changed
        .field("proof_chain")
        .unwrap()
        .bytes()
        .unwrap()
        .to_vec();
    proof_chain[0] ^= 1;
    changed
        .fields
        .insert("proof_chain".into(), V::Bytes(proof_chain));
    // The first fixture block precedes hardfork2: proof_chain is not committed by
    // its legacy header hash. Evidence still binds the exact field value.
    assert_eq!(
        mmx_consensus::header::calc_hash(&changed, &p).unwrap(),
        mmx_consensus::header::calc_hash(&block, &p).unwrap()
    );
    assert!(!evidence.matches(&changed, &p, &context));
    let mut other = p.clone();
    other.network.push('x');
    assert!(!evidence.matches(&block, &other, &context));
    let mut ancestry = context.clone();
    ancestry.iteration_counts[0] += 1;
    assert!(!evidence.matches(&block, &p, &ancestry));
    let mut ancestor = context.clone();
    ancestor.parent.hash.0[0] ^= 1;
    assert!(!evidence.matches(&block, &p, &ancestor));
    let mut invalid = block.clone();
    if let V::Sequence(proofs) = invalid.fields.get_mut("proof").unwrap() {
        if let V::Record(proof) = &mut proofs[0] {
            proof
                .fields
                .insert("difficulty".into(), V::Uint(context.space_diff + 1));
        }
    }
    assert!(transitions::verify_evidence(&invalid, &p, &context).is_err());
    assert!(!evidence.matches(&invalid, &p, &context));
    assert!(evidence.matches(&block, &p, &context));
}
