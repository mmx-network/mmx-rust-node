use mmx_execution::chain::sync::{Block, Policy};
use std::collections::BTreeSet;
use vnx::serde_json::{self, Value};

#[test]
fn native_root_proof_density_sync_position_pending_jobs_and_wrapping_arithmetic() {
    let mut count = 0;
    for text in include_str!("fixtures/sync.jsonl").lines() {
        let r: Value = serde_json::from_str(text).unwrap();
        let n = |key: &str| r[key].as_u64().unwrap() as u32;
        let block = |v: &Value| Block {
            height: v["height"].as_u64().unwrap() as u32,
            vdf_height: v["vdf_height"].as_u64().unwrap() as u32,
            proofs: v["proofs"].as_u64().unwrap(),
        };
        let line: Vec<_> = r["line"].as_array().unwrap().iter().map(block).collect();
        let pending = r["pending"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u32)
            .collect();
        let p = Policy {
            commit_threshold: n("threshold"),
            commit_delay: n("delay"),
            max_future_sync: n("future"),
            avg_proof_count: n("average"),
        };
        let result = p
            .advance(
                block(&r["root"]),
                &line,
                r["synced"].as_bool().unwrap(),
                n("sync_pos"),
                &pending,
            )
            .unwrap();
        assert_eq!(result.fork_weight, n("weight"), "{}", r["name"]);
        let expected: Vec<_> = r["heights"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u32)
            .collect();
        assert_eq!(result.heights, expected, "{}", r["name"]);
        count += 1;
    }
    assert!(count >= 200);
}
#[test]
fn root_policy_rejects_undefined_native_division_and_noncontiguous_history() {
    let p = Policy {
        commit_threshold: 80,
        commit_delay: 24,
        max_future_sync: 100,
        avg_proof_count: 3,
    };
    let root = Block {
        height: 0,
        vdf_height: 0,
        proofs: 0,
    };
    assert!(p
        .advance(
            root,
            &[Block { height: 1, ..root }],
            true,
            0,
            &BTreeSet::new()
        )
        .is_err());
    assert!(p
        .advance(
            root,
            &[Block {
                height: 2,
                vdf_height: 1,
                proofs: 3
            }],
            true,
            0,
            &BTreeSet::new()
        )
        .is_err());
}

#[test]
fn native_sync_future_timestamp_filter_has_a_strict_signed_boundary() {
    use mmx_execution::chain::sync::allows_timestamp;
    for stamp in [i64::MIN, -1, 0, 9000, 9999] {
        assert!(allows_timestamp(stamp, 9000, 10, 100));
    }
    for stamp in [10000, 10001, i64::MAX] {
        assert!(!allows_timestamp(stamp, 9000, 10, 100));
    }
    assert!(allows_timestamp(499, -500, 10, 100));
    assert!(!allows_timestamp(500, -500, 10, 100));
}
