use mmx_consensus::{vdf, Hash, Record};
use mmx_types::{mmx, VnxType};
use std::collections::BTreeMap;
use vnx::serde_json::{self, Value};

fn bytes(v: &Value) -> Vec<u8> {
    v.as_str()
        .unwrap()
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}
fn hex(v: &[u8]) -> String {
    v.iter().map(|b| format!("{b:02x}")).collect()
}
fn headers(row: &Value) -> BTreeMap<Hash, mmx::BlockHeader> {
    row["headers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            let h = mmx::BlockHeader::decode(&bytes(v)).unwrap();
            (h.hash.0, h)
        })
        .collect()
}
fn outcome<T>(v: vdf::VdfResult<T>) -> String {
    v.map_or_else(|e| e.to_string(), |_| "OK".into())
}
fn snapshot(row: &Value, snapshots: &mut BTreeMap<String, Value>) -> Value {
    if let Some(seed) = row["seed"].as_str() {
        snapshots[seed].clone()
    } else {
        snapshots.insert(row["name"].as_str().unwrap().into(), row.clone());
        row.clone()
    }
}

#[test]
fn native_vdf_hashes_admission_cpu_work_and_point_metadata() {
    let mut count = 0;
    let mut accepted = 0;
    let mut snapshots = BTreeMap::new();
    for line in include_str!("fixtures/vdf.jsonl").lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        let snapshot = snapshot(&row, &mut snapshots);
        if row["kind"] != "proof" {
            continue;
        }
        let name = row["name"].as_str().unwrap();
        let r = Record::decode(&bytes(&row["wire"])).unwrap();
        let p = mmx::ChainParams::decode(&bytes(&snapshot["params"])).unwrap();
        let source = headers(&snapshot);
        assert_eq!(Record::decode(&r.encode().unwrap()).unwrap(), r, "{name}");
        assert_eq!(hex(&r.calc_hash(false, 0).unwrap()), row["hash"], "{name}");
        assert_eq!(hex(&r.calc_hash(true, 1).unwrap()), row["hash"], "{name}");
        assert_eq!(
            hex(&vdf::hash_serialize(&r).unwrap()),
            row["serialized"],
            "{name}"
        );
        assert_eq!(
            hex(&vdf::calc_content_hash(&r).unwrap()),
            row["content_hash"],
            "{name}"
        );
        assert_eq!(vdf::is_valid(&r).unwrap(), row["valid"], "{name}");
        assert_eq!(hex(&vdf::output(&r).unwrap()), row["output"], "{name}");
        assert_eq!(vdf::end_iters(&r).unwrap(), row["end_iters"], "{name}");
        assert_eq!(outcome(vdf::authenticate(&r)), row["auth"], "{name}: auth");
        assert_eq!(
            outcome(vdf::validate(&r, &p, &source)),
            row["admission"],
            "{name}: admission"
        );
        let cpu = outcome(vdf::verify_cpu(&r));
        assert_eq!(cpu, row["cpu"], "{name}: hardware CPU oracle");
        assert_eq!(cpu, row["fallback"], "{name}: fallback CPU oracle");
        let point = Record::decode(&bytes(&row["point"])).unwrap();
        assert_eq!(
            hex(&point.calc_hash(false, 0).unwrap()),
            row["point_hash"],
            "{name}"
        );
        assert_eq!(
            hex(&vdf::hash_serialize(&point).unwrap()),
            row["point_serialized"],
            "{name}"
        );
        assert!(vdf::is_valid(&point).unwrap(), "{name}");
        let verified = vdf::verify(&r, &p, &source);
        if row["admission"] == "OK" && row["cpu"] == "OK" {
            let verified = verified.unwrap_or_else(|e| panic!("{name}: {e}"));
            let cpu = vdf::CpuVerifiedProof::verify(r.clone()).unwrap();
            let background = cpu.admit(&p, &source).unwrap();
            assert_eq!(background.proof(), verified.proof());
            assert_eq!(background.point(), verified.point());
            let mut different = p.clone();
            different.vdf_segment_size = different.vdf_segment_size.wrapping_add(1);
            assert!(cpu.admit(&different, &source).is_err());
            assert!(cpu
                .admit(&p, &BTreeMap::<Hash, mmx::BlockHeader>::new())
                .is_err());
            assert_eq!(
                hex(&verified.point().content_hash.0),
                row["point_hash"],
                "{name}"
            );
            assert_eq!(
                Record::from_generated(verified.proof()).unwrap(),
                r,
                "{name}"
            );
            accepted += 1;
        } else {
            assert!(verified.is_err(), "{name}: accepted invalid work");
        }
        count += 1;
    }
    assert_eq!((count, accepted), (205, 48));
}

#[test]
fn native_vdf_output_linkage_and_signed_metadata_exemptions() {
    let mut count = 0;
    let mut accepted = 0;
    let mut snapshots = BTreeMap::new();
    for line in include_str!("fixtures/vdf.jsonl").lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        let snapshot = snapshot(&row, &mut snapshots);
        if row["kind"] != "link" {
            continue;
        }
        let name = row["name"].as_str().unwrap();
        let p = mmx::ChainParams::decode(&bytes(&snapshot["params"])).unwrap();
        let source = headers(&snapshot);
        let block = Record::decode(&bytes(&row["block"])).unwrap();
        let points = row["proofs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| vdf::verify(&Record::decode(&bytes(v)).unwrap(), &p, &source).unwrap())
            .collect::<Vec<_>>();
        let chosen = vdf::link_block(&block, &p, &source, &points);
        if row["chosen"].as_array().unwrap().is_empty() {
            assert!(chosen.is_err(), "{name}: accepted unlinked output");
        } else {
            let ids = chosen
                .unwrap_or_else(|e| panic!("{name}: {e}"))
                .iter()
                .map(|v| Value::String(hex(&v.point().content_hash.0)))
                .collect::<Vec<_>>();
            assert_eq!(ids, row["chosen"].as_array().unwrap().clone(), "{name}");
            accepted += 1;
        }
        count += 1;
    }
    assert_eq!((count, accepted), (184, 72));
}
