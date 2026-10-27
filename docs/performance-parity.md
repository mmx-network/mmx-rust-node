# C++ performance parity

The [2026-10-25 review](cpp-port-audit.md#codebase-review-2026-10-25) found six
admission/storage gaps. The [fix checkpoint](#review-fixes-2026-10-25) records
their resolutions, new bounds and deliberate differences. Earlier fixture
results do not establish performance parity for those paths.

Performance is part of the migration requirements for every component. Read the
C++ counterpart before implementation and preserve its intent, functionality,
performance, reliability and robustness, as well as consensus results. Preserve
the useful properties of its execution strategy; Rust may achieve them with
different internals, scheduling mechanisms or a smaller configuration surface.
One-to-one code or config replication is not a completion criterion. Missing
capabilities, lost operational controls and weaker guarantees are gaps; omitted
framework knobs or architectural differences alone are not. Comparable
measurements, including failure/overload/recovery cases, are required before
claiming equivalent outcomes. A source comparison alone does not establish them.

The initial comparison below uses `../mmx-node` at
`5626ac2b7cd26aa9626ea3b2cf864b4cfe6fbcd2` (2026-10-19). Native fixtures retain
their own recorded source revisions. The [full subsystem audit](cpp-port-audit.md)
records the follow-up fixes and prioritized remaining work. New block headers
and transactions now use a single archive payload store and selected-chain
locators. Existing native payload records are read in place without migration.

| Area | C++ reference and important choices | Rust coverage and remaining checks |
| --- | --- | --- |
| Database | `src/DataBase.cpp`: buffered append log, sorted blocks, versioned rollback, flush thresholds | `crates/mmx-db/src/{native,journal,sorted}.rs`: append values once, index references, positional reads, bounded reader handles, batched checkpoints, historical versions. Rust synchronizes each published journal commit; compare durability and write throughput explicitly before changing this policy. |
| Sync | `src/Router.cpp` height jobs and `src/Node.cpp`: random peer subsets, content-hash observations, unique body downloads, overlapping bounded jobs | `crates/mmx-node/src/sync/peers*`: configured distinct authenticated peer pool, all observed content candidates, content deduplication and timeout reassignment, background peer refill and prefetch. Socket tests cover competing branches, duplicate providers and refill while syncing. See the Router configuration checkpoint for default/timeout changes and scheduling differences. Repeat throughput measurements after scheduling/storage changes. |
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

## Wallet app and `mmxwallet` service

The desktop and web wallet (`apps/wallet`, `mmxwallet::service`,
`crates/mmx-wallet-wasm`) are new clients of the public node API, not ports of
the C++ GUI; their request patterns were chosen for a remote, load-balanced
server rather than measured against C++.

- Public addresses persist after the first unlock: bounded caches in web
  localStorage and desktop/CLI `mmxwallet.json`, and an existing `mmx.WalletFile`
  beside the node seed (fingerprint/account-specific filename, at most 10,000
  addresses and a 1 MiB read limit). Locked wallets continue observing without
  deriving spending keys. Import discovery retains the addresses it already
  derived; changing the visible count within that cache needs no passphrase.
  Missing addresses require a new passphrase entry even during an open session.
- Passphrase wallets cache derived signing keys until manual lock or the existing
  inactivity timer (default five minutes, refreshed on spending). Desktop clears
  its last key owner on lock; WASM frees and zeroizes its Vault. In-flight work is
  invalidated by a lock generation. Native GUI timing remains owned by its
  existing frontend idle timer; the node has its existing 100 ms expiry poll.
  Regular disk wallets stay usable, while regular web wallets drop their seed
  and keys and need the password manager again. CLI commands observe the public
  cache and derive keys in the local signing block, dropping them before network
  validation/broadcast. No change is made to the C++ wallet.
- Plaintext base seed compatibility is retained; there is no extra encryption
  or encrypted VNI type. Protected web wallets persist their base recovery words
  so unlocking requires only the passphrase, which is never persisted. The node
  farmer key remains independent of that passphrase. The additive Rust-only
  `Wallet.set_address_count_ex` RPC carries derivation authorization. Existing VNI
  type/code identities are preserved (including the historical schema timeout
  default); the Rust runtime's actual default is 300 seconds.
- Real-daemon regressions cover regular disk wallets remaining usable, reuse of
  a passphrase spending session across RPC calls, manual lock refusing further
  signing, and cached public addresses surviving lock and daemon restart while
  the seed file stays byte-compatible. These use disposable wallets and a
  synthetic node, without reading live wallet or ledger data.

- Balances are fetched 4 addresses at a time (CLI, service, web), with in-flight
  de-duplication of identical GETs in the UI and a 20 s deadline per request.
  The CLI used to fetch them one by one.
- History refresh re-reads the newest 6 blocks of every address instead of
  `since = top + 1`, replacing those rows, so a short reorg or a lagging server
  behind the load balancer cannot leave stale or missing rows. History is cached
  in memory per open wallet only (capped at 20,000 rows); nothing is persisted.
- Pending transfers are checked with `/transaction?id=` only while pending; a
  sync runs at most every 20 s, polling `/node/info` every 10 s while visible.
  Automatic UI polling and chart-history extension pause while hidden and resume
  when visible. The first address, funded/recently active addresses and recipients
  of own pending sends retain the 20 s cadence; unused spare addresses are checked
  once a minute. Manual refresh checks every address immediately. This deliberate
  latency tradeoff is needed because public `/address` has no history-change marker
  and `/address/history` accepts one address, unlike C++'s batched node calls.
  The shared web/desktop UI defaults new wallets to three addresses, preserving
  saved counts and the one-time import scan of up to 100 addresses. Unlocking the
  current wallet reuses its public view instead of fetching every address again.
  Fake-clock regressions bound a three-address unused wallet to at most 10 history
  reads over two minutes, versus the previous default's roughly 60 reads (ten
  addresses every 20 s). They cover manual refresh, new funds on spare addresses,
  hidden/resumed polling and reuse on unlock. This is request-count evidence from
  a synthetic fixture; production latency and server-load comparisons remain open.
- Max and Review fetch `/chain/info`, `/node/info` and fresh balances every time
  (web and desktop) so a transfer is never signed against stale balances.
- Review signs and checks transfers locally (hashes, cost, signatures, fee payer
  coverage, static fee = static_cost × fee_ratio / 1024). No request carries a
  signed transfer before "Send now"; then a read-only `/node/info` expiry check
  and the node's dry run `/transaction/validate` run before it is posted once to
  `/transaction/broadcast` (web, desktop and CLI). The WebAPI broadcast ignores
  `did_fail` (`add_transaction(tx, false)`), so this extra round trip after
  confirmation is what the C++ wallet's `add_transaction(tx, true)` does in one
  call; nothing is posted when it fails or cannot be checked.
- A full balance read (every 5 min) runs beside the history refresh; addresses
  whose refresh found new rows are read once more before publishing, so balances
  never predate the history shown with them.
- Own broadcast-but-unconfirmed transfers are reserved per wallet in memory, as in
  C++ `ECDSA_Wallet::update_from/update_cache`: sender static fee plus own inputs,
  subtracted (saturating) from fresh balances before Max and Review. Difference
  from C++: C++ prunes with one `get_tx_ids_since(height - commit_delay)` call; the
  public API has no batch equivalent, so the clients look up `/transaction?id=` per
  pending transfer (4 at a time; the web runs them in parallel with `/chain/info`
  and `/node/info`, before balances; the desktop at most once per entry per new
  block), only while transfers are pending, and drop entries once included or
  `expires < height`. Desktop entries carry their network and only reduce
  balances read from it (the chain server can be switched). Failed lookups keep the entry. At most 256 per wallet; not
  persisted across restarts or page loads (C++ `pending_tx` is in memory too).
- Listing desktop wallets computes up to two fingerprints (16,384 SHA-256 rounds
  each) per key file with no cache; it runs only on user actions.
- The exact maximum sendable amount is searched with at most about 5n candidate
  input selections (n = owned addresses), stopping at the first sendable one.
- Chromium resends a POST once when a reused keep-alive connection closes
  without a reply. A broadcast can therefore reach the node twice; it is the
  same signed transaction (same id), which the node accepts idempotently while
  it is pending, so no second transfer can result.

## Explorer

The new `apps/explorer` reads native `www/explorer/public/{index,explorer,mount}.js`,
`WebAPI.cpp::{render_headers,render_transactions,render_balances,render_history}`,
and `Node_api.cpp::{get_tx_ids,get_history,get_farmed_blocks,get_farmer_ranking}`
before changing the client. It shares the wallet's actual Obsidian CSS, fonts
and UI components. It does not change any node, consensus, database, farming or
wallet state code and still uses the existing public WebAPI.
Primary navigation uses text links at the top on every screen size; this
layout change adds no API requests or changes to refresh scheduling.
Network space is displayed with one decimal place (the native explorer uses
three); the GB source units and decimal unit conversion are unchanged.

The overview's TPS statistic reuses the displayed headers' transaction counts
and millisecond timestamps. It divides the mean count per block by the observed
mean block interval (20 timestamps span 19 intervals), rather than inflating
throughput by dividing all 20 counts by only 19 intervals. Missing/inconsistent
headers show an unavailable value. This adds no RPC reads, retained state or
polling changes; the browser regression asserts the original two initial
requests (network info and 20 headers).

Native request bounds and visible-page polling remain: 20 headers / 10 s,
100 transactions / 10 s, 200 ranked farmers / 30 s, farmer summary and 100
farmed headers / 30 s, balances / 10 s, approximately 200 history rows / 60 s,
and network info / 300 s. Contract and individual block reads run on navigation;
an individual transaction refreshes every 10 s. Independent address and farmer
requests run concurrently and publish separately; no per-row detail fanout is
introduced. Balance/NFT requests retain the public server's default limits.

Deliberate differences and paging constraints:

- Visible tables render 20 rows at a time. Historical block pages request only
  their own 20-header range using a height anchor in the URL. The server's
  `min(limit, offset)` excludes genesis, so a boundary page adds one explicit
  `/header?height=0` read. No next-page prefetch or chain-wide scan is added.
- Other list pages slice the native bounded response windows, including the
  full final block returned by address history. The public API does not expose
  a global transaction offset, farmer-ranking offset or farmed-block upper
  cursor. Arbitrarily deep paging for those lists needs server API work; the
  client does not emulate it by repeatedly expanding limits or scanning blocks.
  Direct detail links and block transaction lists remain available.
- Automatic list updates pause after page one so records do not shift during
  paging. Hidden tabs pause/cancel requests and resume once due; native timers
  continue in the background. Refresh schedules from completion, preventing
  overlap when a server takes longer than its interval. This can make the
  effective interval longer than C++ under sustained latency.
- Identical in-flight GETs share one request with independent cancellation.
  Leaving a page aborts its subscriptions; late results cannot replace the
  newly selected page. Each request has a 20 s deadline and 16 MiB streamed
  response bound. Only the current result window is retained; there is no
  accumulated history, durable response cache or new ledger-state cache.
- Invalid search input is rejected locally, including MMX checksums and uint32
  height limits. A failed hash lookup only falls through to transaction lookup
  for an absent header, never for a syncing/unreachable server. Raw integer
  amounts are formatted without floating-point rounding when supplied by the
  API; balance fields still have the public API's floating-point precision.

Synthetic regressions exercise a complete 66-height chain down to genesis with
four header-list requests and one genesis read; transaction paging makes one
bounded list request across multiple UI pages. They also cover a moving tip,
legacy links, malformed search, shared/cancelled requests, response/time limits,
failed-refresh recovery, uint128 amounts, detail-table paging and mobile dark
mode. These are bounded correctness/request-count comparisons against the native
API contract, not native/Rust throughput parity. No live ledger/plot storage is
read by the tests. Native UI/browser CPU, rendering, network latency and
large-contract comparisons remain unmeasured.

## Web app HTTP image

The `mmx-apps` Docker image builds the production web wallet (including WASM)
and explorer, served by hostname on one configurable HTTP port behind a CDN.
Reference paths read first: C++ `src/mmx_node.cpp` (`FileServer_1`/`HttpServer`
setup), `vnx-addons/src/FileServer.cpp` (`http_request_async`, range/chunk
reads and cheap path/method checks), `vnx-addons/src/HttpServer.cpp` (immediate
reply, bounded write/chunk handling and compression workers), and the native
Dockerfile/entrypoint.

This is a separate static origin, not a port of the native authenticated node
HTTP API. nginx uses concurrent nonblocking delivery, sendfile, conditional
and byte-range reads, and immediate file/host/method rejection. There are two
workers with 1024 connections each; files are not buffered in an application
cache. Compressible build outputs get gzip sidecars once at build time instead
of native per-request compression work. Only successful hashed assets get
one-year immutable caching; HTML revalidates and errors use no-store. Each
app's generated security policy is translated at build time, including custom
RPC origins. There is no ledger/plot access, file-upload/delete API or directory
listing in this image. Rust builds use two jobs without incremental/debug
state, and frontend builds run sequentially with two-worker environment limits.

The Gigahost console follow-up uses nginx's inherited stderr descriptor for
startup, error and request logs (`-e stderr`, `error_log stderr` and an empty
access-log filename via nginx's `ngx_conf_open_file`). This preserves request
logs without reopening root-owned VM console pipes, adding a log relay process
or writing temporary log files. Log delivery remains immediate; both access and
error records now share stderr. A disposable supervisor fixture opens console
pipes as root, drops to nginx and verifies startup, delivery and logging. The
previous published image fails that fixture with the reported permission error.

Regression checks exercise real disposable read-only/non-root containers,
default/overridden hostnames and ports, gzip/plain delivery, cache headers on
200/206/304/404/405 responses, framing/security headers and WASM MIME/range
handling. They use synthetic loopback requests without contacting chain APIs.
Native/nginx concurrent static throughput, CPU/RSS and cold-file latency have
not been compared; these tests do not claim performance parity. A replacement
image contains only one release's assets, so uninterrupted older lazy-loaded
sessions require retaining previous hashes at the CDN/object origin. CDN
behavior itself requires verification against the chosen provider's settings.

## Review fixes, 2026-10-25

Native source inspected: `Node.cpp::{add_block,add_transaction,write_block}`,
`Node_update.cpp::{verify_block_proofs,validate_new}`, `Node_api.cpp::get_block_ex`,
`Farmer.cpp`, and `DataBase.cpp::Table::insert_entry`, at
`141054c398d2bab6802174379ce3b191d4f3a499`.

- Historical windows now enter a 4,096-item / 128 MiB RAM quarantine, with a
  120-second missing-parent timeout and 8,192-content negative cache. Arrival
  order, three-observer/all-content downloads, prefetch and body sharing remain.
  A detached branch executes once; only a fully valid branch is archived in a
  batch, before its ledger delta publishes. Rejected candidates write no body
  or invalid-marker records. Existing archived invalid records are not migrated.
  Temporary header overlays preserve ancestry beyond the 2,048-header read
  cache and disappear at publication. Rollback/purge bounds on durable history
  remain independent of quarantine limits.
- Deliberate sync difference: unvalidated prefetch survives cancellation in RAM,
  but not a process restart. It is fetched once again after restart. Fully
  validated archived content still avoids new downloads after restart/rewind.
  Losing speculative downloads is preferable to permanently writing unverified
  network data. Timed-out quarantine entries can also be fetched again; there
  are no duplicate concurrent body leases.
- JSON-RPC allows at most 32 batch calls (including notifications), a shared
  32 MiB serialized reply budget, bounded response conversion and 128-byte
  serialized IDs. The HTTP absolute deadline also bounds mutex acquisition and
  stops further batch execution; it cannot preempt one already-running,
  consensus-cost-bounded VM call. HTTP writes headers/body separately instead
  of copying the full response string. These are service limits, not consensus.
- Submissions check size, expiry and pool capacity before VM work. HTTP/VNX
  validate outside the live mutex and recheck the snapshot owner, version,
  generation, reset epoch, capacity and pending fees before publication. The
  transport-enabled node queues peer work FIFO (256 items / 8 MiB), deduplicates
  IDs and uses two persistent workers sharing the RPC execution slots. Each
  completion imposes an equally long scheduling pause to bound sustained peer
  validation duty. Stale work is retried against a fresh snapshot; reset work
  is discarded. Direct embedded `Node::add_transaction` retains synchronous
  semantics. Native uses randomized dependency-aware batches; Rust uses bounded
  independent snapshot jobs, since admission does not publish ledger writes.
- Farm reports have per-field limits, a 256 KiB encoded limit and a conservative
  32 MiB retained allocation charge, including replacements and expiry. ID
  rotation shares that global budget; metadata identities remain informational.
  The remote VNX reader enforces message/string/container/schema/allocation
  budgets before runtime queueing. Report, proof and challenge drains are
  bounded per service iteration. These bounds deliberately exceed native's
  item-only protection; proof/signing permissions remain unchanged.
- Named GPU indexes now count matching names across platforms, as in native;
  unnamed indexes count GPUs within the configured/first platform. Selection
  precedes device initialization. CPU devices remain excluded.
- Native DB memtable accounting updates on insertion, replacement, tombstones
  and rewind removal. Ordinary commits no longer walk unrelated keys. The same
  debug-profile tmpfs probe (100 one-key commits) now takes **0.322 ms** with
  10 seed rows and **1.624 ms** with 50,000 seed rows, versus **0.575 / 151.750 ms**
  before. This is a bounded CPU/accounting comparison, not C++ or mainnet
  throughput parity. No live ledger/plots, broad state cache or format migration
  was used.

Regression coverage includes signed contextual/ledger-invalid historical blocks,
archive-free staging/expiry/budget failure, atomic promotion, restart and every
retained rollback height, two lying sync peers, timeout-only lease replacement,
submission execution while the owner holds the live mutex, Native ABA rewind,
full-pool rejection before worker acquisition, bounded FIFO admission, aggregate
RPC byte/call/deadline rejection, remote report limits and actual VNX ingress.
Mixed/duplicate device names use pure selection fixtures; no mixed-GPU hardware
performance result is claimed. Full mainnet replay, saturated RPC/harvester
latency and native/Rust transaction throughput remain comparative workloads to
measure; correctness tests do not establish those performance claims.

## Router configuration checkpoint (2026-10-26)

Reference: C++ `141054c398d2bab6802174379ce3b191d4f3a499`,
`modules/Router.vni`, `config/default/Router.json`, `src/Router.cpp`
(`init`, `connect`, `process`, `on_connect`, discovery and relay/cache paths),
and `vnx-addons/modules/{TcpServer,MsgServer}.vni` plus their implementations.
The native default JSON is retained as
`crates/mmx-node/tests/fixtures/router-default.json`. The
[configuration reference](development.md#router-configuration) lists supported
keys, precedence, units, validation and the remaining unsupported settings.

Native `max_connections` is an inbound admission threshold, not a strict total
socket cap: `TcpServer::do_poll()` counts inbound, outbound and connecting
sockets, while `connect_client()` can still dial above the threshold. Rust now
uses a shared atomic count with RAII ownership before authentication/dialing,
replacing the fixed 32-inbound-worker cap. The C++ file supplies 100; eight
outgoing sockets leave 92 inbound slots. Rejected names/IPs do not authenticate,
and blocked names avoid DNS entirely. Configuration is parsed before opening
the ledger. Negative limits disable the admission bound as in native.

Outgoing targets, sync minimums, fixed/master peers, candidate bounds, listener
settings, idle/query/connect/fetch periods, compression and the existing relay
caches now consume their native keys. The C++ default file supplies 8 outgoing
peers and 5 observers. Explicit 1/1 and larger targets work too; the minimum
is clamped to the outgoing target. Fixed peers have extra slots. As in C++,
retry deadlines do not hold an underfilled pool below its target; the connection
cycle controls retries during startup. The peer recovery checkpoint below adds
asynchronous periodic discovery, bounded full-pool probes and persisted
authenticated endpoints without reproducing native timer/rotation mechanics.

Existing bounded background connection/metadata ownership, identity
deduplication, randomized candidate selection and immediate completion admission
are retained. New attempts wait for the configured connect cycle; completed
attempts do not. The metadata reply queue scales with configured pool capacity.
Body requests retain an independent aggregate bound of 32 rather than multiplying
it by the configured peer count. The full fetch deadline now follows config;
body leases and body RPC deadlines use its native one-eighth value (22.5 seconds
with the C++ file), so a silent provider cannot occupy a group for the full
180-second fetch interval. No payload/state cache was added. Configured
first-seen limits apply to each existing seen cache, while receipts retain their
separate 100,000-entry bound. Per-peer cache limits use a shared atomic setting
without adding a global relay lock to each received-message cache update.

Deliberate differences and missing optimizations:

These observations record behavior and evidence still needed. They are not a
requirement to copy C++ implementation details. In particular, using threads
instead of an event loop is not itself a defect; ready-work latency, bounded
resource use, peer isolation and recovery under load must be comparable.

- Rust requires the configured number of fresh successful distinct hash
  observations; native can count failed/empty observers toward completion.
  Rust fetches every observed content candidate once without majority filtering.
  Historical sync intentionally uses outbound connections only, limiting who
  can become a historical observer/download source. Inbound gossip and the
  restricted peer RPC service remain bidirectional. Fixed peers must qualify
  as synced full nodes before pool admission; all downloaded data still needs
  full consensus validation.
- The independent completion checkpoint below removes body/poll join barriers
  and retains partial successes as individual replies arrive. Absolute RPC
  deadlines differ from native inactivity timers; failed/empty bodies retain
  their lease instead of releasing it early. The separate three-second connect,
  ten-second write/partial-frame guards and 64 MiB decode limits remain.
- Native uses an event loop and asynchronous write queues. Rust retains an
  incoming worker and independent sender per authenticated socket, plus bounded
  connection/metadata workers. Raising configured limits changes thread and
  memory costs. The send-pressure checkpoint below enables `max_write_queue`
  and bounded ordered output from authentication onward. `priority_queue_size`
  and native timer/topic queue controls without equivalents remain unsupported.
- Sender publication notifications remain immediate; `send_interval_ms` controls
  idle waits rather than exact native timer phasing. `do_relay` gates forwarded
  publications while preserving reliable local broadcasts, receipts and VDF
  bootstrap. UPnP and Router topic/server/version/mode overrides remain unported.
  The socket/lifetime follow-up below covers native buffer/keepalive/backlog
  controls and periodic inbound disconnects.
- Missing settings retain legacy Rust loopback/three-observer/15-second RPC and
  body fallbacks; loading the native JSON overrides them. Absent `tcp_no_delay`
  retains Rust's enabled incoming latency setting, unlike the inherited native
  listener default. Explicit false is supported on incoming sockets; outgoing
  no-delay follows native endpoint defaults as detailed below.

Regression workloads use only synthetic RAM chains, loopback sockets and small
temporary config files: native default/override layering and units; concurrent
atomic admission; shared incoming/outgoing/pending accounting; 35 incoming peers
with a 40-connection setting; blocked names/IPs; idle authentication expiry;
compression level 20 interoperability; a silent body returning ownership at its
one-second configured lease instead of the eight-second full timeout; 1/1 and
5/12 observer/target settings; content deduplication; fixed-peer extra slots;
underfilled retries; and completion delivery inside a throttled connect period.
An actual-daemon test loads the C++ file with local overrides, starts the
configured listener without a CLI listener override, and syncs from one fixed
peer without seeds. Existing wallet, farmer, harvester and timelord daemon tests
also exercise the shared config/startup paths.
Existing competing-branch, backpressure, relay-priority and stale-owner tests
remain part of validation. These establish behavior and resource bounds, not
C++ performance parity. No live ledger/plot reads, migrations, cache warming or
production-service interruption were used. Comparative saturated-connection,
slow-body and mainnet sync measurements remain outstanding.

Validation: **152 node tests and 10 real-daemon integration tests passed**.
Node/tools all-target Clippy passes with warnings denied; formatting and diff
whitespace checks pass. Node and utility binaries build with two jobs,
incremental/debug output disabled and low CPU/I/O priority.

### Incoming socket settings and connection lifetime

Additional reference: `vnx-base/src/Endpoint.cpp::{set_socket_options,
set_tcp_socket_options,TcpEndpoint::open,bind,listen,accept,set_options}`,
`vnx-base/interface/{Endpoint,TcpEndpoint}.vni`,
`vnx-addons/src/TcpServer.cpp::{main,connect_client}`, and
`Router.cpp::{connect_to,update}` at the same native revision.

`listen_queue_size` now reaches `listen()` when creating the socket, preserving
the native backlog default of 100. `tcp_keepalive`, `send_buffer_size`, and
`receive_buffer_size` configure both the listener before bind/listen and every
accepted socket before authentication. Native zero/negative buffer sizes leave
OS defaults; the OS can clamp effective sizes and backlog. Socket setup uses
the cross-platform `socket2` API, retaining IPv4/IPv6 address iteration and
nonblocking accepts. Failed setup drops its socket instead of publishing a
partially configured listener. Existing decode, admission and worker bounds are
unchanged.

Direction matters: native Router constructs a separate outgoing `TcpEndpoint`,
whose defaults enable no-delay and keepalive with OS buffer defaults. Its
inherited Router socket settings configure the listening endpoint and accepted
sockets. Rust now follows that distinction, correcting the first checkpoint's
application of `Router.tcp_no_delay` to outgoing sockets too. The retained Rust
incoming no-delay fallback remains enabled when the key is absent; explicit
false works. Both directions now enable native TCP keepalive by default.

`disconnect_interval` now limits incoming connection age in seconds, independent
of receive activity. Zero disables it; outgoing connections are exempt, matching
`Router::update`. A bounded deadline check in the existing receive/RPC loops
also covers authentication and partial frames, without a timer thread per peer.
Dropping the connection releases its shared admission lease. Native checks on
its update timer after integer-second rounding; Rust enforces an absolute
receive deadline and can expire up to a timer tick/rounding interval sooner.
The send-pressure checkpoint below removes blocked socket writes from the
receiver's mutex path and independently caps the writer by inbound age.
Synchronous ledger/encoding work can still delay receive-owner cleanup; its
latency under load remains a reliability/performance concern.

New regressions inspect actual listener/accepted/outgoing socket options and
OS buffer sizes, native buffer-disable/seconds semantics, and a busy
authenticated incoming peer expiring while its outgoing sibling remains usable.
A partial frame before authentication also expires with receive-idle expiry
disabled. A replacement peer confirms admission capacity is released. The existing
network and actual-daemon tests cover the changed listener construction.
These are bounded loopback checks, not connection-load or latency parity
measurements. No live ledger/plot data or unrelated service was used.

Follow-up validation: **155 node tests and 10 daemon integration tests passed**.
Node/tools all-target Clippy, formatting and diff whitespace checks pass. Builds
retain the two-job, nonincremental, no-debug, low CPU/I/O priority policy.

### Outcome priorities

The migration target is preserved intent, functionality, performance,
reliability and robustness. Config coverage above is an interoperability
inventory, not a checklist of mandatory Rust features. The material remaining
Router concerns are:

1. Independent body/poll completion is implemented and covered by the checkpoint
   below. Continue representative saturated-network comparisons of completion
   latency, retries and duplicate downloads while retaining the same bounds.
2. The send-pressure checkpoint below covers bounded queues, control/consensus
   priority, slow-reader recovery, cancellation and expiry with saturated
   sockets. Sustained constrained-network, large-record and fanout comparisons
   remain. An event loop or native queue implementation is not prescribed.
3. The peer recovery checkpoint below covers periodic discovery, bounded
   full-pool probes and cached-only restart. Wider churn, eclipse resistance and
   constrained-network comparisons remain; exact native timers, rotation
   policies and storage formats are optional.
4. Connection-role and sync policies must retain availability in the supported
   deployment scenarios, including NAT. Historical sync is outbound-only by
   security policy. Strict successful-observer minimums, fixed-peer
   qualification and absolute deadlines need explicit availability/robustness
   evidence. They are not automatically better or worse solely because they
   differ from C++.
5. Automatic port mapping remains unavailable where deployments require it.
   Manual reachability and outbound bidirectional communication do not implement
   that capability; whether to add UPnP is a feature-scope decision.

Equivalent diagnostics, maintenance and operational controls remain required.
Exact native logging intervals, queue parameter names, internal topic/server
wiring and arbitrary version overrides need not be reproduced. Alternate node
roles are functional scope when supported, rather than a reason to expose a
`mode` setting without implementing the role. Existing fallback/default choices
remain documented tradeoffs, subject to workload and failure-case validation.

### Independent peer completion checkpoint (2026-10-26)

Reference: C++ `141054c398d2bab6802174379ce3b191d4f3a499`,
`Router.cpp::{process,on_return,update,main}`,
`vnx-addons/src/{TcpServer,MsgServer}.cpp` receive/send/shutdown paths.
Native processes individual returns, tracks content leases independently and
does not join all providers before progressing another peer's work.

The Rust body scheduler now moves one connection owner into a background job
and receives correlated individual body completions through a bounded queue.
Valid results are checked against the requested chain/content hashes and height
claims, then statically prevalidated before entering the existing bounded
completion map. A stalled or failed tail cannot discard earlier successes.
The shared limit is 32 unanswered body requests across all peers, including
late attempts from older windows; a peer owns at most one body, metadata or poll
operation. Normal common-content pipelines retain 32-request batching. Healthy
providers can refill request capacity while another provider remains blocked.

Leases still prevent premature duplicate requests. Once a lease expires, other
eligible providers can serve the missing content without joining the late
owner. The late owner remains in its bounded peer slot, is disqualified for
fresh observations, and is joined at completion or pool shutdown. Authenticated
replacement waits for that owner, rejects its later payloads and requires fresh
status qualification. Prior-window pending leases carry into new windows;
late bodies stay within the existing recent-height retention range. Closing
the reply channels before joining releases workers blocked on delivery.
Cancellation first consumes already-delivered completions, retains the ready
bodies in existing RAM staging, clears pending heights and keeps worker ownership.

Idle peer polls now use the same owned completion mechanism as metadata instead
of joining a scope containing every peer. A partial frame on one peer cannot
hold up connection maintenance or healthy peers' hash/body jobs. Sender teardown
also sets its stop flag under the notification mutex and wakes its condition
variable. This handles both an already-idle sender and the race before it
enters the wait, including when `send_interval_ms` is one minute. Socket shutdown
continues to interrupt blocked writes before the sender is joined.

The all-observed-content policy and consensus validation are unchanged. A window
still waits until its candidates are resolved or their leases expire; this is
not early publication that drops a slow competing branch. Contextual checks,
CPU proof verification, ledger execution and durable publication still follow
the existing selected-chain path. The byte target now stops at a completed
height instead of overshooting to a whole provider group. There is no new broad
state/payload cache or live-storage scan. Absolute body deadlines and full leases
after failed/empty replies remain deliberate differences from native.

A controlled RAM workload compiled the previous checked-in scheduler from
`3e2290a` alongside the new scheduler using the same synthetic providers and
block fixtures.
It requested 65 common statically valid blocks and one competing block at height
one from three synthetic providers. Metadata delays were 0/40/60 ms; normal
body batches took an artificial 40 ms. The slow-provider case delayed the
competing provider's body by 350 ms. Each version used fresh RAM nodes, fetched
66 bodies once, and recorded zero retries. Three samples per case alternated
previous/current order, with identical debug-profile/resource settings:

| Workload / median | Previous scheduler | Independent completions |
| --- | ---: | ---: |
| Healthy providers, all common bodies downloaded | 214.858 ms | 212.358 ms |
| Healthy providers, full window | 240.571 ms | 235.872 ms |
| Slow competing provider, all common bodies downloaded | 520.607 ms | 210.427 ms |
| Slow competing provider, full window | 543.015 ms | 442.597 ms |

These measurements isolate scheduling with artificial delays; they do not prove
C++, WAN or mainnet throughput parity. The temporary baseline copy/harness was
removed after comparison. Permanent gated regressions require all 65 healthy
bodies to download before the competing provider is released and check the
aggregate request bound. Other regressions cover a partial batch whose tail
fails, lease-only reassignment, late-owner replacement, cancellation/cleanup,
a blocked poll with three healthy observers, and sender teardown with a long
idle interval. A real socket test delivers a reordered successful reply before
another request times out. Existing native backpressure/reordering, competing
branches, forged commitments and archived-content reuse tests remain applicable.

Validation: **161 node tests and 10 actual-daemon integration tests passed**.
Node/tools all-target Clippy passes with warnings denied; formatting and diff
whitespace checks pass. Node/utility builds and comparisons used two build jobs,
incremental/debug output disabled and low CPU/I/O priority. No live ledger/plot
storage or unrelated process was used.

### Peer discovery and restart recovery checkpoint (2026-10-26)

Reference: C++ `141054c398d2bab6802174379ce3b191d4f3a499`,
`Router.cpp::{main,connect,discover,on_connect,on_return,get_peers,save_data}`
and its valid/public-address filters. Native combines seeds, known peers and
retry candidates, refreshes discovery asynchronously, tests extra candidates
while full, and saves known peers every five minutes and at shutdown.

Managed Rust connections now issue correlated discovery requests during normal
poll/RPC work, initially for outbound sync peers and periodically for full nodes
in both directions. There is no extra discovery receive worker, no synchronous
`get_peers` wait before admitting a qualified sync peer, and at most one pending
request per connection. Optional replies have a deadline bounded by the fetch
timeout and discovery period. Stale/unsolicited, malformed-content and exception
replies do not populate the directory or fail unrelated RPCs. Ordinary RPC
correlation, backpressure windows and body deadlines remain independent.

The shared directory separates up to `max_peer_set` confirmed endpoints from
the same number of untrusted hints. Hints cannot evict confirmed peers or enter
the persistent cache. IP filtering rejects local/private and other conservative
nonpublic ranges, including IPv4-mapped aliases; hostname lookups and incoming
ephemeral source ports are not learned from gossip. Confirmed endpoints require
an authenticated outgoing connection on our genesis, a full-node role and a
synced-height response. Only confirmed public endpoints at the network's default
peer port are advertised. Explicit private/custom-port connections can be cached
for their configured use; IPv6 scope IDs survive the roundtrip. Confirmation is
reachability/identity evidence, never permission to bypass block consensus.

Candidate snapshots refresh only when the directory changes, avoiding address
list copies on ordinary body windows. Removed candidates prune retry bookkeeping
while retaining pending connection owners. A full pool may validate one extra
candidate per discovery period, still respecting the connect cycle and ordinary
per-address retry deadlines. Underfilled pools retain native-style immediate
retries on a connect cycle. Connection budgeting includes retained old owners,
including fixed peers. A higher advertised-height candidate can replace only
an idle lower-height ordinary peer; fixed peers, busy owners and equal-height
connections are retained. Probes that cannot be installed still add confirmed
fallback endpoints. Optional probe thread-creation failure leaves the existing
pool running. This is a bounded exploration policy, not native's random
overconnection and trimming algorithm. Historical sync remains outbound-only
by security policy. Fork-check discovery and broader malicious-peer selection
policies remain separate work.

The executable loads one bounded, versioned JSON cache, namespaced by genesis
and network peer port. `Router.storage_path` selects its directory (relative
CLI paths use `MMX_NETWORK`); otherwise it lives in the Rust ledger directory.
Startup can proceed from cached peers without seeds/fixed peers. The loader
checks regular-file type, 64 KiB byte limit, version/network identity and at most
1,024 entries, reapplies IP blocks and current capacity, and salvages valid
numeric endpoints. Invalid/missing data never stops seeded startup. Existing
C++ cache files are not read or rewritten.

One owned writer snapshots confirmed addresses, releases the directory mutex,
then serializes/writes/syncs a temporary file and atomically replaces the cache
with a Unix directory sync. Dirty changes coalesce to the latest generation,
with an immediate first dirty snapshot, at most one write per five minutes
afterward, and a final dirty flush on graceful shutdown. There is no accumulating
snapshot queue. Slow/failing writes do not hold directory/ledger locks or run on
the coordinator/transport threads; replacement failures preserve the prior
complete file, and write/sync failures are reported and retried on the bounded
cadence. Shutdown joins the writer and can wait for
its filesystem operation, as native shutdown can; this advisory-cache durability
does not replace ledger durability. No ledger/plot scanning or broad state
caching is introduced.

Regressions cover independent hint/confirmed bounds, public filtering, mapped-IP
blocks, confirmed-only advertisement, numeric ports/scopes, corrupt/oversized/
nonregular cache handling, foreign-network rejection, atomic replacement failure,
blocked-writer isolation and final-generation coalescing. Real TCP tests reorder
discovery and ordinary replies, leave discovery unanswered, expire/retry it and
inject stale/unsolicited/negative replies without disrupting ordinary RPCs. A
gated scheduler test bounds full-pool probes to one, and an owned metadata job
proves ahead-peer replacement cannot evict busy/fixed peers or churn an equal
height. The real-daemon suite now shuts down a fixed-only node, removes all seed
and fixed-peer config, restarts the same temporary ledger, and reaches sync
using only the saved cache. These checks establish bounded behavior and recovery,
not C++ throughput or long-running network parity.

Validation: **168 node tests and 10 real-daemon integration tests passed**.
Node/tools all-target Clippy passes with warnings denied, and formatting/diff
whitespace checks pass. Builds use two jobs, incremental/debug output disabled
and low CPU/I/O priority. Tests use RAM chains, loopback sockets and small
temporary config/cache files; no live ledger/plots or unrelated processes were
used. The blocked-writer test establishes isolation/coalescing, not storage or
network performance parity under sustained load.

## Peer send pressure and outbound sync policy (2026-10-26)

Reference inspected at C++ `141054c398d2bab6802174379ce3b191d4f3a499`:
`Router::{send_to,on_request,on_pause,on_resume,send,recv_notify}`,
`vnx-addons/src/MsgServer.cpp::{send_to,on_write}` and
`TcpServer::{send_to,on_write,on_disconnect}`. Native uses nonblocking socket
writes and bounded compressed buffers, rejects blocked-peer RPC with
`vnx.OverflowException` before invoking Node, and reserves priority for consensus
while dropping ordinary unreliable traffic under pressure. Schema declarations
span frames, so an already encoded/partially written frame cannot be reordered
or abandoned without closing the connection.

Rust now starts one owned writer before authentication and attaches the existing
gossip sender after identity/network checks. It replaces the former sender
thread rather than adding another sender per authenticated socket. All managed
socket writes run outside the encoder/queue mutex, allowing the receive owner
to continue reading receipts, gossip and correlated replies or cheaply enqueue
control work while a write is blocked. Unmanaged low-level `Connection` helpers
retain synchronous sends. Enqueue/encoding failures are transactional: no batch
prefix is published and no schema state advances. Accepted frames retain FIFO
ownership until sent or connection cancellation/failure; write errors close the
socket and discard the remaining queue rather than reusing advanced schemas.

`Router.max_write_queue` now controls the compressed byte admission threshold
(10 MiB default, positive values only). Admission below that threshold can cross
it by one batch bounded to 64 MiB + 8 bytes, like native's pre-encoding threshold
but with a bounded Rust RPC batch rather than just one message. At most 256
ordinary messages are charged, including the in-flight batch. Compressed data
uses exact-sized boxed buffers; queue metadata and temporary serialization/
compression allocations are additional bounded costs. Unlike native's decrement
on each partial write, Rust conservatively keeps the whole batch charged until
completion. This prevents moving the batch to the writer from opening an
unaccounted second queue. Existing raw/decoded frame bounds remain unchanged.

Socket partial writes/timeouts or a full ordinary queue trigger pre-dispatch
overflow replies. The separate rejection reserve is limited to 32 messages and
16 KiB; exhausting it closes the managed connection. Successful responses are
retained, and the existing client retries only explicitly rejected requests.
Other queue-admission errors propagate to the connection owner, with no silent
reliable-message loss. Unlike native's receive pause, Rust keeps decoding until
the bounded reserve is exhausted, allowing receipts and replies to progress.
An overload reply cannot bypass bytes of a previously started frame.

The writer drains queued control work before selecting another gossip body.
Gossip payloads stay shared in the existing bounded relay store, with only one
body selected per writer pass. Both receipt classes precede body selection, so
live consensus cannot starve transaction receipts. Every pass revisits live
consensus before VDF bootstrap or transactions; receipt/cache suppression,
randomized fanout timing, upload credit/refusal rules and reliable local
publication behavior remain.
No transaction backlog is pre-encoded ahead of new consensus. Thread wake tokens
retain enqueue/publication/stop notifications across idle transitions; ordinary
RPC enqueue wakes only its own writer rather than all peer workers. Idle waits
still use `send_interval_ms`. Native `priority_queue_size` and `max_queue_ms`
are not mapped to these different queue/scheduling mechanisms.

Every queued batch has a ten-second absolute deadline from admission, capped by
inbound maximum age; partial write progress does not renew it. The writer uses
short socket waits, preserves the same timeout setting across ordinary writes,
and interrupts both directions on expiry/failure/unwind. Drop signals the owned
writer, wakes it and shuts down the socket before joining, including a stalled
write or a long idle interval. Inbound age is enforced independently of receiver
progress. Synchronous response execution/encoding can still delay receive-owner
cleanup, even though socket writes no longer hold its mutex.

Historical synchronization intentionally remains outbound-only, per the
requested security policy. Incoming peers exchange gossip and restricted peer
RPC but cannot qualify as historical observers/download sources, even after
successful authentication. Outbound identity/genesis/observer qualification and
full consensus validation remain mandatory. This is an intentional boundary,
not pending inbound historical-sync functionality. Availability with restricted
outbound access or particular NAT deployments still needs explicit evidence.

New real-socket regressions saturate output with a 1 MiB incompressible record,
4 KiB send buffer and 32 KiB receive buffer. They verify a held ledger mutex is
never reached by rejected RPC, receiver completion within 750 ms while the
socket stays blocked, another authenticated peer's RPC progress, schema-correct
recovery, transactional failed batches, byte/count/rejection bounds, cancellation
within one second, and inbound expiry without a receive owner. A trickle-read
case verifies a 200 ms test batch deadline despite partial write progress. A
relay scheduler regression publishes new consensus during a TX send and checks
it precedes the next TX without replaying completed work or delaying TX receipts.
Authenticated incoming RPC remains usable while historical qualification is
rejected.

These are controlled correctness/isolation workloads, not a C++ throughput or
network performance-parity claim. Sustained saturated-network latency, loss,
churn, fanout CPU, large-frame encoding/compression time and peak resident memory
still need representative comparisons. No live ledger/plot scans or unrelated
process changes were used.

Validation: **175 node tests (121 unit + 54 integration) and 10 real-daemon
integration tests passed**. Node/tools all-target Clippy passes with warnings
denied; formatting and diff whitespace checks pass. Builds/tests use two jobs,
incremental/debug symbols disabled and low CPU/I/O priority.

## Admission counter compiler compatibility (2026-10-27)

Rust 1.99.1 deprecates `AtomicUsize::fetch_update`, failing the warning-denying
workspace CI job. Its renamed replacement is newer than the Rust 1.89 minimum.
Connection admission now uses the same checked compare-and-exchange loop and
Acquire/AcqRel ordering directly, preserving lock-free bounded admission, cheap
rejection, outgoing exemption and permit release. The reference remains
`vnx-addons/src/TcpServer.cpp::{do_poll,connect_client}`; no scheduling, socket
or consensus policy changes are intended. Existing concurrent admission tests
and a new zero-limit/overflow regression cover the boundary behavior. This
compatibility fix makes no new performance-parity claim.

The workspace all-target Clippy failure was reproduced and cleared with Rust
1.99.1 and warnings denied. The exact counter source and its three admission
regressions also compile and pass independently with Rust 1.89.0.
