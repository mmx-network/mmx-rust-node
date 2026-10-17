use mmx_consensus::{transitions::ProofContext, Record};
use mmx_execution::block::{self, BlockContext};
use mmx_types::{mmx, VnxType};
use mmx_vm::StorageDb;
use vnx::serde_json::{self, Value};
fn bytes(v: &Value) -> Vec<u8> {
    v.as_str()
        .unwrap()
        .as_bytes()
        .chunks_exact(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}
#[test]
fn native_space_work_transitions_and_authenticated_body_prepare_atomically() {
    let mut count = 0;
    let mut accepted = 0;
    for line in include_str!("../../mmx-consensus/tests/fixtures/work.jsonl").lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        if row["kind"] != "pipeline" {
            continue;
        }
        let current = Record::decode(&bytes(&row["block"])).unwrap();
        let ctx = BlockContext {
            params: mmx::ChainParams::decode(&bytes(&row["params"])).unwrap(),
            previous: mmx::BlockHeader::decode(&bytes(&row["parent"])).unwrap(),
            expected_vdf_payout: None,
        };
        let proof_context = ProofContext {
            parent: ctx.previous.clone(),
            iteration_counts: row["iterations"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap())
                .collect(),
            challenge: bytes(&row["challenge"]).try_into().unwrap(),
            space_diff: row["difficulty"].as_u64().unwrap(),
        };
        let mut db = StorageDb::memory();
        let prepared = block::prepare_with_proofs(&mut db, &ctx, &current, &proof_context);
        if row["result"] == "OK" {
            let prepared = prepared.unwrap_or_else(|e| panic!("pipeline case {count}: {e}"));
            assert!(prepared.receipt().results.is_empty());
            assert!(prepared.batch().writes.is_empty());
            drop(prepared);
            accepted += 1;
            let mut wrong = proof_context.clone();
            wrong.parent.height += 1;
            assert!(block::prepare_with_proofs(&mut db, &ctx, &current, &wrong).is_err());
        } else {
            assert!(
                prepared.is_err(),
                "pipeline case {count}: native {}",
                row["result"]
            );
        }
        assert!(
            !db.database().has_pending(),
            "pipeline case {count}: published writes"
        );
        count += 1;
    }
    assert_eq!((count, accepted), (9, 3));
}
