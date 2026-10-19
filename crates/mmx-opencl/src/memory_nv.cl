// NVIDIA OpenCL fast path, following MMX CUDA plotter Node_phase1.cu's
// calc_mem_hash: four independent 32-lane hashes per 128-thread workgroup.
// Only compiled for NVIDIA GPUs with verified 32-lane warps and SM >= 7.
#ifdef MMX_NVIDIA_WARP
inline void mmx_syncwarp() {
    asm volatile("bar.warp.sync 0xffffffff;" ::: "memory");
}
inline uint mmx_shuffle_xor(uint value, uint delta) {
    uint result;
    asm volatile("shfl.sync.bfly.b32 %0, %1, %2, 31, -1;"
                 : "=r"(result) : "r"(value), "r"(delta));
    return result;
}
__attribute__((reqd_work_group_size(128, 1, 1)))
__kernel void memory_hash_nv(__global const uint *memory, __global uint *output, uint count) {
    uint lane=get_local_id(0)&31, warp=get_local_id(0)>>5;
    size_t id=get_group_id(0)*4+warp;
    // Every warp is either entirely active or entirely inactive, including the
    // final partial workgroup. No workgroup-wide barrier appears below.
    if (id>=count) return;
    volatile __local uint scratch[4*1024];
    uint base=warp*1024;
    #pragma unroll
    for (uint row=0; row<32; ++row)
        scratch[base+row*32+lane]=memory[((size_t)row*count+id)*32+lane];
    mmx_syncwarp();
    uint state=scratch[base+992+lane];
    for (uint iter=0; iter<256; ++iter) {
        uint sum=rotate(state,lane);
        #pragma unroll
        for (uint step=16; step>0; step>>=1) sum+=mmx_shuffle_xor(sum,step);
        uint dir=sum+(sum<<11)+(sum<<22);
        uint offset=dir>>27, bits=(dir>>22)&31;
        state+=rotate(scratch[base+offset*32+((iter+lane)&31)],bits)^sum;
        mmx_syncwarp();
        // Each lane owns a distinct word. Warp barriers order all reads before
        // these writes and all writes before the following iteration's reads.
        scratch[base+offset*32+lane]^=state;
        mmx_syncwarp();
    }
    output[id*32+lane]=state;
}
#endif
