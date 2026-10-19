# Porting requirements

When porting or changing a subsystem, read its implementation in
`../mmx-node` first. Preserve its important performance choices as well as its
behavior: concurrency, scheduling, batching, deduplication, memory bounds,
positional reads, cheap rejection before expensive work, and immediate delivery
of completed work. Record deliberate differences and missing optimizations in
`docs/performance-parity.md` instead of assuming a working fixture proves parity.

Avoid migration regressions across the node, database, wallet, farmer,
harvester and utilities. Consensus must match C++; the physical database format
may differ. Add meaningful regression tests and compare representative workloads
before claiming performance parity. In particular, benchmark Rust harvesting
with C++ harvesting disabled so C++ cannot warm its plot reads.

Keep disk I/O modest. Do not scan or rewrite live ledger/plot storage for tests,
add broad state caching, or disturb unrelated processes. Build locally with two
jobs, incremental compilation and debug symbols disabled, and low CPU/I/O
priority. Validate complete process identities before stopping owned services.
Commit and push completed source checkpoints.
