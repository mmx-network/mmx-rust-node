//! Repeatable CPU proof benchmark using inputs checked against the native port.
use mmx_consensus::pos;
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
    let xs: Vec<u32> = row["xs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_u64().unwrap() as u32)
        .collect();
    let id: [u8; 32] = vnx::util::from_hex_string(row["id"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let expected = pos::compute(&xs, &id, 29);
    for threads in [1, 2, 4, 8, 20] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        let start = Instant::now();
        pool.install(|| {
            for _ in 0..repetitions {
                assert_eq!(pos::compute(black_box(&xs), black_box(&id), 29), expected);
            }
        });
        println!(
            "{threads:2} threads: {:.3} ms/proof ({repetitions} repetitions)",
            start.elapsed().as_secs_f64() * 1000.0 / f64::from(repetitions)
        );
    }
}
