use mmx_execution::chain::choice::{self, Anchor, Candidate, State};
use vnx::serde_json::{self, Value};
fn hash(value: &Value) -> [u8; 32] {
    vnx::util::from_hex_string(value.as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap()
}
fn weight(value: &Value) -> u128 {
    value.as_str().unwrap().parse().unwrap()
}
#[test]
fn native_fork_choice_roots_weight_votes_wrap_height_hash_and_sync_guards() {
    let mut count = 0;
    for line in include_str!("fixtures/forks.jsonl").lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        let candidates: Vec<_> = row["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| Candidate {
                hash: hash(&c["hash"]),
                prev: hash(&c["prev"]),
                height: c["height"].as_u64().unwrap() as u32,
                total_weight: weight(&c["total_weight"]),
                votes: c["votes"].as_u64().unwrap() as u32,
                proof_verified: c["proof_verified"].as_bool().unwrap(),
                invalid: c["invalid"].as_bool().unwrap(),
                anchor: (!c["anchor"].is_null()).then(|| Anchor {
                    hash: hash(&c["anchor"]["hash"]),
                    total_weight: weight(&c["anchor"]["total_weight"]),
                }),
            })
            .collect();
        let selection = choice::select(
            row["root_height"].as_u64().unwrap() as u32,
            hash(&row["root"]),
            row["sync_pending"].as_u64().map(|n| n as u32),
            &candidates,
        );
        assert_eq!(
            selection.best,
            (!row["best"].as_str().unwrap().is_empty()).then(|| hash(&row["best"])),
            "{}",
            row["name"]
        );
        for s in row["states"].as_array().unwrap() {
            let expected = State {
                anchor: s.get("anchor").map(|h| Anchor {
                    hash: hash(h),
                    total_weight: weight(&s["anchor_weight"]),
                }),
                total_votes: s["total_votes"].as_u64().unwrap() as u32,
                all_proofs_verified: s["all_proofs_verified"].as_bool().unwrap(),
                invalid: s["invalid"].as_bool().unwrap(),
            };
            assert_eq!(
                selection.states.get(&hash(&s["hash"])),
                Some(&expected),
                "{}",
                row["name"]
            );
        }
        count += 1;
    }
    assert_eq!(count, 430);
}
