# C++ performance parity

Performance is part of the migration requirements for every component. Read the
C++ counterpart before implementation and preserve the execution strategy as
well as consensus results. Comparable measurements are required before claiming
performance parity. A source comparison alone does not establish it.

The initial comparison below uses `../mmx-node` at
`5626ac2b7cd26aa9626ea3b2cf864b4cfe6fbcd2` (2026-10-19). Native fixtures retain
their own recorded source revisions. The [full subsystem audit](cpp-port-audit.md)
records the follow-up fixes and prioritized remaining work. New block headers
and transactions now use a single archive payload store and selected-chain
locators. Existing native payload records are read in place without migration.

| Area | C++ reference and important choices | Rust coverage and remaining checks |
| --- | --- | --- |
| Database | `src/DataBase.cpp`: buffered append log, sorted blocks, versioned rollback, flush thresholds | `crates/mmx-db/src/{native,journal,sorted}.rs`: append values once, index references, positional reads, bounded reader handles, batched checkpoints, historical versions. Rust synchronizes each published journal commit; compare durability and write throughput explicitly before changing this policy. |
| Sync | `src/Router.cpp` height jobs and `src/Node.cpp`: random peer subsets, content-hash observations, unique body downloads, overlapping bounded jobs | `crates/mmx-node/src/sync/peers*`: 3–8 distinct authenticated peers, all observed content candidates, content deduplication and timeout reassignment, background peer refill and prefetch. Socket tests cover competing branches, duplicate providers and refill while syncing. Repeat throughput measurements after scheduling/storage changes. |
| Proof/VDF work | `src/Node_verify.cpp`, `src/pos/verify*.cpp`: cheap checks, worker pools, SHA-NI/ARM/AVX batching and accelerated recompute | `crates/mmx-consensus` and `crates/mmx-opencl`: bounded work admission, parallel F1, GPU VDF/F1 kernels and CPU fallback with native witnesses. C++ specialized CPU VDF batching remains a performance comparison gap; the GPU path does not establish CPU fallback parity. |
| Harvesting | `src/Harvester.cpp`, `src/pos/Prover.cpp`: concurrent plot jobs, publish each proof immediately, network challenge deadline, estimated Y park search, post-filter before full proof, retained headers | `crates/mmx-farming` and `crates/mmx-tools`: shared bounded Rayon workers for plot reads and nested F1, streaming proofs, `challenge_delay * block_interval_ms`, density-based Y search, positional reads, per-proof park reuse, retained headers and per-quality error isolation. A stalled-plot regression test verifies that another plot can publish first. The daemon now dispatches owned per-plot jobs while its service loop handles later challenges/reports. Worker and queued-context bounds, original deadlines and per-plot leases isolate stalled reads; compare sustained slow-drive throughput before large-farm migration. |
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
from native credit/rate policy and concurrent transaction execution.

The payload consolidation checkpoint follows C++ `Node::write_block` and its
block/transaction indexes. Headers and transactions have separate immutable
values, enabling positional component reads. Selected snapshots pin an archive
view, so rollback and collection cannot invalidate a detached reader. Existing
crash, fork, purge and native consensus fixtures pass with the new layout. The
temporary two-transaction journal fixture wrote 132,638 bytes versus 395,819
with the former duplicate block/transaction payload writes (66.5% fewer). This
is a bounded write-size comparison; mainnet throughput still needs measurement.

Canonical-parent live admission and local signed production now retain one
opaque verified execution handoff. Fork choice can publish its detached delta
without a second body execution. The native store checks owner and publication
generation before retaining or applying it; rollback/purge/another publication
invalidate it. Same-hash-after-rewind and foreign-store regressions reject stale
effects. Unselected-parent branches still use full replay. This is a bounded
execution context, not a node-wide state cache.

Peer gossip now has a sender independent of the RPC/frame receive loop, with one
shared ordered encoder for each connection. Publication wakes its bounded sender
and the native 20 ms tick handles remaining batches. Separate consensus/transaction
cursors prevent old transaction backlog from delaying new proofs, VDFs or blocks.
A real socket test holds both complete RPC replies and partial inbound frames
while a verified outgoing proof still arrives. Shared record ownership and
20,000-entry per-peer echo caches avoid additional payload queues/copies. Native
credit feedback, early earned credits, staged fanout and peer authentication
remain separate tracked work.

Public balance queries now follow C++ `table.h::find_range`: native sorted sources
seek to the owner range, merge visible keys lazily and stop at the row limit.
Only the winning payload is read. Hidden versions, tombstones, binary-search
probes, metadata/read-ahead and result bytes share a request budget (100,000
operations, 32 MiB, 10,000 scanned balance rows). Budget exhaustion rejects the
whole result. Native zero-row, zero-balance, whitelist and exclusive currency
sentinel behavior is covered. Historical/rollback and corrupt-value tests use
small temporary databases. Balance and public validation RPCs use pinned
snapshots outside the ledger mutex; two slots bound expensive HTTP/VNX work.
Lock-held query tests establish scheduling isolation, not production latency or
throughput parity. Other bounded read methods still run under the node lock.

Harvester scheduling now follows C++ `Harvester::check_queue`/`lookup_task` at
per-plot granularity. The service loop owns at most eight queued contexts;
completed/expired queue entries release immediately, independent of old running
I/O. One global Rayon budget and per-plot leases cap concurrent reads and avoid
reopening the same stalled plot for each challenge. Free workers choose newer
heights first, retaining the original deadline. Farm reporting and snapshot
creation take no plot I/O lock. Tests use a FIFO to block one actual open while
a later challenge emits a native-verified proof and farm-info reads complete;
queue/lease tests cover expiry, caps and completion. Plot reload metadata I/O
itself is still synchronous. This establishes isolation; it does not claim
whole-farm C++ throughput parity.

Header and content-hash RPCs now mirror C++ `Node::get_header` /
`get_block_ex(..., false)`: `get_header`, `get_header_at` and `get_block_hash_ex`
read the compact archive header, never the new transaction payload records.
A temporary native-database regression corrupts an archived transaction and
checks that full block reads fail while lossless header/hash replies still work.
Legacy complete-body records retain their in-place fallback; no migration occurs.
Existing purge/RPC floor and native consensus tests pass on this path.

The first t320 overlap build had a slower adjacent live sample: 27 lookups
averaged 3,353 ms (maximum 4,719 ms), versus 1,899 ms across the preceding 30
(maximum 3,032 ms). Both used the same 1,207 plots, 16 workers, CPU fallback and
C++ harvesting disabled; challenges differed and caches were not flushed.
Worker completion now refills capacity directly from the Rayon worker, retaining
its local scheduling behavior instead of waiting for the 20 ms service tick.
A regression queues two native-proof jobs on one worker and obtains both replies
with a single service poll. This removes a definite scheduling wait; the follow-up
live sample is required before attributing the observed slowdown to that wait.
