# Porting requirements

Use C++ as the reference for intent, functionality, performance, reliability and
robustness, not as a requirement for one-to-one code, architecture or config.
When porting or changing a subsystem, read its implementation in `../mmx-node`
first. Preserve the properties behind its important performance choices:
concurrency, scheduling, batching, deduplication, memory bounds, positional
reads, cheap rejection before expensive work, and immediate delivery of
completed work. Rust may use different mechanisms when they preserve these
outcomes. An omitted C++ framework setting is not by itself a functional gap;
lost capabilities, operational control or guarantees are. Keep meaningful
config names and units compatible where practical, and document unsupported
settings and deliberate differences in `docs/performance-parity.md`.

Judge alternatives by observable behavior and representative workloads,
including slow resources, failures, overload, cancellation and recovery. A
working fixture or a simpler implementation alone does not establish equivalent
performance, reliability or robustness. Record evidence and remaining
uncertainty instead of treating an architectural difference as either a defect
or proof of improvement.

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
