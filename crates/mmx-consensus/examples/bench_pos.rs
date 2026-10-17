//! Repeatable CPU proof benchmark using inputs checked against the native port.
use mmx_consensus::{pos, Record};
use std::{hint::black_box, time::Instant};
use vnx::serde_json::{self, Value};

fn main() {
    let repetitions: u32 = std::env::args()
        .nth(1)
        .map(|v| v.parse().expect("repetition count"))
        .unwrap_or(100);
    assert!(repetitions > 0);
    let row: Value = include_str!("../tests/fixtures/work.jsonl")
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|r| r["kind"] == "compute" && r["ksize"] == 29)
        .unwrap();
    let mut xs: Vec<u32> = row["xs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_u64().unwrap() as u32)
        .collect();
    let mut id: [u8; 32] = vnx::util::from_hex_string(row["id"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let mut ksize = 29;
    if let Some(path) = std::env::args().nth(2) {
        let block = Record::decode(&std::fs::read(path).unwrap()).unwrap();
        println!(
            "Block: {} wire bytes, {} compact payload bytes",
            block.encode().unwrap().len(),
            block.encode_compact().unwrap().len()
        );
        let proof = block.field("proof").unwrap().seq().unwrap()[0]
            .record()
            .unwrap();
        xs = proof
            .field("proof_xs")
            .unwrap()
            .seq()
            .unwrap()
            .iter()
            .map(|v| v.uint().unwrap() as u32)
            .collect();
        id = proof
            .field("plot_id")
            .unwrap()
            .bytes()
            .unwrap()
            .try_into()
            .unwrap();
        ksize = proof.field("ksize").unwrap().uint().unwrap() as u8;
        println!("Archived k{ksize} proof");
        assert!(pos::compute(&xs, &id, ksize).is_ok());
    } else {
        println!("Native F1/table-2 fixture (no final proof)");
    }
    let expected = pos::compute(&xs, &id, ksize);
    for threads in [1, 2, 4, 8, 20] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        for per_call in [true, false] {
            let compute = || {
                assert_eq!(
                    pos::compute(black_box(&xs), black_box(&id), ksize),
                    expected
                );
            };
            let start = Instant::now();
            if per_call {
                for _ in 0..repetitions {
                    pool.install(compute);
                }
            } else {
                pool.install(|| {
                    for _ in 0..repetitions {
                        compute();
                    }
                });
            }
            println!(
                "{threads:2} threads, {}: {:.3} ms/proof ({repetitions} repetitions)",
                if per_call {
                    "enter per proof"
                } else {
                    "one worker batch"
                },
                start.elapsed().as_secs_f64() * 1000.0 / f64::from(repetitions)
            );
        }
    }
}
