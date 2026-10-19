//! Small hardware check/benchmark. Uses no database, plot files or wallet keys.
//! cargo run --release -p mmx-consensus --example opencl_check -- [GPU_INDEX]
use mmx_consensus::pos;
use mmx_opencl::OpenCl;
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::time::Instant;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let index = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "0".into())
        .parse()?;
    for info in mmx_opencl::devices(None, true)? {
        println!("GPU {}: {} ({})", info.index, info.name, info.platform);
    }
    let mut gpu = OpenCl::new(index, None, true)?;
    println!(
        "CPU comparison uses {} Rayon threads",
        rayon::current_num_threads()
    );
    let inputs: Vec<[u8; 32]> = (0..256)
        .map(|n| std::array::from_fn(|i| (n + i) as u8))
        .collect();
    gpu.repeated_sha256(&inputs, 1)?;
    let begin = Instant::now();
    let actual = gpu.repeated_sha256(&inputs, 10000)?;
    let gpu_time = begin.elapsed();
    let begin = Instant::now();
    let expected: Vec<[u8; 32]> = inputs
        .par_iter()
        .map(|value| {
            let mut h = *value;
            for _ in 0..10000 {
                h = Sha256::digest(h).into();
            }
            h
        })
        .collect();
    assert_eq!(actual, expected);
    println!(
        "VDF 256 x 10000: GPU {:?}, CPU {:?}, outputs match",
        gpu_time,
        begin.elapsed()
    );
    let keys: Vec<[u8; 64]> = (0..16385u64)
        .map(|n| {
            let mut key = [0; 64];
            key[..8].copy_from_slice(&n.to_le_bytes());
            key
        })
        .collect();
    gpu.memory_hashes(&keys[..1])?;
    let begin = Instant::now();
    let actual = gpu.memory_hashes(&keys)?;
    let gpu_time = begin.elapsed();
    let begin = Instant::now();
    let expected: Vec<_> = keys.par_iter().map(pos::memory_hash).collect();
    assert_eq!(actual, expected);
    println!(
        "F1 {} memory hashes: GPU {:?}, CPU {:?}, outputs match",
        keys.len(),
        gpu_time,
        begin.elapsed()
    );
    Ok(())
}
