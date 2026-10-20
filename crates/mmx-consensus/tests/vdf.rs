use mmx_consensus::{vdf, Hash, Record};
use mmx_types::{mmx, VnxType};
use std::collections::BTreeMap;
use vnx::serde_json::{self, Value};

#[test]
#[ignore = "bounded CPU-only complete VDF verification comparison"]
fn benchmark_batched_vdf_verification_against_original_parallel_segments() {
    use rayon::prelude::*;
    use sha2::{Digest, Sha256};
    for count in [16usize, 40, 80, 128] {
        let mut proof = mmx::ProofOfTime {
            input: [1; 32].into(),
            prev: [2; 32].into(),
            reward_addr: [3; 32].into(),
            segment_size: 250_000,
            num_iters: count as u64 * 250_000,
            vdf_height: 1,
            ..Default::default()
        };
        let mixed = Sha256::digest([[1; 32].as_slice(), [2; 32].as_slice()].concat());
        let initial: Hash = Sha256::digest([mixed.as_slice(), [3; 32].as_slice()].concat()).into();
        let mut point = initial;
        let mut inputs = Vec::new();
        for _ in 0..count {
            inputs.push(point);
            for _ in 0..proof.segment_size {
                point = Sha256::digest(point).into();
            }
            proof.segments.push(point.into());
        }
        let record = Record::from_generated(&proof).unwrap();
        let baseline = || {
            let start = std::time::Instant::now();
            assert!(inputs
                .par_iter()
                .zip(&proof.segments)
                .all(|(input, expected)| {
                    let mut point = *input;
                    for _ in 0..proof.segment_size {
                        point = Sha256::digest(point).into();
                    }
                    point == expected.0
                }));
            start.elapsed().as_secs_f64() * 1000.0
        };
        let batched = || {
            let start = std::time::Instant::now();
            vdf::verify_cpu(&record).unwrap();
            start.elapsed().as_secs_f64() * 1000.0
        };
        let mut old = Vec::new();
        let mut new = Vec::new();
        for round in 0..5 {
            if round % 2 == 0 {
                old.push(baseline());
                new.push(batched());
            } else {
                new.push(batched());
                old.push(baseline());
            }
        }
        old.sort_by(f64::total_cmp);
        new.sort_by(f64::total_cmp);
        eprintln!("BENCH vdf segments={} size=250000 threads={} old_ms={:.3} batched_ms={:.3} speedup={:.3}",count,rayon::current_num_threads(),old[2],new[2],old[2]/new[2]);
    }
}

#[test]
fn cpu_batches_preserve_segment_inputs_odd_tails_and_first_failure_across_chunks() {
    use sha2::{Digest, Sha256};
    for count in [1usize, 2, 15, 16, 17, 31, 32, 33] {
        for size in [1u32, 3, 17] {
            let mut proof = mmx::ProofOfTime {
                input: [1; 32].into(),
                prev: [2; 32].into(),
                reward_addr: [3; 32].into(),
                segment_size: size,
                num_iters: count as u64 * u64::from(size),
                vdf_height: 1,
                ..Default::default()
            };
            let mixed = Sha256::digest([[1; 32].as_slice(), [2; 32].as_slice()].concat());
            let mut point: Hash =
                Sha256::digest([mixed.as_slice(), [3; 32].as_slice()].concat()).into();
            for _ in 0..count {
                for _ in 0..size {
                    point = Sha256::digest(point).into();
                }
                proof.segments.push(point.into());
            }
            assert!(
                vdf::verify_cpu(&Record::from_generated(&proof).unwrap()).is_ok(),
                "{count} / {size}"
            );
            for bad in [0, count / 2, count - 1] {
                let mut bad_proof = proof.clone();
                bad_proof.segments[bad].0[0] ^= 1;
                assert_eq!(
                    vdf::verify_cpu(&Record::from_generated(&bad_proof).unwrap())
                        .unwrap_err()
                        .to_string(),
                    format!("invalid output on segment {bad}")
                );
            }
            if count > 16 {
                proof.segments[1].0[0] ^= 1;
                proof.segments[16].0[0] ^= 1;
                assert_eq!(
                    vdf::verify_cpu(&Record::from_generated(&proof).unwrap())
                        .unwrap_err()
                        .to_string(),
                    "invalid output on segment 1"
                );
            }
        }
    }
}

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
