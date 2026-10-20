//! Native ARM SHA2 two-lane recursion, using fixed one-block padding.
use super::{IV, K};
use std::arch::aarch64::*;
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
