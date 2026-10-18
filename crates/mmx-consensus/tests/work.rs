use mmx_consensus::{
    pos, proof,
    transitions::{self, history, ProofContext},
    Hash, Record,
};
use mmx_types::{mmx, VnxType};
use std::collections::BTreeMap;
use vnx::serde_json::{self, Value};
fn unhex(s: &str) -> Vec<u8> {
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}
fn hex(v: &[u8]) -> String {
    v.iter().map(|b| format!("{b:02x}")).collect()
}
fn hash(v: &Value) -> Hash {
    unhex(v.as_str().unwrap()).try_into().unwrap()
}
fn record(v: &Value) -> Record {
    Record::decode(&unhex(v.as_str().unwrap())).unwrap()
}
fn params(v: &Value) -> mmx::ChainParams {
    mmx::ChainParams::decode(&unhex(v.as_str().unwrap())).unwrap()
}
fn header(v: &Value) -> mmx::BlockHeader {
    mmx::BlockHeader::decode(&unhex(v.as_str().unwrap())).unwrap()
}
fn xs(v: &Value) -> Vec<u32> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_u64().unwrap() as u32)
        .collect()
}
fn result<T: AsRef<[u8]>, E: std::fmt::Display>(v: Result<T, E>) -> String {
    v.map_or_else(|e| format!("ERR:{e}"), |v| hex(v.as_ref()))
}
fn accepted<T, E: std::fmt::Display>(actual: Result<T, E>, expected: &Value, label: &str) {
    let native = expected.as_str().unwrap();
    match actual {
        Ok(_) => assert_eq!(native, "OK", "{label}: native rejects: {native}"),
        Err(e) => {
            assert_ne!(native, "OK", "{label}: Rust rejects: {e}");
            let e = e.to_string();
            let reason = e.strip_prefix("invalid consensus input: ").unwrap_or(&e);
            assert!(
                reason == native || native.starts_with(&format!("{reason}:")),
                "{label}: native {native:?}; Rust {e:?}"
            );
        }
    }
}
#[test]
fn native_memory_hash_space_work_header_transitions_and_ancestor_lookup() {
    let mut counts = BTreeMap::new();
    for (line, text) in include_str!("fixtures/work.jsonl").lines().enumerate() {
        let row: Value = serde_json::from_str(text).unwrap();
        let kind = row["kind"].as_str().unwrap();
        *counts.entry(kind.to_owned()).or_insert(0) += 1;
        let label = format!("{kind} case {line}");
        match kind {
            "leaf" => {
                let k = row["ksize"].as_u64().unwrap() as u8;
                let x = row["x"].as_u64().unwrap() as u32;
                let (y, meta) = pos::compute_f1(x, &hash(&row["id"]), k).unwrap();
                assert_eq!(y, row["y"].as_u64().unwrap() as u32, "{label}: Y");
                assert_eq!(meta.to_vec(), xs(&row["meta"]), "{label}: metadata");
                assert_eq!(
                    hex(&pos::memory_hash(
                        &unhex(row["key"].as_str().unwrap()).try_into().unwrap()
                    )),
                    row["memory_hash"].as_str().unwrap(),
                    "{label}: memory hash"
                );
            }
            "compute" => {
                let actual = pos::compute(
                    &xs(&row["xs"]),
                    &hash(&row["id"]),
                    row["ksize"].as_u64().unwrap() as u8,
                );
                if row["result"] == "OK" {
                    let entries = actual.unwrap_or_else(|e| panic!("{label}: {e}"));
                    let expected = row["entries"].as_array().unwrap();
                    assert_eq!(entries.len(), expected.len(), "{label}: count");
                    for (a, b) in entries.iter().zip(expected) {
                        assert_eq!(a.y, b["y"].as_u64().unwrap() as u32, "{label}: Y");
                        assert_eq!(
                            hex(&a.meta),
                            b["meta"].as_str().unwrap(),
                            "{label}: metadata"
                        );
                        assert_eq!(a.proof, xs(&b["proof"]), "{label}: canonical proof");
                    }
                } else {
                    accepted(actual, &row["result"], &label);
                }
            }
            "verify" => {
                let actual = pos::verify(
                    &xs(&row["xs"]),
                    &hash(&row["challenge"]),
                    &hash(&row["id"]),
                    row["plot_filter"].as_u64().unwrap() as u32,
                    row["post_filter"].as_u64().unwrap() as u32,
                    row["ksize"].as_u64().unwrap() as u8,
                    row["hardfork"].as_bool().unwrap(),
                );
                assert_eq!(result(actual), row["result"].as_str().unwrap(), "{label}");
            }
            "proof" => accepted(
                proof::verify(
                    &record(&row["wire"]),
                    &params(&row["params"]),
                    &hash(&row["challenge"]),
                    row["difficulty"].as_u64().unwrap(),
                    row["height"].as_u64().unwrap() as u32,
                ),
                &row["result"],
                &label,
            ),
            "transition" => {
                let context = ProofContext {
                    parent: header(&row["parent"]),
                    iteration_counts: row["iterations"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_u64().unwrap())
                        .collect(),
                    challenge: hash(&row["challenge"]),
                    space_diff: row["difficulty"].as_u64().unwrap(),
                };
                accepted(
                    transitions::verify_header(
                        &record(&row["block"]),
                        &params(&row["params"]),
                        &context,
                    ),
                    &row["result"],
                    &label,
                );
            }
            "history" => {
                let source: BTreeMap<_, _> = row["headers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| {
                        let h = header(v);
                        (h.hash.0, h)
                    })
                    .collect();
                let p = params(&row["params"]);
                let current = header(&row["current"]);
                let offset = row["offset"].as_u64().unwrap() as u32;
                let actual = history::challenge(&source, &p, &current, offset);
                if let Ok((_, diff)) = &actual {
                    assert_eq!(
                        *diff,
                        row["difficulty"].as_u64().unwrap(),
                        "{label}: challenge difficulty"
                    );
                }
                assert_eq!(
                    result(actual.map(|v| v.0)),
                    row["challenge"].as_str().unwrap(),
                    "{label}: challenge"
                );
                let actual = history::infusion(&source, &p, &current, offset);
                if let Ok(v) = &actual {
                    assert_eq!(
                        v.num_iters,
                        row["iterations"].as_u64().unwrap(),
                        "{label}: infusion iterations"
                    );
                }
                assert_eq!(
                    result(actual.map(|v| v.hash)),
                    row["infusion"].as_str().unwrap(),
                    "{label}: infusion"
                );
            }
            "context" => {
                let source: BTreeMap<_, _> = row["headers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| {
                        let h = header(v);
                        (h.hash.0, h)
                    })
                    .collect();
                let context = ProofContext::resolve(
                    &record(&row["current"]),
                    &params(&row["params"]),
                    &source,
                );
                if let Ok(ctx) = &context {
                    assert_eq!(
                        ctx.challenge,
                        hash(&row["challenge"]),
                        "{label}: resolved challenge"
                    );
                    assert_eq!(
                        ctx.space_diff,
                        row["difficulty"].as_u64().unwrap(),
                        "{label}: resolved difficulty"
                    );
                    assert_eq!(
                        ctx.iteration_counts,
                        row["iterations"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|v| v.as_u64().unwrap())
                            .collect::<Vec<_>>(),
                        "{label}: resolved iterations"
                    );
                }
                accepted(context, &row["result"], &label);
            }
            "space_diff" => assert_eq!(
                transitions::new_space_diff(&params(&row["params"]), &header(&row["parent"]))
                    .unwrap(),
                row["result"].as_u64().unwrap(),
                "{label}"
            ),
            "challenge" => {
                let p = params(&row["params"]);
                let parent = header(&row["parent"]);
                let chain = transitions::proof_chain(&p, &parent, &hash(&row["proof_hash"]));
                assert_eq!(chain, hash(&row["chain"]), "{label}: proof chain");
                let (challenge, fork) = transitions::next_challenge(
                    &p,
                    &parent,
                    row["height"].as_u64().unwrap() as u32,
                    row["count"].as_u64().unwrap() as u32,
                    &hash(&row["proof_hash"]),
                    &chain,
                )
                .unwrap();
                assert_eq!(challenge, hash(&row["challenge"]), "{label}: challenge");
                assert_eq!(fork, row["fork"].as_bool().unwrap(), "{label}: fork");
            }
            "threshold" => assert_eq!(
                proof::check_threshold(
                    &params(&row["params"]),
                    row["ksize"].as_u64().unwrap() as u8,
                    &hash(&row["quality"]),
                    row["difficulty"].as_u64().unwrap(),
                    row["hardfork"].as_bool().unwrap()
                )
                .unwrap(),
                row["result"].as_bool().unwrap(),
                "{label}"
            ),
            "filters" => {
                let challenge = hash(&row["challenge"]);
                let id = hash(&row["id"]);
                let meta = unhex(row["meta"].as_str().unwrap()).try_into().unwrap();
                let bits = row["bits"].as_u64().unwrap() as u32;
                assert_eq!(
                    proof::plot_challenge(&challenge, &id),
                    hash(&row["plot_challenge"]),
                    "{label}: plot challenge"
                );
                assert_eq!(
                    pos::calc_quality(&challenge, &meta),
                    hash(&row["quality"]),
                    "{label}: quality"
                );
                assert_eq!(
                    proof::check_plot_filter(&challenge, &id, bits),
                    row["plot_filter"].as_bool().unwrap(),
                    "{label}: plot filter"
                );
                assert_eq!(
                    pos::check_post_filter(&challenge, &meta, bits),
                    row["post_filter"].as_bool().unwrap(),
                    "{label}: post filter"
                );
            }
            "pipeline" => {} // Checked through the execution crate's prepared-block API.
            _ => panic!("unknown kind {kind}"),
        }
    }
    assert_eq!(
        counts,
        BTreeMap::from([
            ("leaf".into(), 32),
            ("compute".into(), 4),
            ("verify".into(), 16),
            ("proof".into(), 26),
            ("transition".into(), 63),
            ("history".into(), 108),
            ("context".into(), 18),
            ("space_diff".into(), 210),
            ("challenge".into(), 48),
            ("threshold".into(), 60),
            ("filters".into(), 18),
            ("pipeline".into(), 9)
        ])
    );
}

#[test]
fn proof_recomputation_is_identical_with_one_or_four_workers() {
    let one = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let four = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .unwrap();
    let mut count = 0;
    for text in include_str!("fixtures/work.jsonl").lines() {
        let r: Value = serde_json::from_str(text).unwrap();
        if r["kind"] != "compute" {
            continue;
        }
        let input = xs(&r["xs"]);
        let id = hash(&r["id"]);
        let k = r["ksize"].as_u64().unwrap() as u8;
        let a = one.install(|| pos::compute(&input, &id, k));
        let b = four.install(|| pos::compute(&input, &id, k));
        assert_eq!(a, b);
        assert_eq!(a.is_ok(), r["result"] == "OK");
        count += 1;
    }
    assert_eq!(count, 4);
}
