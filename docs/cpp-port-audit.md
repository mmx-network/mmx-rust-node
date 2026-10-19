# C++ port audit, 2026-10-19

Reference: `mmx-node` at `5626ac2b7cd26aa9626ea3b2cf864b4cfe6fbcd2`;
VNX submodule at `35be1ac0b4966e9e4686c87961e683a408b9ec2c`.
This is a subsystem comparison of the implemented Rust runtime against the C++
execution paths. Native fixtures establish the cases they exercise; they do not
establish complete consensus, security or performance equivalence. Unimplemented
C++ services and RPCs remain outside the Rust compatibility surface. Virtual
plots are obsolete and are deliberately excluded from migration work.

## Changes from this pass

The production checkpoint (`6f8ba88`) ports verified proof relay, full-hash best
proof selection, peak replacement challenge selection, refresh before planning
and after asynchronous signing, native peak/orphan voting scope, and separate
in-flight versus successfully signed proof tracking. A real stronger proof from
C++ tests both queued-announcement and in-flight-signature vetoes. Public proof
relay removes unhashed local harvester/address metadata.

The follow-up checkpoint adds:

- Advancing, verified VDF relay before block production; applied foreign live
  blocks and their linked VDFs relay after successful ledger validation.
  Historical catchup stays silent. Local production uses this same publication
  path and reports success only when selected.
- Accepted foreign vote relay, a 32,768-ID authenticated replay cache, and a
  4,096-item pending-vote limit. Runtime votes are processed together after
  block-proof verification. C++ also checks votes immediately in its receive
  handler; Rust's batching adds at most a ledger tick for a ready vote while
  avoiding a full pending scan on every arrival. Validator membership, best
  proof eligibility, already-counted votes and strict three-interval expiry
  retain native rules. The direct chain admission API retains fixture behavior.
- Pending VDF receipt times, native ten-interval expiry, rejection below the
  root's iteration boundary, and a 32 MiB decoded/wire queue budget. Duplicates
  do not refresh arrival time. Verified work survives the waiting timeout until
  root advancement, matching C++'s separate verified VDF index. The existing
  worker retains at most two CPU jobs. Queue budgets are not total process
  memory budgets: evidence and a bounded number of worker copies also exist.
- Sender echo suppression for VDFs, and the native configured 180-second
  receive-idle timeout for inbound peer connections.
- Reuse of block bodies already loaded for fork verification during replay,
  avoiding the second archive read for those bodies. Relay uses the execution
  handoff instead of reading a third time. Already-relayed VDFs are checked by
  ID before rebuilding their record.
- Public RPC budgets: at most 128 transaction IDs and 32 MiB of decoded returned
  transactions; at most 10,000 owner/currency point reads per balance query.
  These are service admission limits, not block consensus limits.

Socket regressions cover unverified blocks, immediate VDF delivery, accepted and
unauthorized votes, duplicate pending votes, sender echoes, and silent historical
sync. Additional tests cover a full 4,096-vote queue, exact timeout boundaries,
replay-cache poisoning attempts, decoded VDF bytes, waiting versus verified VDF
retention, and multiplicative RPC work. Storage tests use temporary databases;
no live-ledger or plot scan is part of validation.

## Subsystem comparison

| Area and C++ paths inspected | Rust path and evidence | Remaining difference |
| --- | --- | --- |
| Block production: `Node_update.cpp::update`, `make_block`, `Node.cpp::fork_to`, voting, `Node_verify.cpp::add_proof`, `find_best_proof` | `mmx-node/src/production.rs`, `node.rs`; native chain, production and vote witnesses; asynchronous stale-result tests | Independent sending and consensus priority are implemented; native early relay credits and authenticated role handling below remain unported. Signed block tracking is transient as in C++. |
| Header/space consensus: `Node_verify.cpp::verify_proof*`, `ProofOfSpace*`, `pos/verify*.cpp`, native header/hash implementations | `mmx-consensus/src/{proof,transitions,header,pos}.rs`; native proof/hash/difficulty/weight/challenge traces, proof ordering and malformed witnesses | No additional acceptance mismatch identified in this pass. Continue differential mutation coverage at fork boundaries; this is not an exhaustive proof. |
| VDF consensus and scheduling: `Node_verify.cpp::verify_vdf*`, `Node_update.cpp::verify_vdfs` | `mmx-consensus/src/vdf.rs`, `mmx-node/src/vdf_worker.rs`; opaque work evidence rechecked against current history, native infusion/segment/continuation fixtures, two-job worker bound | C++ trust ranking of competing timelords and specialized CPU multi-lane SHA remain missing. |
| Fork choice/sync: `Node.cpp::find_best_fork`, `fork_to`, `Node_update.cpp::on_sync_done`, `Router.cpp::process` | `mmx-execution/src/chain/{choice,forks,sync,voting}.rs`, `mmx-node/src/sync/peers*`; native fork/vote/root witnesses, three-observer socket tests, competing contents, timeout reassignment and deep reorg tests | Requested 3–8 observer policy is deliberately stricter than native completion. Every observed content is fetched once; no majority filter. Native transmit credits remain separate from body leases. |
| Body/economics: `Node_validate.cpp::validate`, `execute`, native reward and fee helpers | `mmx-execution/src/{block,transaction}`, `mmx-consensus/src/authorization.rs`; native block/transaction/economics/NFT/multisig traces and rejection ordering | Canonical-parent validation hands off one verified execution delta; independent transactions still execute serially. Dependency locks and predecessor plans exist, but are not a concurrent executor. |
| Contract execution: `vm/Engine.cpp`, `vm/instr_t.cpp`, `vm_interface.cpp`, `vm/StorageCache.cpp`, `Node_validate.cpp::prepare_context` | `mmx-vm/src/engine*`, `mmx-execution/src/lib.rs`; all declared opcodes, gas, composite ownership, remote calls, hardfork setup, locks, failed-engine publication, recursion/copy/erase and input limits have fixtures | JavaScript compiler and remaining original contract suites are unported. Per-engine/per-transaction storage overlays are bounded by execution and are distinct from a broad node state cache. |
| Native DB: `DataBase.cpp::Table::{find,commit,revert,flush}`, storage versioning | `mmx-db/src/{native,journal,sorted}.rs`; append-only values, reference-only sorted compaction, positional reads, bounded reader handles, pinned MVCC snapshots, atomic suffix publication; crash/truncation/checksum/reopen tests | Block/transaction payload consolidation is complete for new writes; synchronous durability comparisons below remain. Public balance ranges use a streaming bounded merge. No SQLite backend. |
| Rollback/purge: C++ `Node` storage/apply/revert and versioned tables | `mmx-execution/src/chain/{mod,forks}.rs`, `mmx-db/src/native.rs`; every retained-height rollback, deep fork/restart, crash-safe collection, pinned snapshot and RPC floor tests | Purge necessarily rewrites retained live values during explicit collection. There is no automatic background migration or ledger scan. Rollback below the durable floor is refused; private consensus ancestry does not reappear through RPC. |
| Peer and public RPC: `Router.cpp::{on_*,send*,receive_msg_hash,relay_msg_hash}`, `Node_api.cpp`, `WebAPI.cpp` | `mmx-node/src/{peer,relay,inbox,rpc,http,transactions}.rs`, VNX safe readers; native request allowlist, 32 peer workers, absolute frame deadlines, compressed/decoded/schema limits, separate transaction inbox, HTTP framing/deadline tests | Native credits, rate policy, peer-role filtering and full inbound authenticated lifecycle remain incomplete. Balance scans and public validation run on pinned snapshots outside the ledger lock with two shared RPC slots; per-peer credit/rate fairness remains. REST/WebAPI parity is incomplete. |
| Farmer/wallet: `Farmer.cpp::{sign_block,sign_vote,handle}`, `Wallet.cpp`, `include/mmx/ECDSA_Wallet.h`, key derivation and spending | `mmx-farming/src/{farmer,wallet,spending,service}.rs`, `mmx-wallet`, `mmxwallet`; native keys/KDF/signatures, protected-key unlock verification, pending/reservation/fork/ambiguous-submit tests, real daemon RPC tests | Pool partial submission, reverse farmer RPC, complete wallet/history/offer/swap API and wallet query caches remain missing. Farm reports use bounded retention. |
| Harvester/plots: `Harvester.cpp::{check_queue,lookup_task,reload}`, `pos/Prover.cpp`, `pos/verify_full.cpp` | `mmx-farming/src/{harvester,plot,encoding}.rs`, `mmx-tools/src/lib.rs`; bounded shared Rayon pool, immediate proof emission, native deadline, filter-before-full-proof, density-based park search, retained headers, positional park reads/reuse; stalled-plot test and live sample | Owned per-plot jobs allow overlapping challenges and reports; metadata-free/corrupt-quality paths still need additional slow-resource comparisons. Remote recompute remains missing. |
| GPU and CLI/config: native OpenCL VDF code, CUDA recompute, `mmx.cpp`, native module config | `mmx-opencl`, consensus accelerator, `mmx-tools`, `mmxwallet`; GPU-only default discovery, bounded buffers, native CPU result checks/fallback, native config layering and CLI precedence tests, wallet CLI suites | Rust OpenCL F1 replaces CUDA for the implemented path. Static musl cannot load an ICD and falls back to CPU. Full C++ utility/compiler/timelord/service and configuration parity is not claimed. |

## Remaining work, in priority order

1. **Block payload consolidation and canonical-parent execution handoff completed.**
   New block headers and transaction records are written once in the fork
   archive. Selected-chain block entries are eight-byte references and
   transaction entries are 44-byte block/position locators. Independent header
   and transaction lookups use positional reads without loading the whole block.
   Detached chain snapshots pin the corresponding immutable archive view.
   Standalone `ChainState` uses the same split layout in its own database.
   Older native full-body records remain readable; no startup scan, conversion
   or rewrite occurs. Purge collects the new component tables using the same
   retained-block policy. Immutable body storage is separate from required
   contract/ledger state, which must survive deployment-history purging.
   Tests cover legacy/new mixed reads, reopen, every-height rollback, torn
   publication, purge with pinned snapshots and exact payload/index contents.
   A small two-transaction write fixture wrote 132,638 journal bytes versus
   395,819 with the previous duplicated layout (66.5% fewer); this is a write
   size comparison, not a historical-sync throughput claim.
   Canonical-parent live blocks and signed local candidates now pass an opaque,
   fully validated execution handoff into fork choice. Only one pending handoff
   is retained. Native database owner/generation checks run both at admission
   and publication; rewind, purge or another selected publication invalidates
   it. This also rejects a stale result when the parent hash/version returned
   to the same values after rollback. The selected branch adopts its validated
   delta without executing the body again. Remote blocks whose parent is on
   an unselected branch still follow full branch replay; additional contexts
   would require an explicit memory budget. Native witnesses, stale/foreign
   snapshot tests and the existing production attack tests cover this path.
   No broad ledger state cache is introduced.
2. **Relay fairness and credits (public-peer defense and latency).**
   Independent consensus sending and transaction/consensus scheduling are now
   implemented. Each owned connection has one sender, sharing an ordered encoder
   with replies and independent of the receive decoder. Publication wakes it;
   the native 20 ms interval bounds remaining batching waits. A trickled inbound
   frame or withheld RPC reply cannot delay outgoing completed work. Consensus
   and transactions have independent bounded cursors, so a transaction backlog
   cannot hide proofs/VDFs/blocks. Records stay shared; no additional payload
   queue is introduced. Sent/received echo suppression has the native 20,000-ID
   bound and no cache lock is held while a socket writes. C++ uses its network
   event loop; Rust currently uses one sender thread per already-bounded
   connection. Socket tests cover both delayed-reply and partial-frame cases.
   Native `ReceiveNote` feedback is now consumed as transport control, including
   while waiting for RPC responses. Verified publications send acknowledgements
   before payload forwarding; a peer's receipt suppresses only its own copy.
   The 20,000-ID per-peer bound remains. A separate 100,000-ID cache of verified
   publications filters already-processed gossip before inbound queue admission.
   Receipt claims never seed this cache; payload eviction retains known IDs,
   duplicates do not refresh FIFO age, and explicit rollback resets it so work
   can be accepted again. Native first-seen envelope deduplication before full
   verification remains a distinct follow-up.
   Randomized staged fanout is now implemented: each publication shuffles live
   connection tokens once and assigns native `target / (N+1)` ranks. The native
   `Router.relay_target_ms` config is honored (repository default 2,000 ms).
   Receipt notes have independent cursors and go out immediately; bodies use
   shared records and timing metadata. A ready later consensus item can pass a
   delayed earlier item. Bounded completed-sequence sets prevent retransmission
   even if receipt traffic evicts the general per-peer hash cache. No additional
   per-peer payload queue is introduced. New connections retain the previous
   immediate replay behavior; native VDF-only connection replay remains open.
   Still port transaction cost/upload accounting, earned farmer/timelord
   early-relay credits, peer-role filtering and authenticated incoming lifecycle. Current bounds and verified-only relay are not equivalent to
   native per-peer fairness. Preserve transport deduplication separately from
   consensus acceptance; a claimed identity/height never earns relay credit.
3. **Streaming balance RPC reads and snapshot validation completed.**
   Following C++ `Node_api.cpp` balance queries and `table.h::find_range`, the
   native store now merges one visible key per input and resolves only winning
   values. A satisfied row limit stops before advancing to the next row.
   Tombstones, index probes and versions hidden by historical reads or rollback
   consume work; metadata and conservative read-ahead consume bytes before
   reads/allocation. An RPC shares 100,000 index/key operations and 32 MiB across
   all owners/currencies, with at most 10,000 scanned rows. Exhaustion rejects
   rather than returning a partial total. Zero balances count toward native
   per-owner limits; whitelist queries retain native limit-independent point
   reads and the range uses native's exclusive all-ones currency sentinel.
   HTTP and VNX balance queries/public transaction validation capture a pinned
   native database snapshot under the node lock, then run and encode on their
   request worker. Two slots are shared across both protocols; saturation fails
   promptly while cheap status/height calls remain available. VM validation
   retains the native transaction gas/cost limits and cannot publish effects.
   Tests cover mixed history, pending/tombstones, rollback, nested snapshots,
   byte exhaustion before payload/index reads, corrupt masked old values, zero
   limits/balances, all-ones addresses, whole-request budgets and lock release.
   These are service bounds, not consensus limits. Other bounded block and
   transaction RPCs still use the ledger owner; there is no broad state cache.
4. **Overlapping harvester scheduling completed.**
   Following C++ `Harvester::check_queue` / `lookup_task`, the daemon captures
   eligible immutable prover headers and releases the service mutex before
   dispatching plot I/O. Its 20 ms tick continues receiving challenges and
   reporting farm information. Completed proofs publish immediately; each completed worker refills available
   capacity directly without waiting for the service tick. One global
   Rayon pool, including nested F1 computation, remains limited by `--threads`.
   At most that many per-plot leases run, and at most eight contexts retain
   queued eligible plots; raw challenge admission retains its separate bound.
   Newest queued heights receive free workers first. A busy plot waits in the
   queue instead of starting another overlapping read of the same plot. Original
   deadlines are never refreshed; expiry or queue eviction drops pending work,
   while an old blocked OS read can retain only its one worker/context. A queued
   batch does not occupy a worker waiting for all its plots to finish.
   A real FIFO-backed stalled plot regression delivers a valid second challenge
   proof and reads farm information before releasing the first blocked read.
   Deterministic lease tests cover newest-first scheduling, duplicate-read
   suppression, worker/context bounds, expiry and one completion per batch.
   Unlike C++, Rust explicitly caps queued owned contexts and serializes reads
   per plot. Normal filesystem reload itself remains synchronous, as in C++;
   a lookup cannot hold the service lock during it. Native plot witnesses still
   pass. Comparative slow-drive throughput and remote recompute remain open.
5. **Concurrent transaction execution (busy-block throughput).**
   Port C++ dependency-aware worker execution using the already-produced lock
   plan; retain deterministic reservation, fee order, errors and overlay merge
   order. Benchmark independent contracts and a single contended contract.
   Parallel proof checking does not establish transaction execution parity.
6. **CPU VDF batching and competing-work priority.**
   C++ uses SHA-NI/ARM two-lane recursion and AVX2 eight-lane SHA plus timelord
   trust ordering. Rust's SHA dispatch and Rayon segments are not that execution
   strategy. Benchmark CPU-only against C++ and port useful lanes/priority with
   identical infusion and work verification. GPU default performance does not
   establish CPU fallback performance.
7. **Wallet request batching and operational feature completion.**
   `Wallet::update_cache` caches balances/owned contracts/liquidity/history with
   a TTL. Rust reads balances and each pending transaction afresh and serializes
   account operations under a mutex. Prefer batched pinned queries first; keep
   node ledger state uncached per the deployment requirement. Pool/NFT partial
   submission, remote recompute, reverse farmer routing and the missing wallet,
   utility and REST methods need separate compatibility work. Obsolete virtual
   plots are not a gap to fill.
8. **Durability and retained-index workload comparisons.**
   C++ table commit flushes its append stream; Rust journal publication also
   synchronizes it before acknowledging the commit. Keep the stronger current
   crash contract unless deliberately changed. Measure append/fsync/compaction
   and restart costs on temporary representative data. Any batching must retain
   recoverable publication boundaries and avoid repeated payload writes.

## Measurement and validation boundaries

The earlier native storage/sync samples and parallel harvester measurements are
recorded in `performance-parity.md` and README. The first 22 t320 lookups with
C++ harvesting disabled averaged 2,214 ms (max 4,284 ms), compared with 9,292 ms
across the preceding 89 sequential lookups. The farm had 1,207 plots (431 K29,
776 K31), 16 Rust workers and approximately 146 TB effective space. These are
adjacent live samples with different challenges, not a controlled throughput
proof. No caches were flushed and no plot contents were scanned for benchmarks.

This pass makes no new whole-node throughput claim. Regression tests establish
bounds and behavior; local low-priority two-job validation and the deployment's
height/hash comparison check the checkpoint. Production performance comparisons
for the open items above remain required before claiming C++ performance parity.

Checkpoint validation: 304 workspace tests passed, one existing ignored test,
strict workspace Clippy, formatting and the local optimized node build passed.
The preceding production checkpoint also passed GitHub Actions run 39272095917.

## Archived production check

A bounded read of the two archived candidate heights while the Rust writer was
stopped, followed by ordinary RPC reads, found:

| Height | Rust score / proof count | Canonical score / proof count | Rust timestamp later by |
| --- | --- | --- | --- |
| 5,530,070 | 48 / 8 | 150 / 7 | 2,257 ms |
| 5,530,187 | 54 / 4 | 143 / 3 | 1,971 ms |

Each pair shares its parent, challenge, VDF height and total chain weight. The
Rust proofs have lower (better) hashes. C++ `Block::is_valid`, `Block::validate`
and the original `Node::verify_proof` methods accept both Rust block envelopes
and all twelve included space proofs. That check does not replay ledger/history
validation or recover network receipt/vote timing. A better primary score alone
does not replace native vote/height/hash fork choice, and counted votes are not
retroactively revoked. The old logs cannot establish the network cause of these
losses. Missing proof-announcement relay is confirmed in the old source and is
fixed; claiming it as the proven cause would go beyond the available evidence.

After deploying the relay/audit checkpoint, the Rust node was synced with eight
peers at height 5,530,444 and its canonical hash matched C++. A ten-second normal
live sample observed approximately 0.015 MiB/s physical reads and 0.010 MiB/s
physical writes. This is a quiet live sample, not a historical-sync benchmark.
The remote Rust harvester stayed running and the C++ harvester stayed disabled.

The execution-handoff checkpoint passed 308 workspace tests, one existing ignored
test, strict Clippy, formatting and the optimized local build. Signed candidate
validation remains off the ledger mutex; publication rechecks both the best
verified proof and the exact ledger base. The handoff is consumed once.

The independent relay scheduling checkpoint passes 310 workspace tests (one
existing ignored test). Peer frame limits, incremental public reads, schema
publication ordering, RPC/body deadlines and shared sync body leases remain
enforced. A failed partial write closes the socket before schema state advances.

The bounded RPC checkpoint passes 319 workspace/target tests (one existing
ignored test), strict Clippy, formatting and the optimized local node build.
Public query snapshots stay pinned across ledger advancement and release their
shared worker slot on success or error. The preceding three storage/execution/
relay checkpoints also passed GitHub Actions.

The harvester overlap checkpoint passes 323 workspace/target tests (one existing
ignored test), strict Clippy, formatting, the local static musl harvester release
build and both CLI suites (28 tests each). The RPC CI follow-up replaces a newly
deprecated atomic update helper with a minimum-version-compatible compare/
exchange loop and checks contention from sixteen threads.

Header/hash RPC follow-up: native `get_header`, `get_header_at` and
`get_block_hash_ex` now use the compact archive header directly. The hash-only
sync observation path no longer reads/reconstructs every transaction just to
return two hashes. A temporary corrupted-transaction regression proves component
isolation at the actual node API. Legacy bodies remain readable in place, and
purged headers remain hidden. This path also includes the portable RPC permit
CI fix. Targeted execution/node suites, strict Clippy, formatting and the local
optimized node build pass; no live ledger scan or rewrite was performed.

Harvester refill follow-up: a one-worker/two-native-proof regression completes
both queued jobs after a single service poll. Completion refills under a short
shared scheduler lock, with no I/O or delivery callback while holding it.
Newest-height, per-plot exclusion, expiry and context/worker bounds remain.
The initial overlap deployment's slower adjacent live sample is recorded in
`performance-parity.md`; a scheduling fix alone is not a performance claim.

Receipt/replay tests cover real interleaved receipt/RPC frames, suppression of
an acknowledged payload on the wire, isolation from another peer/global cache,
100,000-ID FIFO eviction, explicit rewind/reacceptance and a socket barrier that
checks verified proof replays never enter the ledger inbox. Notes carry no
consensus authority or earned farmer/timelord credit.

The receipt/replay checkpoint passes 329 workspace target tests (one existing
ignored test), strict Clippy, formatting, both CLI suites (28 tests each) and
the local optimized node build. The header and harvester-refill checkpoints
also passed GitHub Actions, including software OpenCL conformance and both CLI
suites.

Purged-peer follow-up: qualification can use the immutable genesis identity when
height-zero history is unavailable. The peer RPC allowlist exports that cached
constant. Legacy C++ peers still use `get_block_hash(0)` without an extra call;
wrong-genesis/self/identity checks remain mandatory. A native DB/socket test
purges history, qualifies successfully and verifies old hashes/bodies stay
hidden before and after qualification.

Staged relay regressions check eight unique native delay ranks, connection-token
retirement, immediate receipts, cancellation of pending payloads, zero-delay
configuration, and ready work passing a deferred item without duplicate sends
when the peer hash cache is evicted. Existing real socket tests for stalled RPC
reads, partial frames, verified-only relay and stronger-proof production vetoes
pass with the staged sender. This is routing/scheduling parity work; it changes
no block acceptance or fork-choice rule.

Staged relay validation: 334 workspace target tests plus the added receipt-only
idle-activity regression pass (335 unique tests, one existing ignored test).
Strict Clippy, formatting and the local optimized node build pass. Control-only
receipt traffic refreshes the native receive-idle clock while absolute frame/RPC
deadlines remain unchanged. The preceding receipt and purged-peer checkpoints
also passed GitHub Actions.

VNX follow-up: the submodule advances to `35be1ac`, adding serialization and
bytecode allocation/recursion/schema fuzz campaigns and the nightly array-chunk
fix. The runtime crate tree is identical to `beaddc6`; this update adds test
coverage without changing node wire or execution behavior.
