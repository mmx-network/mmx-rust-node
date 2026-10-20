//! Fixtures run the original Node::validate(Block), dependency preparation and
//! transaction methods. Only task scheduling is made serial and chain lookups
//! are supplied by the harness; header proof verification remains external.
use mmx_consensus::Record;
use mmx_execution::block::{self, BlockContext};
use mmx_types::{mmx, VnxType};
use mmx_vm::{
    storage::{AtomicStorage, BalanceWrite, ContractData, TransactionBatch, TransactionIndex},
    Address, Output, Storage, StorageDb,
};
use std::collections::{BTreeMap, BTreeSet};
use vnx::serde_json::{self, Value};

fn unhex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn address(value: &Value) -> Address {
    unhex(value.as_str().unwrap()).try_into().unwrap()
}
fn record(value: &Value) -> Record {
    Record::decode(&unhex(value.as_str().unwrap())).unwrap()
}
fn context(seed: &Value) -> BlockContext {
    BlockContext {
        params: mmx::ChainParams::decode(&unhex(seed["params"].as_str().unwrap())).unwrap(),
        previous: mmx::BlockHeader::decode(&unhex(seed["previous"].as_str().unwrap())).unwrap(),
        expected_vdf_payout: seed["expected_payout"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| unhex(s).try_into().unwrap()),
    }
}
fn setup(db: &mut StorageDb, seed: &Value) {
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
                bytes: unhex(c["wire"].as_str().unwrap()),
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
    for s in seed["supplies"].as_array().unwrap() {
        batch.supplies.push((
            address(&s["currency"]),
            s["amount"].as_str().unwrap().parse().unwrap(),
        ));
    }
    db.apply_transaction(&batch).unwrap();
    db.database_mut().commit(10).unwrap();
}
fn output(value: &Value) -> Output {
    Output {
        address: address(&value["address"]),
        contract: address(&value["currency"]),
        amount: value["amount"].as_str().unwrap().parse().unwrap(),
        memo: value["memo_present"]
            .as_bool()
            .unwrap()
            .then(|| unhex(value["memo"].as_str().unwrap())),
    }
}
fn supplies(value: &Value) -> BTreeMap<Address, u128> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                address(&v["currency"]),
                v["amount"].as_str().unwrap().parse().unwrap(),
            )
        })
        .collect()
}
fn seed(name: &str) -> Value {
    include_str!("fixtures/blocks.jsonl")
        .lines()
        .map(|s| serde_json::from_str::<Value>(s).unwrap())
        .find(|v| v["name"] == name)
        .unwrap()
}
fn verify_settlement(db: &StorageDb, trace: &Value) {
    for b in trace["settled"].as_array().unwrap() {
        assert_eq!(
            db.get_balance(&address(&b["address"]), &address(&b["currency"]))
                .unwrap(),
            Some(b["amount"].as_str().unwrap().parse().unwrap()),
            "{}: {b}",
            trace["name"]
        );
    }
    for (currency, amount) in supplies(&trace["settled_supplies"]) {
        assert_eq!(
            db.get_total_supply(&currency).unwrap(),
            Some(amount),
            "{}: supply",
            trace["name"]
        );
    }
}
#[test]
fn native_block_reservations_execution_order_rewards_and_atomic_settlement() {
    let mut count = 0;
    let mut accepted = 0;
    for line in include_str!("fixtures/blocks.jsonl").lines() {
        let trace: Value = serde_json::from_str(line).unwrap();
        let name = trace["name"].as_str().unwrap();
        let block = record(&trace["block"]);
        let ctx = context(&trace);
        let mut db = StorageDb::memory();
        setup(&mut db, &trace);
        let prepared = block::prepare(&mut db, &ctx, &block);
        if !trace["accepted"].as_bool().unwrap() {
            if let Ok(prepared) = prepared {
                panic!(
                    "{name}: accepted invalid block, native error: {}; Rust results: {:?}",
                    trace["error"],
                    prepared.receipt().results
                );
            }
            assert!(
                !db.database().has_pending(),
                "{name}: rejection published writes"
            );
        } else {
            accepted += 1;
            let prepared = prepared.unwrap_or_else(|e| {
                panic!("{name}: {e}, info={:?}, cost={}", e.info, e.total_cost)
            });
            assert!(
                !prepared.storage().database().has_pending(),
                "{name}: prepare published writes"
            );
            let receipt = prepared.receipt();
            assert_eq!(
                receipt.inputs,
                trace["inputs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(output)
                    .collect::<Vec<_>>(),
                "{name}: inputs"
            );
            assert_eq!(
                receipt.outputs,
                trace["outputs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(output)
                    .collect::<Vec<_>>(),
                "{name}: outputs"
            );
            assert_eq!(
                receipt.supply_delta,
                supplies(&trace["supply_delta"]),
                "{name}: issuance"
            );
            let txs = block.field("tx_list").unwrap().seq().unwrap();
            let ids: Vec<_> = txs
                .iter()
                .map(|v| hex(v.record().unwrap().field("id").unwrap().bytes().unwrap()))
                .collect();
            for (index, expected) in trace["plan"].as_array().unwrap().iter().enumerate() {
                let actual: BTreeSet<_> =
                    receipt.plan.locks[index].iter().map(|a| hex(a)).collect();
                let expected_locks: BTreeSet<_> = expected["locks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| s.as_str().unwrap().to_owned())
                    .collect();
                assert_eq!(actual, expected_locks, "{name}: locks {index}");
                let actual: BTreeSet<_> = receipt.plan.predecessors[index]
                    .iter()
                    .map(|i| ids[*i].clone())
                    .collect();
                let predecessors: BTreeSet<_> = expected["predecessors"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| s.as_str().unwrap().to_owned())
                    .collect();
                assert_eq!(actual, predecessors, "{name}: predecessors {index}");
            }
            let actual: BTreeMap<_, _> = prepared
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
            let expected: BTreeMap<_, _> = trace["writes"]
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
            assert_eq!(actual, expected, "{name}: state writes");
            prepared.commit().unwrap();
            verify_settlement(&db, &trace);
            for id in ids {
                assert_eq!(
                    db.get_transaction_index(&unhex(&id).try_into().unwrap())
                        .unwrap()
                        .unwrap()
                        .height,
                    ctx.previous.height + 1
                );
            }
        }
        count += 1;
    }
    assert_eq!(count, 120);
    assert!(accepted >= 40, "{count} cases, {accepted} accepted");
}
#[test]
fn parallel_and_original_serial_execution_match_every_native_block_fixture() {
    let serial = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    for line in include_str!("fixtures/blocks.jsonl").lines() {
        let trace: Value = serde_json::from_str(line).unwrap();
        let candidate = record(&trace["block"]);
        let ctx = context(&trace);
        let summarize = |db: &mut StorageDb| match block::prepare(db, &ctx, &candidate) {
            Ok(prepared) => Ok((
                format!("{:?}", prepared.receipt()),
                format!("{:?}", prepared.batch()),
            )),
            Err(rejected) => Err((
                rejected.transaction,
                rejected.info,
                rejected.total_cost,
                rejected.error.to_string(),
            )),
        };
        let mut a = StorageDb::memory();
        let mut b = StorageDb::memory();
        setup(&mut a, &trace);
        setup(&mut b, &trace);
        let expected = serial.install(|| summarize(&mut a));
        let actual = summarize(&mut b);
        assert_eq!(actual, expected, "{}", trace["name"]);
        assert!(!a.database().has_pending());
        assert!(!b.database().has_pending());
    }
}
#[test]
fn block_prepare_abort_durable_commit_reopen_and_fork() {
    let trace = seed("shared_state_v1");
    let block = record(&trace["block"]);
    let ctx = context(&trace);
    let txid: Address = block.field("tx_list").unwrap().seq().unwrap()[0]
        .record()
        .unwrap()
        .field("id")
        .unwrap()
        .bytes()
        .unwrap()
        .try_into()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    {
        let mut db = StorageDb::open(dir.path()).unwrap();
        setup(&mut db, &trace);
        drop(block::prepare(&mut db, &ctx, &block).unwrap());
        assert!(!db.database().has_pending());
        assert!(db.get_transaction_index(&txid).unwrap().is_none());
        block::execute(&mut db, &ctx, &block).unwrap();
        verify_settlement(&db, &trace);
        db.database_mut().commit(11).unwrap();
    }
    {
        let mut db = StorageDb::open(dir.path()).unwrap();
        verify_settlement(&db, &trace);
        assert!(db.get_transaction_index(&txid).unwrap().is_some());
        db.database_mut().revert(10).unwrap();
        assert!(db.get_transaction_index(&txid).unwrap().is_none());
        assert!(db
            .get_total_supply(&[
                1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0
            ])
            .unwrap()
            .is_none());
        block::execute(&mut db, &ctx, &block).unwrap();
        db.database_mut().commit(11).unwrap();
    }
    verify_settlement(&StorageDb::open(dir.path()).unwrap(), &trace);
}
#[test]
fn later_transaction_rejection_preserves_existing_pending_writes() {
    let trace = seed("forged_isolated_state_v1");
    assert_eq!(trace["accepted"], false);
    let mut db = StorageDb::memory();
    setup(&mut db, &trace);
    db.set_balance(&[99; 32], &[0; 32], 123).unwrap();
    let rejection = match block::prepare(&mut db, &context(&trace), &record(&trace["block"])) {
        Err(e) => e,
        Ok(_) => panic!("accepted forged results"),
    };
    assert_eq!(rejection.transaction, Some(1));
    assert!(db.database().has_pending());
    assert_eq!(db.get_balance(&[99; 32], &[0; 32]).unwrap(), Some(123));
    let mut contract = [0; 32];
    contract[0] = 1;
    assert!(db
        .read(&contract, mmx_vm::MEM_STATIC, None)
        .unwrap()
        .is_none());
}

#[test]
fn header_tampering_and_invalid_farmer_signatures_publish_nothing() {
    use mmx_consensus::{header, Value as N};
    let trace = seed("shared_state_v1");
    let original = record(&trace["block"]);
    let ctx = context(&trace);
    let mut db = StorageDb::memory();
    setup(&mut db, &trace);
    for field in [
        "hash",
        "content_hash",
        "nonce",
        "weight",
        "proof_chain",
        "time_stamp",
    ] {
        let mut block = original.clone();
        match block.fields.get_mut(field).unwrap() {
            N::Bytes(v) => v[0] ^= 1,
            N::Uint(v) => *v = v.wrapping_add(1),
            N::Int(v) => *v = -1,
            _ => panic!("unexpected header field"),
        }
        assert!(
            block::prepare(&mut db, &ctx, &block).is_err(),
            "{field}: accepted tampering"
        );
        assert!(!db.database().has_pending(), "{field}: published writes");
    }
    let mut block = original;
    block.fields.insert(
        "farmer_sig".into(),
        N::Optional(Some(Box::new(N::Bytes(vec![0; 64])))),
    );
    let content = header::calc_content_hash(&block).unwrap();
    block
        .fields
        .insert("content_hash".into(), N::Bytes(content.to_vec()));
    assert!(header::is_valid(&block, &ctx.params).unwrap());
    assert!(block::prepare(&mut db, &ctx, &block).is_err());
    assert!(!db.database().has_pending());
}

#[test]
fn native_economic_rounding_wrapping_activation_votes_and_reward_limits() {
    use block::economics;
    let mut count = 0;
    for (line, text) in include_str!("fixtures/economics.tsv").lines().enumerate() {
        let fields: Vec<_> = text.split('\t').collect();
        assert_eq!(fields.len(), 21);
        let n = |i: usize| fields[i].parse::<u64>().unwrap();
        let p = mmx::ChainParams {
            project_ratio: mmx::uint_fraction_t {
                value: n(0) as u32,
                inverse: n(1) as u32,
            },
            fixed_project_reward: n(2),
            min_reward: n(3),
            reward_adjust_div: n(4) as u32,
            reward_adjust_tick: n(5) as u32,
            reward_adjust_interval: n(6) as u32,
            reward_activation: n(7) as u32,
            ..Default::default()
        };
        let previous = mmx::BlockHeader {
            base_reward: n(8),
            reward_vote_sum: fields[9].parse().unwrap(),
            reward_vote_count: n(10) as u32,
            txfee_buffer: n(11),
            tx_fees: n(12),
            ..Default::default()
        };
        assert_eq!(
            economics::project_reward(&p, n(15)).unwrap(),
            n(16),
            "case {line}: project"
        );
        assert_eq!(
            economics::min_reward_deduction(n(11)),
            n(17),
            "case {line}: deduction"
        );
        assert_eq!(
            economics::new_txfee_buffer(&previous),
            n(18),
            "case {line}: buffer"
        );
        assert_eq!(
            economics::new_base_reward(&p, &previous).unwrap(),
            n(19),
            "case {line}: votes"
        );
        assert_eq!(
            economics::block_reward(&p, &previous, n(13) as u32, n(14) != 0, n(15)).unwrap(),
            n(20),
            "case {line}: reward"
        );
        count += 1;
    }
    assert_eq!(count, 468);
}

#[test]
fn native_vdf_reward_winners_counts_ties_oldest_salt_and_short_history() {
    let mut count = 0;
    for (line, text) in include_str!("fixtures/vdf_winners.jsonl")
        .lines()
        .enumerate()
    {
        let trace: Value = serde_json::from_str(text).unwrap();
        let history: Vec<_> = trace["history"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| mmx::BlockHeader::decode(&unhex(v.as_str().unwrap())).unwrap())
            .collect();
        let expected = trace["winner"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| unhex(s).try_into().unwrap());
        assert_eq!(
            block::economics::vdf_reward_winner(
                trace["interval"].as_u64().unwrap() as u32,
                &history
            ),
            expected,
            "case {line}"
        );
        count += 1;
    }
    assert_eq!(count, 80);
}

#[test]
fn assembled_candidates_pass_independent_block_validation_across_cpp_cases() {
    use k256::ecdsa::{signature::hazmat::PrehashSigner, Signature, SigningKey};
    use mmx_consensus::{header, Value as N};
    use std::time::{Duration, Instant};
    let mut scalar = [0; 32];
    scalar[31] = 1;
    let key = SigningKey::from_slice(&scalar).unwrap();
    let mut checked = 0;
    for line in include_str!("fixtures/blocks.jsonl").lines() {
        let trace: Value = serde_json::from_str(line).unwrap();
        let name = trace["name"].as_str().unwrap();
        if !trace["accepted"].as_bool().unwrap() && !name.starts_with("input_reservation_1100000_")
        {
            continue;
        }
        let mut candidate = record(&trace["block"]);
        let ctx = context(&trace);
        let mut db = StorageDb::memory();
        setup(&mut db, &trace);
        let pending: Vec<Record> = candidate
            .field("tx_list")
            .unwrap()
            .seq()
            .unwrap()
            .iter()
            .map(|v| v.record().unwrap().clone())
            .collect();
        let selected = block::assemble::transactions(
            &db,
            &ctx.params,
            ctx.previous.height + 1,
            &pending,
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(
            !db.database().has_pending(),
            "{name}: assembly changed ledger"
        );
        if name.starts_with("input_reservation_1100000_") {
            assert_eq!(
                selected.len(),
                1,
                "must reject a conflicting aggregate spend"
            );
        }
        let mut static_cost = 0;
        let mut total_cost = 0;
        let mut fees = 0;
        let mut hashes = vec![];
        for tx in &selected {
            static_cost += tx.field("static_cost").unwrap().uint().unwrap();
            let result = tx
                .field("exec_result")
                .unwrap()
                .optional()
                .unwrap()
                .unwrap()
                .record()
                .unwrap();
            total_cost += result.field("total_cost").unwrap().uint().unwrap();
            fees += result.field("total_fee").unwrap().uint().unwrap();
            hashes.push(
                tx.field("content_hash")
                    .unwrap()
                    .bytes()
                    .unwrap()
                    .try_into()
                    .unwrap(),
            );
        }
        let reward = if candidate
            .field("reward_addr")
            .unwrap()
            .optional()
            .unwrap()
            .is_some()
        {
            block::economics::block_reward(
                &ctx.params,
                &ctx.previous,
                ctx.previous.height + 1,
                !candidate.field("proof").unwrap().seq().unwrap().is_empty(),
                fees,
            )
            .unwrap()
        } else {
            0
        };
        for (field, n) in [
            ("tx_count", selected.len() as u64),
            ("static_cost", static_cost),
            ("total_cost", total_cost),
            ("tx_fees", fees),
            ("reward_amount", reward),
        ] {
            candidate.fields.insert(field.into(), N::Uint(n));
        }
        candidate.fields.insert(
            "tx_list".into(),
            N::Sequence(selected.into_iter().map(N::Record).collect()),
        );
        candidate.fields.insert(
            "tx_hash".into(),
            N::Bytes(block::transaction_root(&hashes).to_vec()),
        );
        let hash = header::calc_hash(&candidate, &ctx.params).unwrap();
        let sig: Signature = key.sign_prehash(&hash).unwrap();
        candidate
            .fields
            .insert("hash".into(), N::Bytes(hash.to_vec()));
        candidate.fields.insert(
            "farmer_sig".into(),
            N::Optional(Some(Box::new(N::Bytes(sig.to_bytes().to_vec())))),
        );
        candidate.fields.insert(
            "content_hash".into(),
            N::Bytes(header::calc_content_hash(&candidate).unwrap().to_vec()),
        );
        block::prepare(&mut db, &ctx, &candidate)
            .unwrap_or_else(|e| panic!("{name}: produced invalid candidate: {e}"));
        assert!(!db.database().has_pending());
        checked += 1;
    }
    assert!(checked >= 40);
}

#[test]
#[ignore = "bounded CPU-only throughput comparison; run with --release --ignored --nocapture"]
fn benchmark_parallel_contract_blocks_against_original_serial_execution() {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    for name in [
        "parallel_groups_8_v1",
        "parallel_groups_2_v1",
        "parallel_groups_1_v1",
        "empty_7_v1",
    ] {
        let trace = seed(name);
        let ctx = context(&trace);
        let candidate = record(&trace["block"]);
        let mut db = StorageDb::memory();
        setup(&mut db, &trace);
        let run = |db: &mut StorageDb| {
            let start = std::time::Instant::now();
            for _ in 0..100 {
                drop(block::prepare(db, &ctx, &candidate).unwrap());
            }
            start.elapsed().as_secs_f64() * 10_000.0
        };
        drop(block::prepare(&mut db, &ctx, &candidate).unwrap());
        let mut serial = Vec::new();
        let mut parallel = Vec::new();
        for round in 0..5 {
            if round % 2 == 0 {
                serial.push(pool.install(|| run(&mut db)));
                parallel.push(run(&mut db));
            } else {
                parallel.push(run(&mut db));
                serial.push(pool.install(|| run(&mut db)));
            }
        }
        serial.sort_by(f64::total_cmp);
        parallel.sort_by(f64::total_cmp);
        eprintln!(
            "BENCH {} threads={} serial_us={:.2} parallel_us={:.2} speedup={:.3}",
            name,
            rayon::current_num_threads(),
            serial[2],
            parallel[2],
            serial[2] / parallel[2]
        );
        assert!(!db.database().has_pending());
    }
}
