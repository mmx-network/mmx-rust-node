# C++ port audit, 2026-10-19

Reference: `mmx-node` at `5626ac2b7cd26aa9626ea3b2cf864b4cfe6fbcd2`;
VNX submodule at `beaddc66d23f1f010661f96d109d3e60d3910d60`.
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
| Block production: `Node_update.cpp::update`, `make_block`, `Node.cpp::fork_to`, voting, `Node_verify.cpp::add_proof`, `find_best_proof` | `mmx-node/src/production.rs`, `node.rs`; native chain, production and vote witnesses; asynchronous stale-result tests | Native early relay credits and transmit scheduling below remain unported. Signed block tracking is transient as in C++. |
| Header/space consensus: `Node_verify.cpp::verify_proof*`, `ProofOfSpace*`, `pos/verify*.cpp`, native header/hash implementations | `mmx-consensus/src/{proof,transitions,header,pos}.rs`; native proof/hash/difficulty/weight/challenge traces, proof ordering and malformed witnesses | No additional acceptance mismatch identified in this pass. Continue differential mutation coverage at fork boundaries; this is not an exhaustive proof. |
| VDF consensus and scheduling: `Node_verify.cpp::verify_vdf*`, `Node_update.cpp::verify_vdfs` | `mmx-consensus/src/vdf.rs`, `mmx-node/src/vdf_worker.rs`; opaque work evidence rechecked against current history, native infusion/segment/continuation fixtures, two-job worker bound | C++ trust ranking of competing timelords and specialized CPU multi-lane SHA remain missing. |
| Fork choice/sync: `Node.cpp::find_best_fork`, `fork_to`, `Node_update.cpp::on_sync_done`, `Router.cpp::process` | `mmx-execution/src/chain/{choice,forks,sync,voting}.rs`, `mmx-node/src/sync/peers*`; native fork/vote/root witnesses, three-observer socket tests, competing contents, timeout reassignment and deep reorg tests | Requested 3–8 observer policy is deliberately stricter than native completion. Every observed content is fetched once; no majority filter. Native transmit credits remain separate from body leases. |
| Body/economics: `Node_validate.cpp::validate`, `execute`, native reward and fee helpers | `mmx-execution/src/{block,transaction}`, `mmx-consensus/src/authorization.rs`; native block/transaction/economics/NFT/multisig traces and rejection ordering | Canonical-parent validation hands off one verified execution delta; independent transactions still execute serially. Dependency locks and predecessor plans exist, but are not a concurrent executor. |
| Contract execution: `vm/Engine.cpp`, `vm/instr_t.cpp`, `vm_interface.cpp`, `vm/StorageCache.cpp`, `Node_validate.cpp::prepare_context` | `mmx-vm/src/engine*`, `mmx-execution/src/lib.rs`; all declared opcodes, gas, composite ownership, remote calls, hardfork setup, locks, failed-engine publication, recursion/copy/erase and input limits have fixtures | JavaScript compiler and remaining original contract suites are unported. Per-engine/per-transaction storage overlays are bounded by execution and are distinct from a broad node state cache. |
| Native DB: `DataBase.cpp::Table::{find,commit,revert,flush}`, storage versioning | `mmx-db/src/{native,journal,sorted}.rs`; append-only values, reference-only sorted compaction, positional reads, bounded reader handles, pinned MVCC snapshots, atomic suffix publication; crash/truncation/checksum/reopen tests | Block/transaction payload consolidation is complete for new writes; synchronous durability and range scan materialization below remain. No SQLite backend. |
| Rollback/purge: C++ `Node` storage/apply/revert and versioned tables | `mmx-execution/src/chain/{mod,forks}.rs`, `mmx-db/src/native.rs`; every retained-height rollback, deep fork/restart, crash-safe collection, pinned snapshot and RPC floor tests | Purge necessarily rewrites retained live values during explicit collection. There is no automatic background migration or ledger scan. Rollback below the durable floor is refused; private consensus ancestry does not reappear through RPC. |
| Peer and public RPC: `Router.cpp::{on_*,send*,receive_msg_hash,relay_msg_hash}`, `Node_api.cpp`, `WebAPI.cpp` | `mmx-node/src/{peer,inbox,rpc,http,transactions}.rs`, VNX safe readers; native request allowlist, 32 peer workers, absolute frame deadlines, compressed/decoded/schema limits, separate transaction inbox, HTTP framing/deadline tests | Native credits, rate policy, peer-role filtering and full inbound authenticated lifecycle remain incomplete. RPC range scans and validation can still occupy the ledger owner. REST/WebAPI parity is incomplete. |
| Farmer/wallet: `Farmer.cpp::{sign_block,sign_vote,handle}`, `Wallet.cpp`, `include/mmx/ECDSA_Wallet.h`, key derivation and spending | `mmx-farming/src/{farmer,wallet,spending,service}.rs`, `mmx-wallet`, `mmxwallet`; native keys/KDF/signatures, protected-key unlock verification, pending/reservation/fork/ambiguous-submit tests, real daemon RPC tests | Pool partial submission, reverse farmer RPC, complete wallet/history/offer/swap API and wallet query caches remain missing. Farm reports use bounded retention. |
| Harvester/plots: `Harvester.cpp::{check_queue,lookup_task,reload}`, `pos/Prover.cpp`, `pos/verify_full.cpp` | `mmx-farming/src/{harvester,plot,encoding}.rs`, `mmx-tools/src/lib.rs`; bounded shared Rayon pool, immediate proof emission, native deadline, filter-before-full-proof, density-based park search, retained headers, positional park reads/reuse; stalled-plot test and live sample | Batch scheduling still blocks subsequent challenges and reporting on a stuck drive. Metadata-free/corrupt-quality paths need additional slow-resource comparisons. Remote recompute remains missing. |
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
   Port `Router`'s received/sent caches, `ReceiveNote` feedback, staged randomized
   sending, transaction cost/upload credits, priority congestion handling,
   peer-role filtering and authenticated incoming lifecycle. Current bounds
   protect memory and only verified data relays, but are not equivalent to
   native per-peer fairness. Signed junk from a connected peer can still consume
   admission/CPU slots; the eight authenticated sync providers do not authenticate
   every inbound gossip connection. Preserve the distinction between transport
   deduplication and consensus proof acceptance. Native early block/VDF relay
   spends one earned farmer/timelord credit; never add an unbounded bypass based
   solely on a claimed identity or height. Add adversarial multi-peer latency
   tests before changing this policy.
3. **Streaming RPC range reads (memory and ledger latency).**
   C++ balance `find_range(..., limit)` stops in the table. Rust's unwhitelisted
   `all_balances` materializes the address prefix with `scan` before applying
   `limit`; its 10,000-result check is after a prefix read. Add bounded range
   iterators/snapshot execution, and enforce work/output budgets while reading.
   Also move expensive public `validate` off the ledger mutex with a pinned
   snapshot and bounded workers. Existing request/response limits do not bound
   all intermediate range-query work.
4. **Overlapping harvester scheduling (slow-drive isolation).**
   C++ enqueues per-plot tasks and a completion task, then services the next
   challenge. Rust publishes individual proofs promptly, but the daemon holds
   its harvester lock until `lookup_each` completes. A hung drive can stall
   later challenges, farm reports and reload RPCs despite idle workers. Introduce
   owned immutable lookup jobs and bounded outstanding challenge work; avoid
   holding the service mutex across I/O, preserve newest-challenge preference,
   deadlines, immediate delivery and one global worker budget. Measure with
   C++ harvesting disabled; include one stalled reader and overlapping arrivals.
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
