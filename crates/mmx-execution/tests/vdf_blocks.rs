use mmx_consensus::{vdf, Hash, Record};
use mmx_execution::block::{self, BlockContext};
use mmx_types::{mmx, VnxType};
use mmx_vm::StorageDb;
use std::collections::BTreeMap;
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
fn verified_vdf_linkage_space_work_and_body_prepare_atomically() {
    let mut count = 0;
    let mut accepted = 0;
    let mut snapshots = BTreeMap::<String, Value>::new();
    for line in include_str!("../../mmx-consensus/tests/fixtures/vdf.jsonl").lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        let snapshot = if let Some(seed) = row["seed"].as_str() {
            snapshots[seed].clone()
        } else {
            snapshots.insert(row["name"].as_str().unwrap().into(), row.clone());
            row.clone()
        };
        if row["kind"] != "pipeline" {
            continue;
        }
        let name = row["name"].as_str().unwrap();
        let current = Record::decode(&bytes(&row["block"])).unwrap();
        let ctx = BlockContext {
            params: mmx::ChainParams::decode(&bytes(&snapshot["params"])).unwrap(),
            previous: mmx::BlockHeader::decode(&bytes(&row["parent"])).unwrap(),
            expected_vdf_payout: None,
        };
        let source = snapshot["headers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                let h = mmx::BlockHeader::decode(&bytes(v)).unwrap();
                (h.hash.0, h)
            })
            .collect::<BTreeMap<Hash, _>>();
        let points = row["proofs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                vdf::verify(&Record::decode(&bytes(v)).unwrap(), &ctx.params, &source).unwrap()
            })
            .collect::<Vec<_>>();
        let mut db = StorageDb::memory();
        let prepared = block::prepare_with_vdf(&mut db, &ctx, &current, &source, &points);
        if row["result"] == "OK" {
            let prepared = prepared.unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(prepared.receipt().results.is_empty(), "{name}");
            assert!(prepared.batch().writes.is_empty(), "{name}");
            drop(prepared);
            assert!(
                !db.database().has_pending(),
                "{name}: dropping published writes"
            );

            // A proof-valid block still requires the exact ledger parent and
            // independently admitted work; caller-supplied iteration counts
            // cannot bypass either prerequisite in this API.
            let mut wrong = ctx.clone();
            wrong.previous.height += 1;
            assert!(block::prepare_with_vdf(&mut db, &wrong, &current, &source, &points).is_err());
            assert!(block::prepare_with_vdf(&mut db, &ctx, &current, &source, &[]).is_err());
            block::prepare_with_vdf(&mut db, &ctx, &current, &source, &points)
                .unwrap()
                .commit()
                .unwrap();
            accepted += 1;
        } else {
            assert!(prepared.is_err(), "{name}: native {}", row["result"]);
        }
        assert!(!db.database().has_pending(), "{name}: published writes");
        count += 1;
    }
    assert_eq!((count, accepted), (63, 18));
}
