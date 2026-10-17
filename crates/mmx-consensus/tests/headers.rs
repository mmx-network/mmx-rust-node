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
