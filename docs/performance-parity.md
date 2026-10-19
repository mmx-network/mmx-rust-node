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
credit accounting, early earned credits and peer authentication
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

The refill deployment's first 30 t320 lookups averaged 1,549.9 ms (maximum
3,403 ms), with no queued plots skipped. The same 1,207 plots and 16 CPU workers
remained active and C++ harvesting stayed disabled. These are adjacent live
samples with different challenges, not a controlled same-challenge comparison.
The source-level refill regression proves removal of the scheduling wait;
these samples support keeping the corrected deployment under observation.

Native `Router::recv_notify`/`on_recv_note` feedback now works across Rust peers
and C++ peers. Receipt control frames are consumed during RPC reads using the
same absolute frame deadline; they update the shared 20,000-ID peer cache without
ledger/output locks. Verified relay publications acknowledge before forwarding.
A receipt suppresses only that peer's payload and never earns consensus credit.
A separate 100,000-ID verified-publication cache drops known gossip before
queueing it for the ledger. Payload eviction does not erase these IDs, replays
do not refresh their FIFO age, and explicit rewind resets them. Tests cover
actual receipt/RPC framing, suppression on the wire, cross-peer isolation,
cache bounds, rollback/reacceptance and a socket barrier proving known proofs,
blocks and votes stay out of the inbox. Native first-seen envelope deduplication,
per-peer/global transaction cost accounting, earned early relay
and full inbound identity/role lifecycle remain separate work.

Purged-peer identity: legacy C++ Router exposes the genesis hash through
`get_block_hash(0)`. A purged Rust node intentionally returns no historical hash
there, so authenticated qualification now falls back to the constant
`get_genesis_hash` only when height zero is absent. That cached identity read is
allowed on the peer surface. Unpurged C++ peers use the unchanged path, and a
native temporary-database/socket test proves purged history stays unavailable
while the peer authenticates and rejects the wrong network.

Peer fanout now follows `Router::send_to`: shuffle connections once per
publication, then assign distinct ranks spaced by `target / (N+1)`. The native
`Router.relay_target_ms` setting is honored; the repository default is 2,000 ms.
Receipt cursors run immediately, independent of body timing. Body selection can
pass a deferred item to send ready consensus work behind it. A bounded sparse
sequence set records completion independently of the evictable peer hash cache,
so receipt floods cannot cause those out-of-order payloads to repeat. Selection
examines at most 256 entries per class/tick and retains at most 4,096 completed
out-of-order sequence numbers per class. Payloads remain shared; there is no
new per-peer payload queue. Eight-connection rank/lifetime tests and native
socket/production regressions pass. Whole-network bandwidth/latency comparisons
remain open, along with native VDF-only new-connection replay,
earned early relay and inbound identity/role handling.

Transaction upload credits now follow C++ `Router::send_to`, `send`, `update`
and `on_recv_note`. The global budget uses native static-cost/MMX conversion,
probabilistic admission and a one-second refill cap. Per-peer pending cost uses
the native pre-add threshold, matching-receipt release and 0.999/second decay.
Local RPC submissions retain reliable-send bypass; foreign and orphan-requeued
transactions use the limiter. No payload copies or disk reads are added.
Elapsed-time lazy refill/decay replaces periodic accounting, and a deliberate
20,000-ID bound prevents unacknowledged zero-cost messages growing the native
pending map without limit. Native zero upload means no global limit; zero
pending cost stops foreign forwarding. Controlled-time tests cover probability,
idle refill, independent peers, withheld/late/unrelated receipts and map bounds;
a native socket fixture checks local versus foreign admission. This establishes
transport behavior, not a whole-network bandwidth benchmark.

Peer framing now follows C++ `vnx.addons.MsgServer::on_read`: the production
connection rejects compressed sizes above `Router.max_msg_size` (default 8 MiB)
from the header before reading or allocating the body. Incoming and outgoing
connections apply the same configured bound, including the independent sender.
The separate Rust 64 MiB decoded-byte bound remains; a highly compressible
record larger than 8 MiB is still accepted as in C++. Local configuration above
64 MiB or zero is rejected. Socket tests hold an oversized header open without
a body, cover both byte orders and a configured lower bound, and decode a valid
record exceeding 8 MiB raw. Failed output encoding preserves connection schemas
and sends no batch prefix. No ledger or plot reads are involved.

ProofResponse verification now follows C++ `Node::verify_proofs` and
`Node::verify(ProofResponse)`: all eligible, unexpired proofs run through the
shared Rayon CPU budget, and the owner consumes completed results immediately.
The full batch joins before block planning or accepting an asynchronous signature.
This removes the former 16-response cutoff that could postpone a queued stronger
proof. The existing 4,096-response / 8 MiB inbound bounds remain. Immutable
challenge/difficulty contexts resolve selected history once per distinct height;
workers perform no database reads, and still check every envelope, signature,
root boundary, exact height and space proof. A 16-result channel provides
backpressure without duplicating the whole queue. One temporary coordinator
dispatches work; the owner applies results without a worker/ledger mutex.
Embedded callers already inside Rayon use sequential outer dispatch to avoid
pool starvation; nested space-proof computation still uses their pool.
Tests hold one job while a later completion is consumed, join all work after an
owner error, exercise a nested single-worker pool, reject frozen-context misuse,
and put twenty authenticated invalid proofs before the valid stronger proof in
the native production-veto fixture. These establish scheduling and safety;
representative proof-batch throughput comparisons remain open.

First-seen gossip now follows C++ `receive_msg_hash` / `on_{block,vdf,proof,vote,
transaction}` before ledger work. Decoders share a separate 100,000-ID FIFO;
concurrent arrivals authenticate outside its short lock, then atomically admit
only one copy to the inbox. Body commitments, transaction static validity and
native envelope signatures precede first insertion. That deliberate ordering
hardens C++ paths which insert before signature checks. Claimed IDs, receipts
and first-seen envelopes never seed verified work or earn early relay credits.
Replays do not refresh FIFO age; explicit rewind clears both caches. As in C++,
queue eviction does not erase the first-seen record. Invalid unsolicited
envelopes now terminate their peer before owner work. Tests mutate signed block
transaction bodies, VDF/proof signatures and transaction nonces while retaining
the real content ID, then accept the real envelope; eight simultaneous arrivals
enqueue one copy. Lock-held native socket tests cover rejection and independent
valid delivery. Verified IDs also guard VDF record construction/publication after
the smaller retained-payload window has evicted old records, avoiding cyclic
republication during a long wait for a block. There is no ledger I/O or state
cache in the decoder path.
