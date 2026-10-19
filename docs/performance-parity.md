# C++ performance parity

Performance is part of the migration requirements for every component. Read the
C++ counterpart before implementation and preserve the execution strategy as
well as consensus results. Comparable measurements are required before claiming
performance parity. A source comparison alone does not establish it.

The initial comparison below uses `../mmx-node` at
`5626ac2b7cd26aa9626ea3b2cf864b4cfe6fbcd2` (2026-10-19). Native fixtures retain
their own recorded source revisions. The [full subsystem audit](cpp-port-audit.md)
records the follow-up fixes and prioritized remaining work. In particular,
reference-only compaction does not eliminate the chain layer's separate block
and transaction payload copies; consolidating those writes remains the first
storage optimization.

| Area | C++ reference and important choices | Rust coverage and remaining checks |
| --- | --- | --- |
| Database | `src/DataBase.cpp`: buffered append log, sorted blocks, versioned rollback, flush thresholds | `crates/mmx-db/src/{native,journal,sorted}.rs`: append values once, index references, positional reads, bounded reader handles, batched checkpoints, historical versions. Rust synchronizes each published journal commit; compare durability and write throughput explicitly before changing this policy. |
| Sync | `src/Router.cpp` height jobs and `src/Node.cpp`: random peer subsets, content-hash observations, unique body downloads, overlapping bounded jobs | `crates/mmx-node/src/sync/peers*`: 3–8 distinct authenticated peers, all observed content candidates, content deduplication and timeout reassignment, background peer refill and prefetch. Socket tests cover competing branches, duplicate providers and refill while syncing. Repeat throughput measurements after scheduling/storage changes. |
| Proof/VDF work | `src/Node_verify.cpp`, `src/pos/verify*.cpp`: cheap checks, worker pools, SHA-NI/ARM/AVX batching and accelerated recompute | `crates/mmx-consensus` and `crates/mmx-opencl`: bounded work admission, parallel F1, GPU VDF/F1 kernels and CPU fallback with native witnesses. C++ specialized CPU VDF batching remains a performance comparison gap; the GPU path does not establish CPU fallback parity. |
| Harvesting | `src/Harvester.cpp`, `src/pos/Prover.cpp`: concurrent plot jobs, publish each proof immediately, network challenge deadline, estimated Y park search, post-filter before full proof, retained headers | `crates/mmx-farming` and `crates/mmx-tools`: shared bounded Rayon workers for plot reads and nested F1, streaming proofs, `challenge_delay * block_interval_ms`, density-based Y search, positional reads, per-proof park reuse, retained headers and per-quality error isolation. A stalled-plot regression test verifies that another plot can publish first. The daemon still waits for a lookup batch before scheduling the next challenge; C++ can queue successive challenges while workers are busy. Compare this under slow-drive stress before large-farm migration. |
| Wallet/farmer | `src/Wallet.cpp`, `src/Farmer.cpp`: retained farmer identities, separate signing work, native key derivation and RPC | Rust retains farmer keys, uses a private signer and a restricted remote harvester listener, and tests native signatures and wallet spending through actual daemons. Large-wallet/history latency and signing under sustained harvester traffic still need comparative measurements. Pool partial submission and other unported RPCs remain listed in README. |

The production follow-up compares `Node::update`, `Node::make_block`,
`Node::add_proof`, `Node::find_best_proof`, and `Router::handle(ProofResponse)`.
Verified proof announcements now relay before block creation, with unhashed
harvester/address metadata removed and sender echoes suppressed. A full block
requires ownership of the best verified full proof hash for its challenge.
Planning refreshes queued announcements and fork choice before taking a ledger
snapshot; asynchronous signatures are rechecked before publication. Peak
replacement uses the peak's challenge, voting follows the native peak/orphan
rules, and failed attempts can retry while signed proofs remain suppressed.
The native stronger-proof fixture exercises both pre-build and in-flight vetoes.

For each change, record the C++ paths inspected, relevant fixture/correctness
checks, worker and memory bounds, duplicate work avoided, and representative
latency/throughput/I/O measurements. Test failure and slow-resource cases too:
one slow peer, disk or plot must not hold up unrelated completed work. Preserve
native ordering wherever it affects proof identity, execution or consensus.

For farming measurements, disable the C++ harvester and retain normal filesystem
caches. Do not flush machine caches or scan plot contents. Record plot counts,
compression, worker limits and proof delivery timing alongside lookup duration.

The relay follow-up forwards advancing verified VDFs, fully applied live blocks
and accepted votes. It batches runtime vote processing with authenticated replay
deduplication and a 4,096-pending limit, bounds retained VDF bytes and waiting
time, suppresses VDF sender echoes, and releases receive-idle inbound peers.
Fork replay reuses the already-loaded block bodies; the relay handoff reuses
those records too. RPC point-read products and transaction batch/output sizes
now have admission budgets. The audit distinguishes these completed changes
from native credit/rate policy, streaming range reads and payload consolidation.
