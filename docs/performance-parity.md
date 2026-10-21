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
| Proof/VDF work | `src/Node_verify.cpp`, `src/pos/verify*.cpp`: cheap checks, worker pools, SHA-NI/ARM/AVX batching and accelerated recompute | `crates/mmx-consensus`, `crates/mmx-crypto/src/recursive` and `crates/mmx-opencl`: bounded work admission, parallel F1, GPU VDF/F1 kernels and runtime-dispatched CPU lane batching. Native CPU kernel and old/new Rust proof benchmarks are recorded below; full-node and ARM throughput parity remain unmeasured. |
| Harvesting | `src/Harvester.cpp`, `src/pos/Prover.cpp`: concurrent plot jobs, publish each proof immediately, network challenge deadline, estimated Y park search, post-filter before full proof, retained headers | `crates/mmx-farming` and `crates/mmx-tools`: shared bounded Rayon workers for plot reads and nested F1, streaming proofs, `challenge_delay * block_interval_ms`, density-based Y search, positional reads, per-proof park reuse, retained headers and per-quality error isolation. A stalled-plot regression test verifies that another plot can publish first. The daemon now dispatches owned per-plot jobs while its service loop handles later challenges/reports. Worker and queued-context bounds, original deadlines and per-plot leases isolate stalled reads; compare sustained slow-drive throughput before large-farm migration. |
| Wallet/farmer | `src/Wallet.cpp`, `src/Farmer.cpp`: retained farmer identities, separate signing work, native key derivation and RPC | Rust retains farmer keys, uses a private signer and a restricted remote harvester listener, and tests native signatures and wallet spending through actual daemons. Large-wallet/history latency and signing under sustained harvester traffic still need comparative measurements. Pool partial submission and other unported RPCs remain listed in README. |
| Timelord | `src/TimeLord.cpp`, `sha256_{ni,arm}_rec.cpp`, `Node_update.cpp`: dedicated serial kernel, infusion forecasts, duplicate/retry suppression, independent signing/publication, retained segment history | `mmx-timelord` and `mmx-tools/src/timelord.rs`: native interval/proof/key formats, single-lane SHA-NI/ARM SHA2, bounded history ring, separate request/publisher/compute threads, generation cancellation, private stop RPC and configured service startup. Intel hybrid CPUs automatically select an allowed SHA-capable E-core; other CPUs retain normal scheduling. Benchmarks below compare native/Rust kernels on both core types. |

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
now have admission budgets. Credit/rate progress is detailed below; concurrent
transaction execution remains separate work.

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
20,000-entry per-peer echo caches avoid additional payload queues/copies.
Credit accounting and earned early relay are detailed below. The authenticated
peer lifecycle and role filtering checkpoint is recorded at the end of this file.

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
blocks and votes stay out of the inbox. Subsequent sections add native first-seen
deduplication, transaction cost accounting and earned early relay. Signed inbound
identity and role filtering now use the same lifecycle as outbound peers.

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
remain open. Authenticated role filtering is now implemented. VDF-only bootstrap and
earned early relay are detailed below.

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

The lazy upload refill clock is monotonic across contending sender threads.
An earlier captured timestamp arriving after a later admission cannot roll the
clock backward and refill the same interval twice. A controlled out-of-order
two-peer regression checks the exact remaining shared credits.

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

Earned early relay now follows C++ `Router::handle` / `on_block` / `on_vdf`:
verified ProofResponses grant one permission for their farmer and VDF height;
verified VDFs replenish one permission for their timelord. Blocks never earn
credit. Farmer pruning uses the previous verified VDF height, preserving better
proof credit at the new height. Native commit-delay replay cutoffs and strict
`segments < Router.max_vdf_segments` apply before early forwarding. Credits do
not bypass final verification or seed the verified-ID cache. Unverified early
records share one Arc with the inbox and have a separate 128-item / 32 MiB
window; promotion keeps fanout timing and sequence, replaces the early record
with the owner's normalized verified record and never resends its body. Farmer
and timelord key maps each have a deliberate 100,000-key bound; timelord eviction
is FIFO, and farmer expiration follows verified progress. Explicit rewind clears
credit and early payloads. Credits are transient across restart, as in C++.
C++ writes diagnostic credit/trust text files but does not restore those maps;
the Rust runtime does not add those periodic diagnostic writes.

Native receipt timing now also follows `receive_msg_hash`: a newly authenticated
envelope broadcasts its receipt before full work validation, even if it lacks
early-forwarding permission. A shared 100,000-ID metadata window keeps independent
consensus/transaction cursors; new connections skip historical receipts. These
notes remain transport claims, never verified evidence or credit grants. Decoded
per-record retention limits precede first-seen insertion, preventing unhashed
padding from suppressing the smaller real envelope. Relay and pool byte budgets
now charge decoded heap footprint as well as compact bytes. Tests hold the ledger
while an earned block is forwarded, refuse a second signed same-height block,
exercise exact key/height/segment/replay/pruning boundaries, verify shared payload
ownership and promotion without retransmission, bound failed early work, reject
padding-cache poisoning and check immediate receipt-only delivery. All storage
checks remain on temporary data. This establishes defenses and scheduling;
whole-network latency and bandwidth comparisons remain open.

Connection bootstrap now mirrors C++ `Router::on_connect` / `vdf_history`:
new peers receive only retained verified VDFs, not the existing block, proof,
transaction or unverified early-relay queues. Per-class body cursors begin at
attachment; receipt cursors already did. History keeps the first verified VDF
per height, shares Arc payloads with live publication and prunes through the
previous verified VDF height when a block publishes. Deliberate safety bounds
are 4,096 history items / 32 MiB and 16 replay bodies / 256 KiB per tick (one
larger record can progress). Live consensus comes first, then bootstrap VDFs,
then transactions. Unlike C++'s insertion inside its first-relay branch, Rust
also retains work that was early-relayed and subsequently verified. Receipt-only
decoder views never retain the sender's history snapshot. Tests cover ordered
history, first-per-height choice, pruning, count/byte bounds, shared ownership,
live priority, failed sends, batching, rewind and absence of old gossip replay.
The live transaction socket fixture now establishes the peer before publishing,
matching native's non-replay behavior. No ledger/plot scans or extra writes occur.

Block existence checks now follow C++ fork/index probes (`add_fork`,
`block_index.find`) rather than reading full archived bodies. Native `contains`
resolves the same MVCC reference as `get`, including pending writes, tombstones,
snapshot overlays, historical limits and retained floors, then stops before
opening or checksumming the payload. It works for split and legacy values.
Live duplicate/parent checks and coordinated-sync missing-parent checks use this
path. Actual validation, replay and RPC body reads still load/checksum payloads;
startup retains its full selected-tip integrity check. Differential temporary
database tests cover every rollback/reopen and nested snapshots. A corruption
test proves presence succeeds while an actual value read fails; the native
header/RPC fixture also probes existence despite a corrupt transaction payload.
This removes body I/O from those predicates without a ledger cache or new writes.

VDF scheduling now also follows C++ `vdf_verify_pending`'s unsigned proof-hash
deduplication. An authenticated second timelord signature over the same committed
work cannot occupy another waiting queue slot or refresh the original deadline.
The worker independently tracks both signed publication IDs and unsigned work
hashes until completion, including when an in-flight proof leaves the retained
queue. Its existing one-active / one-waiting bound remains. Completed evidence
still belongs to its exact original signed record; it is never rebound to a
different key or envelope. A native VDF is re-signed with another valid key in
the queue regression, and a blocked-worker test checks alias suppression after
retention eviction. This avoids duplicate CPU/GPU work; verified-input/trust
priority and specialized CPU SHA lanes are implemented in the later checkpoints below.

VDF retention now also respects native `max_vdf_count` instead of a fixed 32
records. Mainnet's limit is 100; discarding the first work record after 32
could permanently break an otherwise valid continuation from the selected tip.
The item budget now covers `max_vdf_count * (commit_delay + 1)` for the gap and
uncommitted window, clamped to 32–16,384 records; the existing 32 MiB heap/wire
budget remains. A regression extends the native two-iteration fixture into 40
correctly committed/signed VDFs, verifies the whole chain, and checks the peak
advances to 40. No ledger writes are needed. This removes the artificial count
restriction; exhaustion of service byte budgets remains explicit admission
failure and is not evidence that a network proof fails consensus.

Peer lifecycle checkpoint: both socket directions pipeline native v103 ID, info
and fresh signed challenge requests. Full nodes then prove genesis and synced
height; light/API clients authenticate without exporting ledger methods, never
qualify as sync observers and receive no periodic ledger queries. No gossip
sender attaches before these checks. Native pre-handshake VDFs use a 128-item / 32 MiB temporary window;
failed handshakes admit nothing. ID signatures and network identity precede
duplicate connection replacement in a weak socket registry shared by incoming
and outgoing peers. Native full/light/API role filters run before fanout ranks;
reliable local transactions require synced full recipients. Incoming status
refreshes every ten seconds. Socket tests reject ID/signature/challenge replay,
network, role and old-version attacks; a forged reconnect leaves its predecessor
usable, while a fresh valid reconnect replaces it. Role/rank/receipt tests and
existing sync, purge, rollback, RPC and farming tests pass. Rust deliberately
requires signed v103 identities for every connection rather than C++'s unproven
legacy fallback, and withholds gossip until identity/network checks finish.

Block transaction checkpoint: native reservations/locks precede bounded Rayon
execution. The owner immediately merges completed VM deltas and releases ready
dependents, retaining parent-ledger reads and block-ordered errors/publication.
Empty/single-TX blocks, fully serial graphs and nested Rayon calls retain the
original serial implementation. A bounded completion channel and early release
of consumed VM buffers avoid an extra full-block payload copy. All 120 native
fixtures compare exact serial/parallel results and atomic abort behavior; new
native eight-transaction SHA256/write/mint cases cover independent, interleaved
and fully contended graphs at both versions. The stalled-branch regression
checks immediate refill and merge-before-dispatch, and a panic regression guards
the completion channel. A four-worker local release RAM-only benchmark (five
alternating rounds of 100 preparations) measured median us/block:

| Eight transactions | Original serial | Dependency scheduler | Speedup |
| --- | ---: | ---: | ---: |
| Eight independent contracts | 3,715 | 2,007 | 1.85x |
| Two interleaved contract chains | 3,684 | 3,126 | 1.18x |
| One contended contract (serial path) | 3,708 | 3,612 | 1.03x |
| Cached-signature transfers | 42.6 | 48.6 | 0.88x |

The last case exposes roughly 6 us of scheduling overhead for tiny cached work.
These are bounded old-Rust/new-Rust comparisons, not full-node C++ or disk
throughput claims. Run the ignored release `benchmark_parallel_contract_blocks`
test to reproduce. Production candidate selection now uses ordered acceptance and cancellation
with the same bounded scheduler, as recorded below.

Production selection checkpoint: normalize and execute independent candidates on
workers, but reserve fees/inputs and accept effects in fee/ID order. Running and
completed-but-unaccepted candidates together occupy at most the configured pool
width. Rejected candidates never enter the shared VM overlay; successors start
only after the preceding selection decision, including a rejection. Absolute
deadlines stop starting new jobs; fatal storage/normalization errors cancel
further work, and scoped in-flight jobs finish before returning. This preserves
the previous Rust accepted-effects-only behavior rather than copying C++'s
broader speculative context before final filtering. All 120 fixture-derived
candidate sets match the retained serial path byte-for-byte; assembled blocks
pass independent validation. A multi-sender aggregate-fee rejection regression
checks that the skipped call cannot change the funded successor's mint/state.
Expired deadlines retain no candidates and change no ledger state.

The same bounded four-worker RAM-only release benchmark measured median us/set:

| Eight candidate transactions | Original serial | Ordered dependency pool | Speedup |
| --- | ---: | ---: | ---: |
| Eight independent contracts | 3,843 | 1,945 | 1.98x |
| Two interleaved contract chains | 3,830 | 2,974 | 1.29x |
| One contended contract (serial path) | 3,769 | 3,743 | 1.01x |
| Cached-signature transfers | 72.9 | 70.4 | 1.04x |

These five alternating 100-set rounds compare Rust scheduling on native-derived
work, without disk writes. They do not establish complete C++ throughput parity.
The ignored release `benchmark_parallel_candidate_selection` test reproduces it.

VDF priority checkpoint: native inferred-input-height / timelord-trust ordering
now precedes bounded CPU/GPU admission. Only authenticated header outputs and
the verified input/output graph establish heights, never the record's claimed
vdf_height. FIFO breaks equal priorities and verified output completion releases
its own successors. Native score changes are -1 per actual submission, +2 only
for an advancing verified result, and min(score,0)-100 on CPU failure. Signer
identity accompanies worker completion even after queue eviction. A 100,000-key
FIFO bound and saturating arithmetic deliberately bound this scheduling hint.
Retired work returns before history reads; busy workers avoid repeated static
reads. Known input/output aliases do not repeat CPU verification. A bounded repair exception to native's input/output-only check permits a
correct-start replacement only when all its CPU inputs and segments exactly
match checked work, preventing wrong metadata from suppressing recovery without
allowing forged repair claims to occupy CPU jobs. Failed CPU
work is retired, not retried on every synchronous tick, and no checked evidence
is rebound to another record. Regressions cover priority, dependency release,
exact credits/penalties, eviction, aliases, wrong-start recovery and the forty-step
protocol gap. CPU SIMD batching is recorded below.

Reconnect ownership checkpoint: native authenticated identity replacement now
replaces the corresponding sync slot too. A pending old metadata owner and the
latest new socket share one bounded slot until the old operation returns; no
worker is abandoned and no duplicate observer is counted. Nonblocking control
draining during ordinary peer polling promotes replacements even below the
three-peer minimum. Metadata query generations reject stale same-ID completions
before touching a new worker or status. Real socket reconnection preserves the
three distinct sync peers, and a controlled blocked-owner test exercises stale
height isolation and promotion while below the minimum.

VDF alias hardening: inferred iteration counters now accompany inferred heights
in the authenticated work graph, removing reliance on claimed chronological
ordering. A known usable input/output pair suppresses every metadata alias. If
only wrong-start metadata exists, only an exact-computation, correct-start
replacement can take a verification job; it still gets its own exact-record
check. Eight signed forged-intermediate repair claims consume no jobs, while the
genuine replacement advances. Positive trust also requires usable start, infusion
and iteration linkage, tightening native's input-only bonus without changing
block acceptance or proof hashes.

CPU VDF checkpoint: `mmx-crypto/src/recursive` ports native fixed-32-byte recursive
SHA256 with two interleaved SHA-NI/ARM SHA2 states and eight AVX2 lanes. All target
features are runtime-checked before private unsafe kernels. Fixed-size arrays,
unaligned loads, initialized AVX tails and scalar odd hardware tails cover every
length; the 64 hardware rounds are unrolled and digest words stay in registers
between iterations. Ordinary hashes, signatures and VM SHA instructions retain
their existing implementation. GPU-only OpenCL remains the default.

`verify_cpu` caps chunks at native's 16 segments, balances complete worker waves
and preserves SIMD groups. Every segment starts from its supplied predecessor,
including when that predecessor is invalid. A failure skips later pending chunks
but leaves earlier chunks eligible, retaining deterministic first-error reporting.
The kernel has zero/odd/partial-lane/endian/padding differential tests against
RustCrypto; both x86 paths execute locally. The ARM SHA2 kernel also passes local
AArch64 emulation, and a native ARM CI job exercises crypto and consensus suites.
Complete proof tests cover 1/2/15/16/17/31/32/33 segments, multiple iteration
counts and invalid outputs across chunk boundaries.

Local Intel Core Ultra 7 265KF release medians (five samples, RAM only) for sixteen
lanes of 250,000 iterations were C++ SHA-NI 48.972 ms, Rust SHA-NI 48.962 ms,
old single-lane Rust 126.957 ms and forced AVX2 91.877 ms. The native output is
asserted in the ignored `benchmark_recursive_sha256_backends` test;
`tools/reference_recursive_sha.cpp` reproduces the C++ baseline. Forced AVX2 on
a SHA-capable CPU is a kernel comparison, not an older CPU performance claim.

The complete-proof `benchmark_batched_vdf_verification_against_original_parallel_segments`
test compares original parallel single-segment verification with the batched
implementation, four Rayon workers and five alternating samples:

| Segments, 250,000 iterations each | Original Rust ms | Batched CPU ms | Speedup |
| --- | ---: | ---: | ---: |
| 16 | 35.010 | 12.637 | 2.77x |
| 40 | 82.575 | 31.100 | 2.66x |
| 80 | 159.800 | 61.715 | 2.59x |
| 128 | 254.743 | 98.464 | 2.59x |

These measurements establish bounded CPU kernel/proof improvements, without
ledger or plot I/O. ARM emulation establishes correctness, not throughput.

Timelord checkpoint: the latency-oriented single-lane kernel ports native's
look-ahead schedule, constant padding, unrolled rounds and register-resident
recursive digest. A separately guarded AVX implementation permits VEX encodings;
SHA-NI without AVX and ARM SHA2 remain supported. Native's redundant SHA256MSG1
on constant padding is removed. Ordinary hashes and the independent-lane verifier
remain unchanged. The worker chooses an allowed SHA-capable Intel E-core by
CPUID core type, with explicit CPU/normal-scheduler overrides. Probing and
pinning affect only this worker, never the caller or other application threads.

The contiguous history ring removes tree nodes, stored counters and per-segment
heap allocation. Hashing does not wait for signing or network writes. Request
look-ahead is ported from Node::update, including the one verified restart input;
publication refreshes on verified-work/tip changes and repeats cached control
requests every two seconds without new ledger reads. No ledger state cache is
added. Native history/retry semantics and exact signatures match five C++
scenarios generated by executing original TimeLord methods at controlled segment
boundaries. Actual daemon tests cover native configuration, both executable
names, private control permissions, key reuse, invalid-work rejection, verified
VDF progress and automatic node service startup.

Deliberate safety differences: missing/expired restart history never inserts a
zero input. A supplied network input wins when native's is_reset branch would
ignore it. Removing an already-computed middle infusion rewinds to the retained
or supplied start; superseded intervals with interior/conflicting infusions are
never signed. Generations fence stopped/restarted work and cancellation is checked
between 65,536-iteration chunks. Pending/infusion maps cap at 1,000; stop clears
obsolete pending requests too. Authoritative inputs can recover a full forecast
queue, and resets discard forecasts on an incompatible segment lattice; current
infusions take priority over far-future entries at the retention bound.
At the last forecast boundary the worker sleeps
on its condition variable until a new request, rather than computing an unusable
uninfused continuation; look-ahead intervals keep useful hashing continuous.
An idle/resume regression checks the iteration counter stops exactly at that
boundary and new forecasts wake it. Keys are atomically installed once, mode 0600,
and corrupt files fail startup without replacing the identity. The new private
control default is 11334 because Rust's private farmer uses 11332; the native
port remains explicitly selectable. Native diagnostic speed smoothing is retained
but initialized from the first sample instead of underreporting during warmup.

Local Core Ultra 7 265KF kernel medians, seven samples of two million iterations
on fixed logical cores, with no ledger/plot I/O:

| Single-chain kernel | P-core 0 ms (MH/s) | E-core 8 ms (MH/s) |
| --- | ---: | ---: |
| C++ SHA-NI, GCC `-O3 -mavx` | 42.209 (47.38) | 33.104 (60.42) |
| Rust SHA-NI + AVX | 42.395 (47.18) | 33.670 (59.40) |
| Rust SHA-NI without AVX | 42.400 (47.17) | 33.353 (59.97) |
| Generic RustCrypto single-hash loop | 63.476 (31.51) | 53.628 (37.29) |

The Rust kernel is near native throughput; these samples do not demonstrate
a meaningful AVX advantage or improvement over C++ on this machine. E-core
placement improves about 26% over P-core placement, and specialized recursion
improves about 59% over the generic E-core loop. End-to-end `--benchmark` also
confirmed default selection of CPU 8, explicit CPU 0 and unrestricted fallback
when the allowed set contains only CPU 0; all produced the same output. The
reference helper accepts `--single` for the native baseline. Longer production
timelord throughput comparisons remain separate from these short RAM benchmarks.

A further x86 experiment held pre-feed-forward words between iterations and
folded IV additions into the first round constants to shorten that dependency.
It preserved all scalar/native digests, but three alternating runs per core
(seven timing samples each) measured E-core AVX 33.774 to 33.877 ms and SSE
33.471 to 33.601 ms; P-core changes were under 0.3%. There was no useful gain,
so the native-style schedule was retained instead of adding another variant.

Shutdown checks assert successful SIGINT exit for both timelord executable names.
Closing the request stream after the stop flag is set is normal shutdown, rather
than a daemon failure that could trigger an unnecessary supervisor restart.

## GNU builds and remote GPU harvesting

The t320 CPU-load investigation found a deployment gap: the portable static musl
harvester could not dynamically load its installed NVIDIA OpenCL ICD. It used
CPU proof computation despite having an RTX A5000. The GNU build on the newer
CI runner previously required a newer glibc than t320's Ubuntu 20.04/glibc 2.31.
GNU binary packaging now builds inside Ubuntu 20.04 and checks each packaged
binary's required glibc symbol versions against that baseline. The static musl package
remains a CPU-only option. This changes packaging, not plot or consensus logic.

A local two-job release build in an existing Ubuntu 20.04 build container was
deployed to t320 after checking its checksum and full owned process identity.
Only the Rust harvester restarted; C++ harvesting remained disabled. The same
configuration, 16 shared workers and 262144 F1 limit were retained. Measurements
used ordinary farming activity and process counters, without additional plot
lookups or cache flushes.

Two 120-second windows, with twelve natural challenges each, measured:

| Deployment | Mean CPU (100% = one logical CPU) | Median lookup | Maximum lookup | Queued plots skipped |
| --- | ---: | ---: | ---: | ---: |
| Static musl, CPU fallback | 167.2% | 1648 ms | 3277 ms | 0 |
| GNU/glibc 2.31, OpenCL A5000 | 39.1% | 1096 ms | 1521 ms | 0 |

The observed CPU reduction was 76.6%, with a 33.5% lower median lookup time.
These windows contain different challenges and include the allocator/runtime
change; they do not isolate GPU effects or establish C++ performance parity.
The restarted harvester reported 1,207 plots and 146.2 TB effective farm size.
Both sample windows produced no qualifying proofs, so they measure lookup load
rather than successful proof publication.

The RAM-only hardware check on t320 compared actual GPU outputs with CPU outputs:
256 chains of 10,000 SHA iterations took 16.2 ms on GPU versus 310.6 ms on four
CPU threads; 16,385 F1 memory hashes took 4.76 ms versus 102.8 ms. Both outputs
matched exactly. Startup also passed the normal accelerator self-tests, and the
live log confirmed `Using OpenCL GPU 'NVIDIA RTX A5000'` without driver errors.

The reboot follow-up compares native `scripts/systemd/{mmx-node.service,install.sh}`.
Rust now provides a persistent user-service installer with restart-on-exit,
journal logging and SIGINT shutdown. Lingering starts these services at boot;
node, farmer, wallet, harvester and private SSH tunnel processes remain under
their owning account. Installation uses per-user units instead of native
system-wide units; a host may require authorization to enable lingering.
Mount/free-space preconditions preserve
the existing ledger location and low-disk startup guard. The live deployment
replaced transient units/screen sessions and verified the same five farmer
identities/reward address through a dedicated Rust wallet; C++ wallet startup
is no longer required. Unit argument escaping, validation and storage
preconditions have regression checks. No ledger/plot format or consensus logic
changed.

## Harvester idle memory

After the GNU/OpenCL deployment, t320's 16-worker harvester retained about
1,025 MiB RSS between challenges. `/proc/PID/smaps` attributed 932 MiB to
anonymous memory, including sixteen worker heaps of roughly 53–57 MiB; NVIDIA
library mappings contributed about 84 MiB. Inspection of
`pos::compute_with_accelerator` and native `pos/verify_full.cpp` confirmed that
proof computation uses temporary vectors which are released after the lookup.
These maps pointed to allocator retention rather than a deliberately retained
plot cache.

The persistent harvester service now sets `MALLOC_MMAP_THRESHOLD_=131072`.
This fixes [glibc's mmap threshold](https://sourceware.org/glibc/manual/latest/html_node/Memory-Allocation-Tunables.html)
at 128 KiB instead of allowing its dynamic policy to move large allocations
into worker heaps. Large temporary buffers can return to the OS when freed.
The service example includes the setting; it applies to GNU/glibc builds and
does not cap memory required by active proof computations.

With the same binary, A5000, 800 available plots, 16 workers and 262144 F1 limit,
the post-change 120-second sample ended at 130.5 MiB RSS, with sampled RSS at
most 168.9 MiB and an observed process high-water mark of 221.2 MiB. The old
process had remained at 1,025.3 MiB through its 60-second sample. Mean CPU was
11.3% before and 11.2% after (100% means one logical CPU). No plots were skipped
or GPU errors reported. Fifteen new lookups had a median of 1,236 ms, versus
690 ms in the preceding eighteen logged lookups; these different live challenges
are not an isolated throughput comparison. Normal farming was the only source
of plot reads. Only the owned harvester restarted; node and farmer stayed up.
The deliberately removed SSD was also removed from the configured plot roots;
it had already contributed no plots to either sample.

## Home and network path resolution

The path follow-up reads native `activate.sh`, `run_node.sh`,
`run_harvester.sh`, `src/mmx_{node,wallet,harvester,timelord}.cpp`, and
`modules/{Node,Wallet,TimeLord}.vni`. The shared `mmx-config` resolver now uses
`MMX_HOME` (working directory by default), the home `NETWORK` selector,
home-local `MMX_DATA` before the environment, and an explicit `MMX_NETWORK`
override. Default config layers are installation network, installation role,
then home-local; explicit `-c` lists replace discovery. The node passes resolved
home/network directories to its owned children without mutating global process
environment. Relative wallet storage is home-based and timelord/node database
storage is network-based. Explicit CLI data paths and absolute paths remain
explicit. Selector reads are bounded to 64 KiB, happen only at startup, and do
not enumerate ledger or plot contents.

Temporary-directory tests cover selector/env/CLI precedence, paths with spaces,
missing trailing slashes, invalid selectors, layer order and absolute storage.
The daemon regression starts node, wallet, farmer, harvester and timelord from a
separate home without `-c` or `--data-dir`, syncs against three synthetic peers,
checks the home-based wallet, network-based ledger/key, and shuts down owned
children. The existing explicit-config/data-dir scenario remains covered.
An incompatible C++ `blocks.dat` is rejected before creating Rust ledger files.
No live data is moved or read for these tests; consensus and steady-state I/O
are unchanged. This is startup compatibility coverage, not a throughput claim.

Deliberate differences: Rust does not run the C++ activation script's password
generation/config-copy side effects. Missing optional config directories are
skipped; automatic service startup requires a home-local directory or explicit
`-c`, retaining node-only startup for unconfigured installations. Invalid network
selectors fail early. Absolute paths override their base rather than using C++
string concatenation. The Rust node keeps its transport identity inside its
ledger directory; `Node.storage_path` does not relocate that identity. The C++
ledger format remains incompatible and must use a separate directory.
