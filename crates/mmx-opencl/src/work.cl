// MMX consensus work. All arithmetic is unsigned wrapping arithmetic. The memory
// hash follows src/pos/cuda_recompute.cu and the portable Rust implementation.
__kernel void repeated_sha256(__global uint *hash, uint iterations, uint count) {
    size_t id = get_global_id(0);
    if (id >= count) return;
    uint msg[16] = {0};
    for (uint i = 0; i < 8; ++i) msg[i] = bswap_32(hash[id * 8 + i]);
    msg[8] = 0x80000000U;
    msg[15] = 256;
    for (uint j = 0; j < iterations; ++j) {
        uint state[8];
        for (uint i = 0; i < 8; ++i) state[i] = h_init[i];
        sha256(msg, state);
        for (uint i = 0; i < 8; ++i) msg[i] = state[i];
    }
    for (uint i = 0; i < 8; ++i) hash[id * 8 + i] = bswap_32(msg[i]);
}

__kernel void generate_memory(__global const uint *keys, __global uint *memory, uint count) {
    size_t id = get_global_id(0);
    if (id >= count) return;
    uint state[32];
    // First 16 SHA256 round constants are also the native memory-hash IV.
    #pragma unroll
    for (uint i = 0; i < 16; ++i) {
        state[i] = keys[id * 16 + i];
        state[16 + i] = k[i];
    }
    uint b = 0, c = 0;
    for (uint row = 0; row < 32; ++row) {
        for (uint round = 0; round < 4; ++round) {
            #pragma unroll
            for (uint i = 0; i < 16; ++i) {
                uint a = state[i], d = state[16 + i];
                a += b; d = rotate(d ^ a, 16U);
                c += d; b = rotate(b ^ c, 12U);
                a += b; d = rotate(d ^ a, 8U);
                c += d; b = rotate(b ^ c, 7U);
                state[i] = a; state[16 + i] = d;
            }
        }
        #pragma unroll
        for (uint i = 0; i < 8; ++i) {
            ((__global uint4 *)memory)[((size_t)row * count + id) * 8 + i] =
                (uint4)(state[i*4], state[i*4+1], state[i*4+2], state[i*4+3]);
        }
    }
}

// One workgroup per key, one lane per word. Barriers are workgroup-wide and
// unconditional; correctness does not depend on a vendor's warp/subgroup size.
__attribute__((reqd_work_group_size(32, 1, 1)))
__kernel void memory_hash(__global const uint *memory, __global uint *output, uint count) {
    size_t id = get_group_id(0);
    uint lane = get_local_id(0);
    __local uint scratch[1024];
    __local uint sums[32];
    for (uint row = 0; row < 32; ++row) scratch[row * 32 + lane] = memory[((size_t)row * count + id) * 32 + lane];
    barrier(CLK_LOCAL_MEM_FENCE);
    uint state = scratch[992 + lane];
    for (uint iter = 0; iter < 256; ++iter) {
        sums[lane] = rotate(state, lane);
        barrier(CLK_LOCAL_MEM_FENCE);
        for (uint step = 16; step > 0; step >>= 1) {
            if (lane < step) sums[lane] += sums[lane + step];
            barrier(CLK_LOCAL_MEM_FENCE);
        }
        uint sum = sums[0];
        uint dir = sum + (sum << 11) + (sum << 22);
        uint offset = dir >> 27;
        uint bits = (dir >> 22) % 32;
        state += rotate(scratch[offset * 32 + (iter + lane) % 32], bits) ^ sum;
        barrier(CLK_LOCAL_MEM_FENCE);
        scratch[offset * 32 + lane] ^= state;
        barrier(CLK_LOCAL_MEM_FENCE);
    }
    output[id * 32 + lane] = state;
}
