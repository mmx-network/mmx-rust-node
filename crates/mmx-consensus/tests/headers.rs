use mmx_consensus::{header, proof, Record};
use mmx_types::{mmx, VnxType};
use std::collections::BTreeMap;
use vnx::serde_json::{self, Value};

fn unhex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks_exact(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn record(value: &Value) -> Record {
    Record::decode(&unhex(value.as_str().unwrap())).unwrap()
}
fn hash_result(value: mmx_consensus::Result<impl AsRef<[u8]>>) -> String {
    value.map_or_else(|_| "ERR".into(), |v| hex(v.as_ref()))
}

#[test]
fn checked_header_projection_matches_self_describing_native_conversion() {
    fn compare(record: &Record) {
        let envelope = Record {
            name: "mmx.BlockHeader".into(),
            fields: record
                .fields
                .iter()
                .filter(|(name, _)| name.as_str() != "tx_list")
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect(),
        };
        let expected = envelope
            .encode()
            .and_then(|wire| Ok(mmx::BlockHeader::decode(&wire)?));
        let actual = header::to_typed(record);
        match expected {
            Ok(expected) => assert_eq!(actual.unwrap(), expected),
            Err(_) => assert!(actual.is_err()),
        }
    }
    let mut count = 0;
    for line in include_str!("fixtures/headers.jsonl").lines() {
        let trace: Value = serde_json::from_str(line).unwrap();
        compare(&record(&trace["wire"]));
        count += 1;
    }
    for line in include_str!("fixtures/native.tsv").lines() {
        let fields: Vec<_> = line.split('\t').collect();
        let record = Record::decode(&unhex(fields[2])).unwrap();
        if matches!(record.name.as_str(), "mmx.Block" | "mmx.BlockHeader") {
            compare(&record);
            count += 1;
        }
    }
    assert!(count >= 570);
    let original = Record::from_generated(&mmx::BlockHeader::default()).unwrap();
    // Exercise each field independently so zero/default fixture fields cannot
    // hide an omitted assignment or a copy from a neighboring field.
    for (index, (name, value)) in original.fields.iter().enumerate() {
        use mmx_consensus::Value as V;
        let value = match value {
            V::Uint(_) => V::Uint(1024 + index as u64),
            V::Int(_) => V::Int(-1),
            V::Bool(value) => V::Bool(!value),
            V::Bytes(value) => V::Bytes(vec![index as u8 + 1; value.len()]),
            V::Optional(None) => V::Optional(Some(Box::new(V::Bytes(vec![
                index as u8 + 1;
                if name == "farmer_sig" {
                    64
                } else {
                    32
                }
            ])))),
            V::Sequence(_) if name == "vdf_reward_addr" => {
                V::Sequence(vec![V::Bytes(vec![index as u8 + 1; 32])])
            }
            V::Sequence(_) if name == "proof" => V::Sequence(vec![V::Record(
                Record::from_generated(&mmx::ProofOfSpaceOG::default()).unwrap(),
            )]),
            other => panic!("unexpected header field {name}: {other:?}"),
        };
        let mut changed = original.clone();
        changed.fields.insert(name.clone(), value);
        compare(&changed);
    }
    let mut unusual = original.clone();
    unusual.fields.insert(
        "proof".into(),
        mmx_consensus::Value::Sequence(vec![mmx_consensus::Value::Record(
            Record::from_generated(&mmx::ProofOfTime::default()).unwrap(),
        )]),
    );
    compare(&unusual);
    let mut full = original.clone();
    full.name = "mmx.Block".into();
    full.fields
        .insert("tx_list".into(), mmx_consensus::Value::Opaque(vec![0xff]));
    compare(&full);
    for (name, value) in [
        ("height", mmx_consensus::Value::Uint(u64::MAX)),
        ("reward_vote", mmx_consensus::Value::Int(128)),
        ("reward_vote_sum", mmx_consensus::Value::Int(i64::MIN)),
        (
            "farmer_sig",
            mmx_consensus::Value::Optional(Some(Box::new(mmx_consensus::Value::Bytes(vec![
                0;
                63
            ])))),
        ),
        ("proof_chain", mmx_consensus::Value::Bytes(vec![17; 32])),
        ("nonce", mmx_consensus::Value::Uint(u64::MAX)),
        ("time_stamp", mmx_consensus::Value::Int(i64::MIN)),
    ] {
        let mut changed = original.clone();
        changed.fields.insert(name.into(), value);
        compare(&changed);
    }
    let mut missing = original;
    missing.fields.remove("proof_chain");
    assert!(header::to_typed(&missing).is_err());
}

#[test]
fn checked_header_projection_retains_general_reader_container_limits() {
    use mmx_consensus::Value as V;
    let mut proof = Record::from_generated(&mmx::ProofOfSpaceOG::default()).unwrap();
    proof.fields.insert(
        "proof_xs".into(),
        V::Sequence(vec![
            V::Uint(0);
            vnx::binary::Limits::default().max_container + 1
        ]),
    );
    let mut header = Record::from_generated(&mmx::BlockHeader::default()).unwrap();
    header
        .fields
        .insert("proof".into(), V::Sequence(vec![V::Record(proof)]));
    let wire = header.encode().unwrap();
    assert!(mmx::BlockHeader::decode(&wire).is_err());
    assert!(header::to_typed(&header).is_err());
}

#[test]
fn time_difficulty_walk_shares_headers_and_preserves_genesis_clamping_and_wrapping() {
    use mmx_consensus::transitions::history::{block_iters, HeaderSource};
    use std::sync::Arc;

    struct Shared(BTreeMap<[u8; 32], Arc<mmx::BlockHeader>>);
    impl HeaderSource for Shared {
        fn get_header(
            &self,
            _: &[u8; 32],
        ) -> mmx_consensus::proof::ProofResult<Option<mmx::BlockHeader>> {
            panic!("time-difficulty lookup must not copy whole space-proof payloads")
        }
        fn get_shared_header(
            &self,
            hash: &[u8; 32],
        ) -> mmx_consensus::proof::ProofResult<Option<Arc<mmx::BlockHeader>>> {
            Ok(self.0.get(hash).cloned())
        }
    }
    let mut source = Shared(
        (0..=3u8)
            .map(|height| {
                (
                    [height; 32],
                    Arc::new(mmx::BlockHeader {
                        hash: [height; 32].into(),
                        prev: [height.wrapping_sub(1); 32].into(),
                        height: u32::from(height),
                        time_diff: 36 - 6 * u64::from(height),
                        ..Default::default()
                    }),
                )
            })
            .collect(),
    );
    let tip = source.0[&[3; 32]].clone();
    let mut params = mmx::ChainParams {
        commit_delay: 2,
        time_diff_divider: 3,
        time_diff_constant: u64::MAX,
        ..Default::default()
    };
    assert_eq!(
        block_iters(&source, &params, &tip).unwrap(),
        10u64.wrapping_mul(u64::MAX)
    );
    params.commit_delay = 100;
    assert_eq!(
        block_iters(&source, &params, &tip).unwrap(),
        12u64.wrapping_mul(u64::MAX)
    );
    source.0.remove(&[2; 32]);
    assert!(block_iters(&source, &params, &tip).is_err());
    params.commit_delay = 0;
    assert_eq!(
        block_iters(&source, &params, &tip).unwrap(),
        6u64.wrapping_mul(u64::MAX)
    );
    params.time_diff_divider = 0;
    assert!(block_iters(&source, &params, &tip).is_err());
}

#[test]
fn native_header_serialization_hashes_static_validity_farmer_auth_and_context() {
    let mut count = 0;
    let mut snapshots = BTreeMap::<String, Value>::new();
    for line in include_str!("fixtures/headers.jsonl").lines() {
        let trace: Value = serde_json::from_str(line).unwrap();
        let name = trace["name"].as_str().unwrap();
        let r = record(&trace["wire"]);
        let snapshot = if let Some(seed) = trace["seed"].as_str() {
            snapshots[seed].clone()
        } else {
            snapshots.insert(name.to_owned(), trace.clone());
            trace.clone()
        };
        let p = mmx::ChainParams::decode(&unhex(snapshot["params"].as_str().unwrap())).unwrap();
        let previous =
            mmx::BlockHeader::decode(&unhex(snapshot["previous"].as_str().unwrap())).unwrap();
        let encoded = r.encode().unwrap();
        assert_eq!(
            Record::decode(&encoded).unwrap(),
            r,
            "{name}: wire round trip"
        );
        let valid = header::is_valid(&r, &p)
            .map_or_else(|_| "ERR".into(), |v| if v { "1" } else { "0" }.to_owned());
        assert_eq!(
            valid,
            trace["valid"].as_str().unwrap(),
            "{name}: static validity"
        );
        if trace["hash"] != "SKIP" {
            assert_eq!(
                hash_result(header::calc_hash(&r, &p)),
                trace["hash"].as_str().unwrap(),
                "{name}: hash"
            );
            assert_eq!(
                hash_result(header::hash_serialize(&r, &p)),
                trace["serialized"].as_str().unwrap(),
                "{name}: exact bytes"
            );
        } else {
            assert!(header::calc_hash(&r, &p).is_err(), "{name}: null proof");
        }
        assert_eq!(
            hash_result(header::calc_content_hash(&r)),
            trace["content_hash"].as_str().unwrap(),
            "{name}: content hash"
        );
        if trace["auth"] != "SKIP" {
            let auth = header::authenticate(&r).map_or_else(|e| e.to_string(), |_| "OK".to_owned());
            assert_eq!(
                auth,
                trace["auth"].as_str().unwrap(),
                "{name}: farmer authentication"
            );
        }
        assert_eq!(
            header::validate_context(&r, &previous, &p).is_ok(),
            trace["context"] == "OK",
            "{name}: context; native {}",
            trace["context"]
        );
        count += 1;
    }
    assert_eq!(count, 570);
}

#[test]
fn native_og_nft_proof_record_hashes_coordinate_endianness_and_plot_ids() {
    let mut count = 0;
    for (index, line) in include_str!("fixtures/proof_records.jsonl")
        .lines()
        .enumerate()
    {
        let trace: Value = serde_json::from_str(line).unwrap();
        let r = record(&trace["wire"]);
        assert_eq!(
            Record::decode(&r.encode().unwrap()).unwrap(),
            r,
            "case {index}: wire round trip"
        );
        assert_eq!(
            proof::is_valid(&r).unwrap(),
            trace["valid"].as_bool().unwrap(),
            "case {index}: static validity"
        );
        assert_eq!(
            hash_result(r.calc_hash(false, 0)),
            trace["hash"].as_str().unwrap(),
            "case {index}: proof record hash"
        );
        assert_eq!(
            hash_result(r.calc_hash(true, 1)),
            trace["hash"].as_str().unwrap(),
            "case {index}: hash version is ignored"
        );
        assert_eq!(
            hash_result(proof::calc_proof_hash(&r)),
            trace["proof_hash"].as_str().unwrap(),
            "case {index}: coordinates hash"
        );
        assert_eq!(
            hash_result(proof::calc_plot_id(&r)),
            trace["plot_id"].as_str().unwrap(),
            "case {index}: plot identity"
        );
        assert_eq!(
            proof::validate_plot_id(&r).is_ok(),
            trace["identity"] == "OK",
            "case {index}: identity validation"
        );
        count += 1;
    }
    assert_eq!(count, 100);
}
