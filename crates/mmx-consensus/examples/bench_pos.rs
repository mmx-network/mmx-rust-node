//! Repeatable CPU proof benchmark using inputs checked against the native port.
use mmx_consensus::{pos, Record};
use std::{hint::black_box, time::Instant};
use vnx::serde_json::{self, Value};

fn main() {
    let mut args: Vec<_> = std::env::args().skip(1).collect();
    let workers = args.first().is_some_and(|arg| arg == "--header-workers");
    if workers {
        args.remove(0);
    }
    let repetitions: u32 = args
        .first()
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
    if let Some(path) = args.get(1) {
        let bytes = std::fs::read(path).unwrap();
        let block = if let Some(payload) = bytes.strip_prefix(b"MMXR\x01\0\0\0") {
            Record::decode_compact(payload).unwrap()
        } else {
            Record::decode(&bytes).unwrap()
        };
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
    if workers {
        // Keep serial match tables on the eight caller threads and send only
        // F1 work into the global Rayon pool, exactly as header verification.
        let compute = || {
            assert_eq!(
                pos::compute(black_box(&xs), black_box(&id), ksize),
                expected
            );
        };
        let start = Instant::now();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let compute = &compute;
                scope.spawn(move || {
                    for _ in 0..repetitions {
                        compute();
                    }
                });
            }
        });
        let seconds = start.elapsed().as_secs_f64();
        println!(
            "{} global F1 threads, eight header workers: {:.3} ms/proof, {:.1} proofs/s ({} proofs)",
            std::env::var("RAYON_NUM_THREADS").unwrap_or_else(|_| "default".into()),
            seconds * 1000.0 / (8.0 * f64::from(repetitions)),
            8.0 * f64::from(repetitions) / seconds,
            8 * repetitions,
        );
        return;
    }
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
        drop(pool);
        // A fresh process gives each trial an independent global Rayon pool.
        // Installing the entire compute() in a local pool would also move the
        // serial match tables and would not model the production scheduler.
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--header-workers")
            .args(&args)
            .env("RAYON_NUM_THREADS", threads.to_string())
            .status()
            .unwrap();
        assert!(status.success());
    }
}
