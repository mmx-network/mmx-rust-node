//! Native ARM SHA2 two-lane recursion, using fixed one-block padding.
use super::{IV, K};
use std::arch::aarch64::*;
/// Native latency-oriented one-chain schedule, distinct from dual-lane verify.
#[target_feature(enable = "sha2")]
pub(super) unsafe fn single(point: &mut [u8; 32], iterations: u64) {
    let init0 = vld1q_u32(IV.as_ptr());
    let init1 = vld1q_u32(IV.as_ptr().add(4));
    let pad0 = vld1q_u32([0x80000000, 0, 0, 0].as_ptr());
    let pad1 = vld1q_u32([0, 0, 0, 256].as_ptr());
    let mut hash0 = vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(point.as_ptr())));
    let mut hash1 = vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(point.as_ptr().add(16))));
    for _ in 0..iterations {
        let mut a = init0;
        let mut b = init1;
        macro_rules! state {
            ($words:expr, $round:literal) => {{
                let msg = vaddq_u32($words, vld1q_u32(K.as_ptr().add($round)));
                let saved = a;
                a = vsha256hq_u32(a, b, msg);
                b = vsha256h2q_u32(b, saved, msg);
            }};
        }
        state!(hash0, 0);
        let mut m0 = vsha256su0q_u32(hash0, hash1);
        state!(hash1, 4);
        m0 = vsha256su1q_u32(m0, pad0, pad1);
        let mut m1 = vsha256su0q_u32(hash1, pad0);
        state!(pad0, 8);
        m1 = vsha256su1q_u32(m1, pad1, m0);
        let mut m2 = pad0;
        state!(pad1, 12);
        m2 = vsha256su1q_u32(m2, m0, m1);
        let mut m3 = vsha256su0q_u32(pad1, m0);
        macro_rules! rounds {
            ($w0:ident,$w1:ident,$w2:ident,$w3:ident,$round:literal) => {{
                state!($w0, $round);
                $w3 = vsha256su1q_u32($w3, $w1, $w2);
                $w0 = vsha256su0q_u32($w0, $w1);
            }};
        }
        rounds!(m0, m1, m2, m3, 16);
        rounds!(m1, m2, m3, m0, 20);
        rounds!(m2, m3, m0, m1, 24);
        rounds!(m3, m0, m1, m2, 28);
        rounds!(m0, m1, m2, m3, 32);
        rounds!(m1, m2, m3, m0, 36);
        rounds!(m2, m3, m0, m1, 40);
        rounds!(m3, m0, m1, m2, 44);
        state!(m0, 48);
        m3 = vsha256su1q_u32(m3, m1, m2);
        state!(m1, 52);
        state!(m2, 56);
        state!(m3, 60);
        hash0 = vaddq_u32(a, init0);
        hash1 = vaddq_u32(b, init1);
    }
    vst1q_u8(point.as_mut_ptr(), vrev32q_u8(vreinterpretq_u8_u32(hash0)));
    vst1q_u8(
        point.as_mut_ptr().add(16),
        vrev32q_u8(vreinterpretq_u8_u32(hash1)),
    );
}
#[target_feature(enable = "sha2")]
pub(super) unsafe fn sha2(points: &mut [[u8; 32]; 2], iterations: u32) {
    let init0 = vld1q_u32(IV.as_ptr());
    let init1 = vld1q_u32(IV.as_ptr().add(4));
    let pad0 = vld1q_u32([0x80000000, 0, 0, 0].as_ptr());
    let pad1 = vld1q_u32([0, 0, 0, 256].as_ptr());
    let mut hash0 = [vdupq_n_u32(0); 2];
    let mut hash1 = hash0;
    for lane in 0..2 {
        hash0[lane] = vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(points[lane].as_ptr())));
        hash1[lane] = vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(points[lane].as_ptr().add(16))));
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
                        let next = vsha256su0q_u32(lane[slot], lane[(slot + 1) % 4]);
                        lane[slot] =
                            vsha256su1q_u32(next, lane[(slot + 2) % 4], lane[(slot + 3) % 4]);
                    }
                }
                let constant = vld1q_u32(K.as_ptr().add(GROUP * 4));
                let a = vaddq_u32(words[0][slot], constant);
                let b = vaddq_u32(words[1][slot], constant);
                let saved = state0;
                state0[0] = vsha256hq_u32(state0[0], state1[0], a);
                state0[1] = vsha256hq_u32(state0[1], state1[1], b);
                state1[0] = vsha256h2q_u32(state1[0], saved[0], a);
                state1[1] = vsha256h2q_u32(state1[1], saved[1], b);
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
            hash0[lane] = vaddq_u32(state0[lane], init0);
            hash1[lane] = vaddq_u32(state1[lane], init1);
        }
    }
    for lane in 0..2 {
        vst1q_u8(
            points[lane].as_mut_ptr(),
            vrev32q_u8(vreinterpretq_u8_u32(hash0[lane])),
        );
        vst1q_u8(
            points[lane].as_mut_ptr().add(16),
            vrev32q_u8(vreinterpretq_u8_u32(hash1[lane])),
        );
    }
}
