//! Native sha256_ni_rec.cpp's two interleaved states and eight-lane AVX2 SHA.
//! Registers retain digest words across recursion; byte conversion happens once.
use super::{IV, K};
#[cfg(target_arch = "x86")]
use std::arch::x86::*;
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

#[target_feature(enable = "sha,ssse3,sse4.1")]
pub(super) unsafe fn sha_ni(points: &mut [[u8; 32]; 2], iterations: u32) {
    let swap = _mm_set_epi64x(0x0c0d0e0f08090a0b, 0x0405060700010203);
    let init0 = _mm_set_epi64x(0x6a09e667bb67ae85, 0x510e527f9b05688c);
    let init1 = _mm_set_epi64x(0x3c6ef372a54ff53a, 0x1f83d9ab5be0cd19);
    let pad0 = _mm_set_epi64x(0, 0x0000000080000000);
    let pad1 = _mm_set_epi64x(0x0000010000000000, 0);
    let mut hash0 = [_mm_setzero_si128(); 2];
    let mut hash1 = hash0;
    for lane in 0..2 {
        hash0[lane] = _mm_shuffle_epi8(_mm_loadu_si128(points[lane].as_ptr().cast()), swap);
        hash1[lane] = _mm_shuffle_epi8(_mm_loadu_si128(points[lane].as_ptr().add(16).cast()), swap);
    }
    for _ in 0..iterations {
        let mut state0 = [init0; 2];
        let mut state1 = [init1; 2];
        let mut words = [
            [hash0[0], hash1[0], pad0, pad1],
            [hash0[1], hash1[1], pad0, pad1],
        ];
        // Native keeps the fixed 64-round schedule unrolled. A runtime
        // group loop spills state and loses much of the dual-lane throughput.
        macro_rules! rounds {
            ($group:literal) => {{
                const GROUP: usize = $group;
                const SLOT: usize = GROUP % 4;
                let slot = SLOT;
                if GROUP >= 4 {
                    for lane in &mut words {
                        let next = _mm_sha256msg1_epu32(lane[slot], lane[(slot + 1) % 4]);
                        let next = _mm_add_epi32(
                            next,
                            _mm_alignr_epi8::<4>(lane[(slot + 3) % 4], lane[(slot + 2) % 4]),
                        );
                        lane[slot] = _mm_sha256msg2_epu32(next, lane[(slot + 3) % 4]);
                    }
                }
                let round = _mm_loadu_si128(K.as_ptr().add(GROUP * 4).cast());
                let a = _mm_add_epi32(words[0][slot], round);
                let b = _mm_add_epi32(words[1][slot], round);
                state1[0] = _mm_sha256rnds2_epu32(state1[0], state0[0], a);
                state1[1] = _mm_sha256rnds2_epu32(state1[1], state0[1], b);
                state0[0] =
                    _mm_sha256rnds2_epu32(state0[0], state1[0], _mm_shuffle_epi32::<0x0e>(a));
                state0[1] =
                    _mm_sha256rnds2_epu32(state0[1], state1[1], _mm_shuffle_epi32::<0x0e>(b));
            }};
        }
        rounds!(0);
        rounds!(1);
        rounds!(2);
        rounds!(3);
        rounds!(4);
        rounds!(5);
        rounds!(6);
        rounds!(7);
        rounds!(8);
        rounds!(9);
        rounds!(10);
        rounds!(11);
        rounds!(12);
        rounds!(13);
        rounds!(14);
        rounds!(15);

        for lane in 0..2 {
            let a = _mm_shuffle_epi32::<0x1b>(_mm_add_epi32(state0[lane], init0));
            let b = _mm_shuffle_epi32::<0xb1>(_mm_add_epi32(state1[lane], init1));
            hash0[lane] = _mm_blend_epi16::<0xf0>(a, b);
            hash1[lane] = _mm_alignr_epi8::<8>(b, a);
        }
    }
    for lane in 0..2 {
        _mm_storeu_si128(
            points[lane].as_mut_ptr().cast(),
            _mm_shuffle_epi8(hash0[lane], swap),
        );
        _mm_storeu_si128(
            points[lane].as_mut_ptr().add(16).cast(),
            _mm_shuffle_epi8(hash1[lane], swap),
        );
    }
}

macro_rules! rotr {
    ($value:expr,$bits:expr) => {
        _mm256_or_si256(
            _mm256_srli_epi32::<$bits>($value),
            _mm256_slli_epi32::<{ 32 - $bits }>($value),
        )
    };
}
#[target_feature(enable = "avx2")]
pub(super) unsafe fn avx2(points: &mut [[u8; 32]; 8], iterations: u32) {
    let initial = IV.map(|word| _mm256_set1_epi32(word as i32));
    let mut hashes = [_mm256_setzero_si256(); 8];
    for (word, hash) in hashes.iter_mut().enumerate() {
        let lane: [i32; 8] = std::array::from_fn(|lane| {
            u32::from_be_bytes(points[lane][4 * word..4 * word + 4].try_into().unwrap()) as i32
        });
        *hash = _mm256_loadu_si256(lane.as_ptr().cast());
    }
    for _ in 0..iterations {
        let mut words = [_mm256_setzero_si256(); 16];
        words[..8].copy_from_slice(&hashes);
        words[8] = _mm256_set1_epi32(0x80000000u32 as i32);
        words[15] = _mm256_set1_epi32(256);
        let mut state = initial;
        for (round, &constant) in K.iter().enumerate() {
            let slot = round % 16;
            if round >= 16 {
                let a = words[(slot + 1) % 16];
                let b = words[(slot + 14) % 16];
                let s0 = _mm256_xor_si256(
                    _mm256_xor_si256(rotr!(a, 7), rotr!(a, 18)),
                    _mm256_srli_epi32::<3>(a),
                );
                let s1 = _mm256_xor_si256(
                    _mm256_xor_si256(rotr!(b, 17), rotr!(b, 19)),
                    _mm256_srli_epi32::<10>(b),
                );
                words[slot] = _mm256_add_epi32(
                    _mm256_add_epi32(words[slot], s0),
                    _mm256_add_epi32(words[(slot + 9) % 16], s1),
                );
            }
            let [a, b, c, d, e, f, g, h] = state;
            let s1 = _mm256_xor_si256(_mm256_xor_si256(rotr!(e, 6), rotr!(e, 11)), rotr!(e, 25));
            let choose = _mm256_xor_si256(_mm256_and_si256(e, f), _mm256_andnot_si256(e, g));
            let t1 = _mm256_add_epi32(
                _mm256_add_epi32(_mm256_add_epi32(h, s1), choose),
                _mm256_add_epi32(_mm256_set1_epi32(constant as i32), words[slot]),
            );
            let s0 = _mm256_xor_si256(_mm256_xor_si256(rotr!(a, 2), rotr!(a, 13)), rotr!(a, 22));
            let majority = _mm256_xor_si256(
                _mm256_xor_si256(_mm256_and_si256(a, b), _mm256_and_si256(a, c)),
                _mm256_and_si256(b, c),
            );
            state = [
                _mm256_add_epi32(t1, _mm256_add_epi32(s0, majority)),
                a,
                b,
                c,
                _mm256_add_epi32(d, t1),
                e,
                f,
                g,
            ];
        }
        for word in 0..8 {
            hashes[word] = _mm256_add_epi32(initial[word], state[word]);
        }
    }
    for (word, hash) in hashes.into_iter().enumerate() {
        let mut lanes = [0u32; 8];
        _mm256_storeu_si256(lanes.as_mut_ptr().cast(), hash);
        for lane in 0..8 {
            points[lane][4 * word..4 * word + 4].copy_from_slice(&lanes[lane].to_be_bytes());
        }
    }
}
