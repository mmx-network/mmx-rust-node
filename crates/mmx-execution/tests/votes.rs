use mmx_consensus::{vote, Record};
use mmx_types::{mmx, VnxType};
use std::collections::BTreeMap;
use vnx::serde_json::{self, Value};
fn unhex(value: &Value) -> Vec<u8> {
    vnx::util::from_hex_string(value.as_str().unwrap()).unwrap()
}
fn record(value: &Value) -> Record {
    Record::decode(&unhex(value)).unwrap()
}
#[test]
fn native_vote_hash_domain_content_signatures_and_parse_short_circuit() {
    let mut count = 0;
    for line in include_str!("fixtures/votes.jsonl").lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        if row["kind"] != "crypto" {
            continue;
        }
        let r = record(&row["wire"]);
        let block = r
            .field("hash")
            .unwrap()
            .bytes()
            .unwrap()
            .try_into()
            .unwrap();
        let key = r
            .field("farmer_key")
            .unwrap()
            .bytes()
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(
            vote::signing_hash(&block, &key).as_slice(),
            unhex(&row["message"])
        );
        assert_eq!(
            vote::content_hash(&r).unwrap().as_slice(),
            unhex(&row["content"])
        );
        let valid = vote::is_valid(&r).map_or_else(
            |e| e.to_string(),
            |valid| if valid { "1" } else { "0" }.into(),
        );
        assert_eq!(valid, row["valid"].as_str().unwrap(), "{}", row["name"]);
        assert_eq!(vote::verify(&r).is_ok(), row["valid"] == "1");
        count += 1;
    }
    assert_eq!(count, 110);
}
#[test]
fn native_validator_history_limits_unclamped_delay_distinct_keys_and_first_proof() {
    let mut count = 0;
    let mut snapshots = BTreeMap::<String, Vec<Value>>::new();
    for line in include_str!("fixtures/votes.jsonl").lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        if row["kind"] != "history" {
            continue;
        }
        let wires = if let Some(seed) = row["seed"].as_str() {
            snapshots[seed].clone()
        } else {
            let wires = row["wires"].as_array().unwrap().clone();
            snapshots.insert(row["name"].as_str().unwrap().into(), wires.clone());
            wires
        };
        let headers: BTreeMap<_, _> = wires
            .iter()
            .map(|wire| {
                let h = mmx::BlockHeader::decode(&unhex(wire)).unwrap();
                (h.hash.0, h)
            })
            .collect();
        let id = row["tip"].as_u64().unwrap();
        let mut tip = [0; 32];
        tip[..8].copy_from_slice(&id.to_le_bytes());
        let p = mmx::ChainParams {
            commit_delay: row["delay"].as_u64().unwrap() as u32,
            max_validators: row["max_validators"].as_u64().unwrap() as u32,
            ..Default::default()
        };
        let actual = vote::validators(
            &headers,
            &p,
            &headers[&tip],
            row["max_history"].as_u64().unwrap() as u32,
        )
        .unwrap();
        let expected = row["validators"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| unhex(k).try_into().unwrap())
            .collect();
        assert_eq!(actual, expected, "{}", row["name"]);
        count += 1;
    }
    assert_eq!(count, 101);
}
