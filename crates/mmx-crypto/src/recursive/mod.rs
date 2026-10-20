//! Fixed 32-byte recursive SHA256, following native VDF CPU lane batching.
//! Only this work kernel uses SIMD; ordinary hashes/signatures are unchanged.
use sha2::{Digest, Sha256};
#[cfg(target_arch = "aarch64")]
mod arm;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod x86;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    ShaNi,
    ArmSha2,
    Avx2,
    Scalar,
}
pub fn backend() -> Backend {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if std::is_x86_feature_detected!("sha")
            && std::is_x86_feature_detected!("ssse3")
            && std::is_x86_feature_detected!("sse4.1")
        {
            return Backend::ShaNi;
        }
        if std::is_x86_feature_detected!("avx2") {
            return Backend::Avx2;
        }
    }
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("sha2") {
        return Backend::ArmSha2;
    }
    Backend::Scalar
}
/// Hash each fixed-width lane `iterations` times. Runtime feature checks guard
/// every target-feature call; no input length or alignment is assumed by SIMD.
pub fn hash_many(points: &mut [[u8; 32]], iterations: u32) {
    dispatch(points, iterations, backend());
}
fn dispatch(points: &mut [[u8; 32]], iterations: u32, selected: Backend) {
    if iterations == 0 || points.is_empty() {
        return;
    }
    match selected {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        Backend::ShaNi => {
            let mut pairs = points.chunks_exact_mut(2);
            for pair in pairs.by_ref() {
                // SAFETY: the selected backend checked SHA/SSSE3/SSE4.1, and
                // chunks_exact_mut supplies exactly two complete 32-byte lanes.
                unsafe {
                    x86::sha_ni(pair.try_into().unwrap(), iterations);
                }
            }
            scalar(pairs.into_remainder(), iterations);
        }
        #[cfg(target_arch = "aarch64")]
        Backend::ArmSha2 => {
            let mut pairs = points.chunks_exact_mut(2);
            for pair in pairs.by_ref() {
                // SAFETY: runtime SHA2 support and exactly two complete lanes.
                unsafe {
                    arm::sha2(pair.try_into().unwrap(), iterations);
                }
            }
            scalar(pairs.into_remainder(), iterations);
        }
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        Backend::Avx2 => {
            for chunk in points.chunks_mut(8) {
                let mut lanes = [[0; 32]; 8];
                let count = chunk.len();
                lanes[..count].copy_from_slice(chunk);
                // SAFETY: runtime AVX2/OS support is checked, and the local
                // array supplies all eight lanes, including initialized tails.
                unsafe {
                    x86::avx2(&mut lanes, iterations);
                }
                chunk.copy_from_slice(&lanes[..count]);
            }
        }
        _ => scalar(points, iterations),
    }
}
fn scalar(points: &mut [[u8; 32]], iterations: u32) {
    for point in points {
        for _ in 0..iterations {
            *point = Sha256::digest(*point).into();
        }
    }
}
#[cfg(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64"))]
const IV: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];
#[cfg(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64"))]
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

#[cfg(test)]
mod tests {
    use super::*;
    fn check(selected: Backend) {
        for count in [0, 1, 2, 3, 7, 8, 15, 16, 17] {
            for iterations in [0, 1, 2, 17, 64] {
                let points: Vec<_> = (0..count)
                    .map(|i| {
                        std::array::from_fn(|j| {
                            (i as u8)
                                .wrapping_mul(71)
                                .wrapping_add((j as u8).wrapping_mul(37))
                        })
                    })
                    .collect();
                let mut expected = points.clone();
                scalar(&mut expected, iterations);
                let mut actual = points;
                dispatch(&mut actual, iterations, selected);
                assert_eq!(
                    actual, expected,
                    "{selected:?}: {count} lanes / {iterations} iterations"
                );
            }
        }
        let mut expected = [[0xff; 32], [0; 32]];
        let mut actual = expected;
        scalar(&mut expected, 1009);
        dispatch(&mut actual, 1009, selected);
        assert_eq!(actual, expected);
    }
    #[test]
    fn automatic_recursive_hashes_match_scalar_endian_padding_and_tail_cases() {
        eprintln!("recursive SHA backend: {:?}", backend());
        check(backend());
    }
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[test]
    fn avx2_recursive_hashes_match_even_when_sha_ni_is_preferred() {
        if std::is_x86_feature_detected!("avx2") {
            check(Backend::Avx2);
        }
    }
    #[test]
    #[ignore = "CPU-only recursive SHA throughput comparison"]
    fn benchmark_recursive_sha256_backends() {
        let modes = vec![Backend::Scalar, backend()];
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        let mut modes = modes;
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        if std::is_x86_feature_detected!("avx2") {
            modes.push(Backend::Avx2);
        }
        for mode in modes {
            let mut samples = Vec::new();
            for _ in 0..5 {
                let mut points = [[0x42; 32]; 16];
                let start = std::time::Instant::now();
                dispatch(&mut points, 250_000, mode);
                let expected = vnx::util::from_hex_string(
                    "0f455a761c2e98ee34f9e8b9a16cabfa791fcc6f4c1aea0f7fe3ef4c0dfb030f",
                )
                .unwrap();
                assert!(
                    points.iter().all(|point| point.as_slice() == expected),
                    "native recursive SHA output"
                );
                std::hint::black_box(points);
                samples.push(start.elapsed());
            }
            samples.sort();
            eprintln!(
                "BENCH recursive {mode:?} lanes=16 iterations=250000 median_ms={:.3}",
                samples[2].as_secs_f64() * 1000.0
            );
        }
    }
}
