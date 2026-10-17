use mmx_consensus::{response, Record};
use vnx::serde_json::{self, Value};
fn raw(v: &Value) -> Vec<u8> {
    vnx::util::from_hex_string(v.as_str().unwrap()).unwrap()
}
#[test]
fn native_farmer_response_hashes_local_fields_static_validity_and_signature_errors() {
    let mut count = 0;
    for line in include_str!("fixtures/responses.jsonl").lines() {
        let r: Value = serde_json::from_str(line).unwrap();
        let record = Record::decode(&raw(&r["wire"])).unwrap();
        assert_eq!(
            record.calc_hash(false, 0).unwrap().as_slice(),
            raw(&r["hash"]),
            "{}",
            r["name"]
        );
        assert_eq!(
            response::content_hash(&record).unwrap().as_slice(),
            raw(&r["content"]),
            "{}",
            r["name"]
        );
        let valid = response::is_valid(&record)
            .map(|v| if v { "1" } else { "0" }.to_owned())
            .unwrap_or_else(|e| e.to_string());
        let auth = response::authenticate(&record)
            .map(|_| "1".to_owned())
            .unwrap_or_else(|e| e.to_string());
        assert_eq!(valid, r["valid"].as_str().unwrap(), "{}", r["name"]);
        assert_eq!(auth, r["auth"].as_str().unwrap(), "{}", r["name"]);
        assert_eq!(Record::decode(&record.encode().unwrap()).unwrap(), record);
        count += 1;
    }
    assert_eq!(count, 80);
}
