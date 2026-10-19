//! Real ICD tests, required in CI with POCL and optionally run on a physical GPU:
//! MMX_OPENCL_TESTS=1 cargo test -p mmx-consensus --test opencl
use mmx_consensus::{accelerator::Accelerator, pos, vdf, Hash, Record};
use mmx_opencl::OpenCl;
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use vnx::serde_json::{self, Value};

fn unhex(value: &Value) -> Vec<u8> {
    let text = value.as_str().unwrap();
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn real_opencl_matches_native_vdf_and_compressed_proof_work() {
    if std::env::var("MMX_OPENCL_TESTS").as_deref() != Ok("1") {
        eprintln!("Set MMX_OPENCL_TESTS=1 to require an OpenCL ICD");
        return;
    }
    let gpu_only = std::env::var("MMX_OPENCL_ALL_DEVICES").as_deref() != Ok("1");
    let mut engine = OpenCl::new(0, None, gpu_only).expect("OpenCL ICD required for this test");
    eprintln!("Testing OpenCL: {:?}", engine.device);
    // Non-aligned counts, repeated launches, zero work and buffer growth/reuse.
    let inputs: Vec<Hash> = (0..33)
        .map(|n| std::array::from_fn(|i| (n * 7 + i) as u8))
        .collect();
    for count in [0, 1, 33, 2] {
        for iterations in [0, 1, 2499, 2500, 2501, 5003] {
            let expected: Vec<Hash> = inputs[..count]
                .par_iter()
                .map(|input| {
                    let mut hash = *input;
                    for _ in 0..iterations {
                        hash = Sha256::digest(hash).into();
                    }
                    hash
                })
                .collect();
            assert_eq!(
                engine
                    .repeated_sha256(&inputs[..count], iterations)
                    .unwrap(),
                expected
            );
        }
    }
    let many = vec![inputs[1]; 65537];
    let expected: Hash = Sha256::digest(inputs[1]).into();
    assert!(engine
        .repeated_sha256(&many, 1)
        .unwrap()
        .iter()
        .all(|hash| *hash == expected));
    let mut expected = inputs[1];
    for _ in 0..40001 {
        expected = Sha256::digest(expected).into();
    }
    assert_eq!(
        engine.repeated_sha256(&[inputs[1]], 40001).unwrap(),
        [expected]
    );
    let rows: Vec<Value> = include_str!("fixtures/work.jsonl")
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    let leaves: Vec<_> = rows.iter().filter(|r| r["kind"] == "leaf").collect();
    let keys: Vec<[u8; 64]> = leaves
        .iter()
        .map(|r| unhex(&r["key"]).try_into().unwrap())
        .collect();
    let hashes = engine.memory_hashes(&keys).unwrap();
    for (row, hash) in leaves.iter().zip(hashes) {
        assert_eq!(hash.as_slice(), unhex(&row["memory_hash"]));
    }
    // Cross the device batch boundary; every entry has a distinct key. The CPU
    // oracle is independent of local size, barriers, layout and allocation reuse.
    let keys: Vec<[u8; 64]> = (0..4097u64)
        .map(|n| {
            let mut key = std::array::from_fn(|i| i as u8);
            key[..8].copy_from_slice(&n.to_le_bytes());
            key
        })
        .collect();
    let expected: Vec<_> = keys.par_iter().map(pos::memory_hash).collect();
    assert_eq!(engine.memory_hashes(&keys).unwrap(), expected);
    assert_eq!(engine.memory_hashes(&keys[..1]).unwrap(), expected[..1]);
    assert!(engine.memory_hashes(&[]).unwrap().is_empty());
    drop(engine);

    let accelerator = Accelerator::new(0, None, gpu_only).unwrap();
    let mut checked = 0;
    for line in include_str!("fixtures/vdf.jsonl").lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        if row["kind"] != "proof" {
            continue;
        }
        let r = Record::decode(&unhex(&row["wire"])).unwrap();
        let cpu = vdf::verify_cpu(&r);
        let gpu = vdf::verify_accelerated(&r, &accelerator);
        assert_eq!(gpu.is_ok(), cpu.is_ok(), "{}", row["name"]);
        if cpu
            .as_ref()
            .is_err_and(|e| e.to_string().starts_with("invalid output on segment"))
        {
            assert_eq!(gpu.unwrap_err().to_string(), cpu.unwrap_err().to_string());
        }
        assert!(
            accelerator.enabled(),
            "driver errors must not silently turn a GPU test into a CPU test"
        );
        checked += 1;
    }
    assert_eq!(checked, 205);
    for row in rows.iter().filter(|r| r["kind"] == "compute") {
        let id: Hash = unhex(&row["id"]).try_into().unwrap();
        let k = row["ksize"].as_u64().unwrap() as u8;
        let mut xs: Vec<u32> = row["xs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u32)
            .collect();
        let cpu = pos::compute(&xs, &id, k);
        assert_eq!(
            pos::compute_with_accelerator(&xs, &id, k, Some(&accelerator)),
            cpu
        );
        xs.reverse();
        xs.extend_from_within(..);
        assert_eq!(
            pos::compute_with_accelerator(&xs, &id, k, Some(&accelerator)),
            cpu
        );
        assert!(accelerator.enabled());
    }
    assert_eq!(
        pos::compute_with_accelerator(&[], &[0; 32], 8, Some(&accelerator)),
        pos::compute(&[], &[0; 32], 8)
    );
    assert_eq!(
        pos::compute_with_accelerator(&[1], &[0; 32], 33, Some(&accelerator)),
        Err(pos::Error::Invalid("invalid ksize"))
    );
}
