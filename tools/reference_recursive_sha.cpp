// CPU-only reference for the native SHA-NI kernel; no ledger or plots are read.
// g++ -O3 -std=c++17 -msha -mssse3 -msse4.1 -I../mmx-node/include \
//   tools/reference_recursive_sha.cpp ../mmx-node/src/sha256_ni_rec.cpp -o /tmp/mmx-sha-reference
#include <sha256_ni.h>
#include <array>
#include <algorithm>
#include <chrono>
#include <cstdio>
int main() {
 if(!__builtin_cpu_supports("sha")) return 2;
 std::array<double,5> timings;
 std::array<std::array<unsigned char,32>,16> points;
 for(auto& timing:timings) {
  for(auto& point:points) point.fill(0x42);
  auto start=std::chrono::steady_clock::now();
  for(int i=0;i<16;i+=2) recursive_sha256_ni_x2(points[i].data(),250000);
  timing=std::chrono::duration<double,std::milli>(std::chrono::steady_clock::now()-start).count();
 }
 std::sort(timings.begin(),timings.end());
 std::printf("BENCH native SHA-NI lanes=16 iterations=250000 median_ms=%.3f digest=",timings[2]);
 for(auto value:points[0]) std::printf("%02x",value);
 std::puts("");
}
