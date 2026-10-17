# MMX Rust node

A Rust port of [`mmx-node`](https://github.com/madMAx43v3r/mmx-node), starting
with the smart contract engine and versioned database. The workspace now includes
a node executable with native peer sync, durable fork handling, native VNX read
RPC and HTTP JSON-RPC. Wallet and block-production services remain separate port
stages.

The behavioral reference is `../mmx-node` (`~/workspace/mmx-node`), currently at
`ca6438b37c08007106d58efe4a990f6ecb276649`. Port consensus behavior from the
reference implementation and its tests, including protocol-specific differences.
Consensus compatibility is required: identical transaction/block acceptance,
execution results, state transitions, gas and limits, failure behavior, and
protocol-version rules. The database's physical disk format is independent and
does not need to match C++. Do not silently replace unsupported behavior with
approximations.

## Build and run

Requires Rust 1.89 or newer and a C toolchain for bundled SQLite/Zstd. The disk
backend currently targets Unix filesystems with file locking, directory sync,
and file sync support.

Clone with `git clone --recurse-submodules`, or initialize the VNX dependency in
an existing checkout:

```sh
git submodule update --init --recursive
```

Normal builds use the checked-in VNI declarations and the Rust VNX submodule;
they do not need the C++ checkout, CMake, or a C++ compiler.

```sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all --check

# Run repeatedly to increment a contract counter in a persistent database.
cargo run -p mmx-vm --example counter -- ./tmp/counter-db

# Initialize/reopen genesis using the original network configuration/assets.
cargo run -p mmx-execution --example bootstrap -- ../mmx-node mainnet ./tmp/mainnet-db
```

Build and start the node using the original public network assets:

```sh
cargo build --release -p mmx-node --locked
./target/release/mmx-node --mmx-root ../mmx-node --network mainnet \
  --data-dir ./mmx-data
# Supply seed candidates with --peer HOST[:PORT]; otherwise use native seed config.
# Discovery fills a random pool of 3–8 distinct authenticated synced full nodes.
# Serve stored state without network access:
./target/release/mmx-node --mmx-root ../mmx-node --offline
# Keep a chosen height before resuming sync (all later orphans remain archived):
./target/release/mmx-node --mmx-root ../mmx-node --rollback-to 1000
```

Default listeners are local: native peers on the configured network port
(mainnet 11337), VNX service `Node` at `127.0.0.1:11331`, and HTTP JSON-RPC at
`http://127.0.0.1:11380/rpc`. Override with `--p2p-listen`, `--rpc-listen` and
`--http-listen`. `--sync-once` exits after coordinated sync. Ctrl-C stops the
servers and sync workers; each selected batch is already durable.

```sh
curl http://127.0.0.1:11380/rpc -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"get_status"}'
curl http://127.0.0.1:11380/rpc -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":2,"method":"get_block_hash","params":{"height":100}}'
```

One coordinator owns historical download jobs. It selects a random pool of
3–8 synced full nodes, verifies each native signed node ID and genesis, and
rejects duplicate identities and self-connections. Native peer discovery fills
the pool; sync waits when fewer than three qualified peers are available.
For every height, `get_block_hash_ex` collects `(block hash, content hash)` from
at least three distinct peers. Every unique content hash is fetched once with
`get_block(hash)` and recomputed/authenticated before archival. Hashes are not
majority-voted: two lying peers cannot hide the third peer's valid candidate.
A second provider is tried only after the original 15-second request lease
expires. Restart, rewind and tip probes reuse matching archived content.

This ports the height-job approach in native `Router::process()` and
`Node::sync_result()`, with a minimum of three rather than the upstream default
of two. All candidates reach native fork choice, CPU proof checks and ledger
validation; a rejected candidate does not discard another peer's valid block.
Unknown parents trigger bounded backwards discovery above genesis. Download
never lowers the fork-choice root to force acceptance of a peer branch. Native lookahead and
strict future-timestamp bounds also apply. The active view includes the selected
tip and downloaded prefix; retained future orphans are not automatically selected
after an offline rewind. Tests cover holding each requested height across timer
updates/restarts, then replaying when sync resumes.

Each connection correlates reordered native returns while serving peer requests,
with at most four body requests outstanding. Common content is fetched from its
fastest successful hash claimant; all selected peers still supply hash responses,
and a slower provider is tried only after the request lease expires.
Download windows retain up to 64
heights and target 8 MiB, including competing candidates, in an atomic archive
transaction. Progress logs separate hash queries, bodies, archive publication,
proofs and ledger replay. A final multi-peer tip probe runs after publication;
a changing tip keeps the node in historical mode. HTTP status includes `sync_peers`.
One bounded producer queries the next window while the current one validates.
It registers pending heights before peer I/O, using the existing root's lookahead,
and joins before those clients are reused on error or cancellation. Missing
ancestors and empty tip windows suspend prefetch. HTTP status also counts body
requests and timeout reassignments; logs distinguish network stages from the
remaining download wait after overlap.
Completed prefetched bodies are retained before the producer replies, including
on cancellation. Reopening the node reuses those bodies after fresh peer hash
queries; their presence alone does not advance the selected chain.
The single-source pipelined helper remains available for fixtures and local
snapshot tooling; the production executable always uses the peer coordinator.

Peer broadcasts enter a bounded FIFO without taking the ledger mutex, so proof
validation does not interrupt socket reads. The node update drains and validates
the queue; embedded callers must drive `Node::tick()`. The queue retains at most
32 MiB of decompressed messages and 16,384 entries, dropping oldest entries when
full. HTTP status exposes queued messages, bytes and the dropped-message count.

Immutable header caches are shared across validator/challenge walks. Speculative
replay retains only cached headers below its replacement height and publishes its
new cache after the database transaction succeeds. Wire descriptor declarations
are cached independently of record payloads. Neither cache establishes proof work.
New ledger/archive writes retain each complete block and derive its full header
when needed, avoiding another disk copy of all space proofs. Existing separate
header rows remain readable. Admission/publication warms the immutable cache only
after its transaction succeeds. Cold reads and replacement tests preserve legacy
unhashed fields even when a replacement block has the same ID.
Stored blocks and transaction records use a versioned descriptor-free payload:
class identities resolve against the exact checked-in schema, while raw Variant
bytes remain untouched. Unknown identities, truncation and trailing data fail.
The reader also accepts earlier self-describing database rows. Peer decoding
continues to require native stream descriptors; storage lookup is a separate API.
The compact form round-trips all 824 native serialization fixtures.

For repeatable performance comparisons, keep an earlier release binary and use
an already downloaded range. The benchmark makes SQLite backups, downloads that
same range over native sockets into isolated copies, verifies the final hash,
and reports elapsed time plus RPC latency. The source database is left intact.

```sh
cargo run --release -p mmx-consensus --example bench_pos -- 100
# Supply a self-describing native block to benchmark its first complete proof:
cargo run --release -p mmx-consensus --example bench_pos -- 200 /tmp/block.vnx
cargo build --release -p mmx-node --example serve_snapshot
python3 tools/bench_sync.py --source-dir ./mmx-data \
  --baseline /tmp/mmx-node.before --candidate target/release/mmx-node \
  --from-height 4000 --to-height 4500 --output ./tmp/sync-benchmark
```

The sync benchmark starts three frozen snapshot peers with independent node
identities, then uses the production coordinator. `serve_snapshot` is a local
benchmark utility that serves a previously validated archive, without a
single-peer exception in the node executable.
The isolated receiver starts with only the requested prefix, so its missing
suffix must be downloaded. The harness checks the final hash and, for binaries
with request counters, exactly one body request per missing block plus timeout
reassignments. Production rollback continues to retain every archived block.

In a local mainnet replay of heights 4000–4500, these cache/staging changes raised
the download-and-validation rate from 11.7 to 28.6 blocks/s. Including startup,
the retained future archive and shutdown, elapsed time fell from 66.9 to 38.3 s.
Both runs reached `B5801963A9C5BC34E7A277425B07880FA7A428945DE9647B034536E77F19E844`.
RPC p95 latency during those runs was 2.0 and 2.4 s respectively; larger atomic
windows improve publication throughput while increasing time between snapshots.

CPU proof recomputation parallelizes independent F1 inputs while preserving the
native canonical proof order. `RAYON_NUM_THREADS` can bound the worker pool.
Fork admission retains immutable space/transition evidence bound to every header
field (including legacy unhashed fields), exact parameters and resolved ancestry.
Replay rechecks that binding, header authentication, VDF linkage and full ledger
execution; stale evidence falls back to work verification. It never bypasses
body validation, and deep-prefix replay still re-verifies space work.

The read API supports parameters, heights/sync status, genesis/VDF hashes,
blocks/headers by hash/height, transaction lookup/height/block IDs, contracts and
balances. HTTP accepts bare names or `mmx.Node.*`; `get_status` adds node status.
Other HTTP methods use the generated VNX field representations (fixed byte arrays
and little-endian uint128 bytes), and method parameters are named objects.
Unsupported methods return explicit errors. These public RPC listeners expose
reads only. Native compressed peer requests carry lossless block records.

Socket integration tests download the native fixture chain, switch a deep fork,
query the resulting ledger over VNX and HTTP, rewind to genesis, reopen and replay.
A mainnet run validated through height 113,248 before replacing the original
single-peer downloader. Download then resumed with seven authenticated peers
and HTTP RPC available throughout. A complete mainnet
sync has not finished. The node uses an indexed disk backend with every
historical version retained. Sync throughput and remaining peer services are
still being improved.

The example executes MMX instructions, charges gas, stages contract state, and
commits it to disk. Running it again reopens the database and increments the same
counter. It does not require the C++ repository or a running MMX node.

## Workspace

| Crate | Implemented |
| --- | --- |
| `mmx-db` | Named versioned tables, size-first key ordering, inclusive historical reads, snapshot scans, atomic batches, rollback, and both an indexed SQLite WAL backend and a durable journal backend. Exclusive writer locking and atomic rollback/replacement protect both. |
| `mmx-node` | Executable, native Zstd peer protocol, historical download with ancestor discovery and native fork choice, live block/VDF/vote admission, durable rewind/restart, VNX public reads and HTTP JSON-RPC. |
| `mmx-types` | Generated MMX/VNX wire types, type registry, typed RPC clients and service dispatchers, built from the VNI snapshot using the VNX submodule. |
| `mmx-consensus` | Native hashes/costs and authorization, header/vote/farmer-response authentication, validator history, CPU space/VDF work verification, difficulty/challenge/weight transitions, output linkage and ancestor lookup. |
| `mmx-crypto` | Public-key address hashing and native ECDSA verification, with one signature cache shared by authorization and VM `VERIFY`. |
| `mmx-execution` | Contract loading/calls and constructors; transaction/block validation, NFT rewards, shared execution and atomic settlement; native network bootstrap, durable selected-chain state, orphan archives, native fork-choice policy and atomic branch replacement. |
| `mmx-vm` | MMX value types and compact serialization, instruction metadata and compressed bytecode v1, interpreter with heap references and collections, gas accounting, call frames, read-only execution, and contract storage adapters. |

The interpreter executes all 48 recognized opcodes: copy/clear and cloning,
branches and local calls, unsigned 256-bit arithmetic, comparisons, logical and
bitwise operations, shifts, type/size queries, string/binary concatenation and
slicing, arrays/maps and entry access, conversions, SHA-256, secp256k1 signature
verification, transfers, balances, events, logs, cross-contract hooks, and explicit
failures.
Multiplication preserves both the legacy overflow rule and the protocol-version-1
fix.

Heap references, arrays, maps, and nested collections execute with allocation,
reference counting, aliasing, entry mutation, lazy storage loads, and recursive
cleanup. `COPY`, `CLR`, `SIZE`, and `CONCAT` include their collection paths.
`CLONE` allocates a copied value and returns a reference; references inside a
collection keep their aliases rather than being recursively deep-cloned.

`CONV` includes native integer parsing/wrapping, bases, byte order, and MMX
Bech32m address presentation. Address decoding retains the reference's accepted
HRPs, uppercase encoding, and unused padding bits. `VERIFY` uses Rust `k256`,
including compact signature validation and high-S rejection. The native signature
cache's initially zero tuple and its eviction behavior are preserved and tested;
changing that upstream quirk requires an agreed consensus change.

`SEND`, `MINT`, and `BALANCE` use a private per-currency balance cache and stage
transaction outputs with raw byte memos. `StorageDb` supports versioned balance
snapshots. The VM stages debits and outputs; `mmx-execution` applies contract
balance debits together with state and returns outputs for the node's settlement
layer. `Engine::outputs()` exposes staged transfers only after successful execution, and refuses them after failure.

`Engine::run_with_host()` supplies `LOG`, `EVENT`, `RCALL`, and `CREAD` through the
`Host` trait. Callbacks can read VM values, charge gas, execute child bytecode, and
write return values. Remote calls enforce frame, argument, and recursion limits,
then clean up arguments while preserving the return slot. With no host, logs and
events skip their values, while remote calls and contract reads fail as in C++.
Hosts must stage their own effects and discard them on transaction failure. The
`mmx-execution` crate supplies contract loading, dependency resolution, and a
shared transaction overlay behind these hooks.

New heap objects are reclaimed when their last reference is released. Objects
loaded from storage stay pinned, matching C++; copying stored maps and erasing
stored collection descriptors remain errors in the reference VM. Read-only
execution protects loaded state but can allocate temporary objects; publishing
new persistent state still fails. A failed execution or commit cannot publish
state through that engine instance. Opcode coverage alone does not establish
full consensus compatibility.

## Database and execution semantics

Writes belong to the **current** version. `commit(next_version)` persists an
atomic batch across all tables, then advances the current version. For example,
writes at version 0 followed by `commit(1)` are visible in historical reads at
version 0. `revert(v)` discards writes at versions **greater than or equal to**
`v`, along with staged writes. This matches the C++ `Table` convention.

`Engine::run()` changes only its private memory. `Engine::commit()` first clears
stack references and reclaims unreachable new heap objects, then checks the
entire persistence gas cost before applying one atomic batch to its storage
backend. `storage.database_mut().commit(next_version)` then makes the staged
changes durable. The low-level APIs keep this split; `chain::ChainState` combines
validated block effects, archives and tip updates into one durable commit. Use a
fresh engine per invocation and serialize access to each storage backend.

`Database::commit_batch(next_version, writes)` validates and commits additional
writes with the original pending batch, without staging them before the journal
succeeds. Invalid writes, invalid version transitions and journal failures leave
the original visible/pending state unchanged. `StorageDb::commit_transaction()`
uses it to combine VM/ledger effects with caller-owned metadata tables. After a
journal I/O failure the handle rejects further writes; reopening resolves whether
the complete frame reached disk.

`Database::fork(version)` creates a detached, committed snapshot before that
version. `replace_suffix(version, &branch)` checks that retained history matches
exactly, then replaces every later version in one journal frame. Historical
reads, tombstones and version gaps survive replacement. A torn replacement
recovers the complete old suffix; a complete frame recovers the complete new
suffix. Both snapshots must have no pending writes. Snapshots currently copy
the RAM index, and the complete replacement must fit the 64 MiB frame limit.

`mmx_execution::prepare()` runs a batch of already authenticated and funded
invocations against a disposable overlay. It loads native VNX contract records,
checks public/constructor methods and dependency locks, sets transaction/caller
externals, handles deposits, and copies nested arguments and return values between
engines. Setup timing follows the configured hardfork-2 height. Contract read
costs come from the chain index, independently of repository caching or disk I/O.
Raw metadata strings, method names, and dependency names retain non-UTF-8 bytes.

A `PreparedExecution` holds exclusive backend access while the node checks dynamic
fees and execution inputs/outputs. `batch_mut()` lets it add settlement and fee
writes; `commit()` publishes the entire batch atomically. Dropping the prepared
execution discards it. `execute()` is a convenience that prepares and commits
immediately. A later operation's failure discards earlier child commits, balance
changes, and deployments while preserving pre-existing pending database writes.
The caller still advances the database version separately.

Outputs follow native child-before-parent ordering and are not credited as
deposits for later invocations in the same transaction. Native const methods skip
their own storage commit; they do not force child engines into read-only mode.
Non-const commits clear their return slot, matching the reference executor.
Child gas is added only after successful execution and return copying; outer VM
costs are clamped on failure as in `Node::execute`.

The invocation API expects transaction authentication, static contract/dependency
validity and deposit funding checks to have succeeded. Its `Receipt::total_cost`
includes supplied initial cost, contract reads and execution, before execution
input/output costs. The `mmx_execution::transaction` API combines these steps.

`transaction::prepare(&mut storage, &context, &transaction)` checks native static
validity, the active transaction version, duplicate IDs, sender authorization,
fee funding, expiry, explicit inputs/outputs, deployment validity/dependencies and
per-operation user authorization. It runs constructors and calls at their native
positions, accounts for deposits and contract spending, creates execution inputs
and outputs, applies implicit-deposit rules, checks dynamic fees, and validates
any supplied execution result. Version 0 retains the reference's GCC/libstdc++
unordered-map iteration order for implicit deposits, including bucket collisions
and rehashes; version 1 sorts currencies.

`PreparedTransaction::result()` exposes success or a charged execution failure.
Rejections return an error and publish nothing. On commit, failed transactions
publish only their fee debit and transaction index; successful transactions
publish VM state, deployments, settlement balances and the index together.
Settlement credits all outputs before clamping input debits, matching native
single-transaction block application. Ledger validation uses the original
balance snapshot independently of the VM's deposit/spending cache. `LedgerStorage`
is an explicit backend capability requiring index queries and atomic publication
of every batch field; `StorageDb` implements it.

Dropping a prepared transaction aborts it. `transaction::execute()` prepares and
commits immediately, and `transaction::with_result()` attaches a computed result
while recalculating static cost and the content hash. The caller separately
advances the database version. Transaction index height and deployment read cost
survive reopen and rollback, including read costs for failed deployments whose
contract record is absent. Error codes, addresses and operation indexes are
enforced when matching results; diagnostic line/message fields retain the native
consensus exemption.

Sequentially committing transactions through the single-transaction API does not
implement native block validation. Use `mmx_execution::block::prepare()` for a
post-genesis block body, after the chain layer verifies proof work and transitions.
`BlockContext` supplies the verified parent corresponding to the backend snapshot and the
expected VDF payout. The body API checks parent linkage/height, transaction
content hashes, the native binary-tree root, cost/fee/count commitments and
limits, transaction activation, duplicate IDs, descending fee ratios and the
size-dependent minimum fee tiers. It also checks header static validity, header
and content hashes, the farmer signature, timestamp delta bounds, space-fork
length and difficulty minima before executing transactions.

Before execution, the body API reserves every transaction fee and successful
transaction's explicit inputs against the original ledger snapshot. It computes
native dependency locks and predecessor sets, then executes serially in block
order, a valid schedule for the native dependency graph. VM state and spending
caches carry forward between successful transactions. Authorization, explicit
funding and contract metadata continue to read the chain snapshot: earlier block
outputs cannot fund later validation or VM spending, and new deployments remain
unavailable to other transactions until block application.

`PreparedBlock::receipt()` exposes the validated results, execution plan, ordered
block inputs/outputs and supply deltas. A rejected block publishes nothing,
including earlier successful transactions. Dropping the preparation aborts it;
commit publishes VM state, deployments, all settlement balances, transaction
indexes and supply counters together. Settlement credits every block output,
including VDF, farmer and project rewards, before applying clamped input debits.
Native total-supply accounting clamps each currency's net issuance to zero
before adding it to the persisted counter. `BlockStorage` requires supply queries
and atomic publication in addition to the ledger capability; `StorageDb`
implements it. The caller separately advances the database version.

The body API checks reward activation, fee buffers, project address continuity,
reward votes/adjustments, reward amounts and VDF payout cadence. Public
`block::economics` helpers preserve native rounding/wrapping and select the VDF
winner from authenticated ancestors, including tie-breaking with the oldest
visited ancestor's proof hash. A successful body validation requires the chain
layer's proof work and transitions before block acceptance. `ChainState` supplies
that integration; fork choice and node services remain separate.

`mmx_consensus::header` exposes exact header hash serialization, hash/content-hash
calculation, static validity, farmer authentication and contextual scalar checks.
The hash preimage retains the actual wire type identity: native `Block` and
`BlockHeader` hash differently even with identical fields. Slicing a signed Block
to a BlockHeader preserves the stored hash while changing its preimage. The
preimage includes `proof_chain` starting at hardfork 2, signed reward fields and
raw 128-bit weights. Static validity and `authenticate()` remain separate, as in
C++; authentication verifies
the supplied hash using the first proof's farmer key and the shared crypto cache.

OG/NFT proof records support `Record::calc_hash()` and `proof::is_valid()`.
`proof::calc_proof_hash()` hashes the challenge and little-endian 32-bit proof
coordinates, and `calc_plot_id()`/`validate_plot_id()` implement the native OG/NFT
identity domains. Header static validity checks proof shape and does not call
plot identity validation or verify proof work. The chain layer must perform
those separate stages before block acceptance.

`proof::verify()` implements native OG/NFT space-proof verification in its
original order: shape, expected challenge/difficulty, plot identity, plot filter,
ksize limits, CPU recomputation, quality threshold and proof score. `pos` ports
the 4 KiB memory hash, SHA-512 leaf generation, eight matching tables, full
metadata deduplication and canonical proof backtrace. It enforces the Y window,
the original X ordering, and hardfork-1 post filtering/quality selection. Proofs
contain 256 uncompressed X values; compressed plotting/proving and GPU/remote
compute remain separate work.

`transitions::verify_header()` checks proof ordering/uniqueness, proof-chain
commitments, VDF height and expected iteration totals, bounded time-difficulty
changes, challenge infusion (including hardfork-2 forced infusion), space
difficulty/fork counters, weight and cumulative weight, then every space proof.
`ProofContext::resolve()` derives its expected challenge, difficulty and infusion
iteration counts through an authenticated `history::HeaderSource`. The lookup
rules retain native challenge delay, skipped VDF heights, pre-genesis challenges
and commit-delay clamping; missing non-genesis ancestors cause an error.

`block::prepare_with_proofs()` combines these checks with header authentication
and prepared body settlement, requiring the same trusted parent in both contexts.
Header hashes and the farmer signature are checked before CPU space work. The
lower-level `block::prepare()` continues to expect separately verified space
proofs/transitions. These two APIs require separately verified VDF work/output
linkage before accepting a block; iteration counts alone do not establish VDF
work. Every body preparation API checks NFT reward targets against the original
ledger snapshot.

`mmx_consensus::vdf` ports ProofOfTime and VDF_Point hash serialization and static
validity, timelord authentication, history-dependent admission and portable CPU
work verification. Admission checks the infused block's committed time difficulty,
iteration count, segment size and segment count in native order. CPU verification
mixes the input, infused block hash and reward address, then checks each recursive
SHA-256 segment. `vdf::verify()` combines these stages and constructs an immutable
`VerifiedPoint`; decoding a point or checking its metadata hash does not establish
verified work. The signed proof remains available through `VerifiedPoint::proof()`.

`vdf::link_block()` follows verified outputs backward through the expected
infusions and iteration counts, requiring the final input to equal the trusted
parent's output. Equal-output candidates retain insertion order. It preserves
native consensus exemptions: linkage does not check signed point start/height
metadata or compare block reward addresses with point reward addresses.
`block::prepare_with_vdf()` authenticates the header, resolves its proof context
from trusted history, links admitted VDF points, verifies space proofs/transitions
and prepares atomic body settlement. Supply points verified with the same chain
parameters. Its caller supplies the ancestor-derived VDF payout winner and
trusted history; `ChainState` derives those inputs from its persisted tip.

`mmx_execution::nft::target()` implements native Plot NFT reward resolution. It
falls back to the contract address when the executable, binary or
`mmx_reward_target` method is absent. Otherwise it calls that method using the
ledger peak's height and protocol version, a canonical address string or null
farmer argument, null user/transaction ID, the contract address and chain network.
It uses `max_tx_cost` without charging contract metadata reads. Method visibility
and constructor flags do not restrict this native call; the method's const flag
controls storage immutability, and every call discards writes and transfers.
Remote calls and contract-read callbacks remain unavailable in this path, as in
C++. The return conversion preserves native VNX behavior, including invalid
address strings becoming zero and byte lists being truncated or zero-padded.
All list elements are read before conversion, including those beyond byte 32.

Block validation requires `reward_contract` to match the first NFT proof only
when `reward_addr` is present, resolves the target with `reward_account`, and
checks the result before transaction execution. It uses the parent's height:
an NFT unlocking at the candidate's height remains locked during that block's
reward validation. Calls cannot observe transactions or new deployments in the
candidate block, and their temporary writes never enter the settlement batch.

`mmx_execution::chain::genesis::build()` reproduces native genesis construction
from a trusted `genesis::Assets` manifest. Its nine transactions are ordered as
offer, swap, token, plot NFT, NFT, template, escrow, time lock and relay. Mainnet
prepends the testnet rewards transaction and validates a separate project-relay
transaction to obtain `project_addr`; that relay is not deployed at genesis.
Other networks use the reference testnet project address. The VDF/challenge
domains, nonce, initial difficulties, timestamp casts, transaction commitments
and header hashes retain native rules. Bootstrap transactions receive native
genesis checks rather than normal transaction validation, and constructors and
operations do not execute.

`chain::network::Network::load(root, configuration)` reads the original
`config/<configuration>` tree through VNX, including parent inheritance, namespace
JSON files, leaf overrides and array appends. The configured `chain.params.network`
selects `data/<network>`; configuration overlays can have different directory
names. `Network::from_config()` accepts an already layered configuration with
caller overrides. Both return the generated parameters and lossless bootstrap
records for `ChainState::initialize()`.

The loader reads all nine binaries in native order and mainnet's rewards JSON
and separate project relay. Text conversion preserves MMX addresses, native
integer casts/defaults, enum names and 128-bit decimal/hex amounts, including
the native permissive digit parser and overflow behavior. The shared VNX parser
retains native string escapes, including literal `u0041` for `\u0041`. Binary
files use the strict consensus reader. Loading reports file paths on errors and
does not publish state; genesis checks remain in `genesis::build`/`initialize`.
Files are bounded to 64 MiB. Text support covers the shipped distribution and
its input/output/execution-result records; floating-point integer conversions
and non-null polymorphic contract/operation/solution JSON payloads fail explicitly.
General native text/schema migration remains separate work.

`chain::ChainState` owns the storage backend and exposes read-only ledger access.
`initialize()` applies genesis balances, deployments, read-cost indexes and
supply. `append()` authenticates the header, derives challenges/infusions and the
VDF payout from stored history, links independently verified VDF points, verifies
space work/transitions, resolves NFT targets and validates the body. It commits
VM writes, balances, deployments, transaction indexes, supply, lossless records,
height indexes and the selected tip in one durable database transaction. Block height `h` writes
at version `h` and commits version `h + 1`. Reopen checks the exact stored chain
parameters and the tip/height/version relationship. Headers have their own table
and retain the signed Block's stored hash when sliced to BlockHeader.

```rust
let mut chain = mmx_execution::chain::ChainState::open(path, params)?;
chain.initialize(&assets)?; // once, on a fresh chain
let point = mmx_consensus::vdf::verify(&time_proof, chain.params(), &chain)?;
chain.append(&candidate_block, &[point])?;
```

`revert(h)` durably removes selected blocks and ledger effects at heights `>= h`;
`revert(0)` also removes genesis. A replacement branch can then be verified and
appended explicitly. Selected block/header/transaction archives rewind with the
ledger. `reorganize(first_height, &blocks_with_verified_work)` validates the
replacement on a detached ledger and publishes it with `replace_suffix` only
after every block succeeds. Genesis cannot be replaced. Failed execution or
database I/O cannot expose a partially reverted chain.

`chain::forks::ForkChain` adds a separate durable archive, independent of selected
ledger rollback. `admit()` checks native block commitments and farmer signatures,
retains disconnected blocks and ordered signed VDF proofs, and merges later
work without changing the first block's arrival order. The archive writes ahead
of selected state and binds exact chain parameters and genesis. Reopen does not
trust saved proof-verification flags: VDF work and space proofs are checked again.

`update(selected_root_hash, lowest_pending_sync_height)` resolves the native
fork-choice policy, replays the winning branch against its actual ancestor
snapshot, and atomically switches the ledger, transaction indexes, archives and
tip. Losing blocks remain available through `ForkChain::block()`. Missing parents
or work leave candidates waiting; invalid bodies are excluded and selection
continues with another candidate. Alternate-root branches wait for backwards
sync when native policy requires it and replay the prefix below the old root
before switching. That historical prefix rechecks space proofs and full execution
without old PoTs, matching native `fork_to`; live blocks above the root still
require VDF evidence. The node preserves its current root while downloading
competing branches and updates the root hash only after branch selection.
Replacements stream records and discard intermediate receipts during node replay,
so a deep fork does not accumulate complete block bodies/results in RAM.

The standalone `chain::choice::select()` ports native proof/invalid propagation,
wrapping cumulative votes, root/peak weight comparisons, height/hash tie-breaks
and the backwards-sync guard. It accepts already authenticated vote counts.
`ForkChain::admit_vote()` authenticates native vote content/signatures and queues
votes until the referenced block's proof is verified. `update_at()` supplies an
explicit clock for deterministic processing; `update()` uses wall time. Validator
membership is selected once at proof verification, starting with the unclamped
commit-delay ancestor, then scanning at most `max_history` headers for distinct
first-proof farmer keys. The default history limit is the native 1,000 headers.

New votes require the best known proof for that challenge and a key in the
block's validator set. Each key counts once per block. Accepted votes retain
their count when stronger proofs arrive and do not expire; pending votes time
out after three block intervals, using the native strict boundary. The proof
pool accepts additional `proof::VerifiedProof` evidence. Votes, eligibility and
proof-ranking caches reset on restart, matching the native process; durable
ledger/orphan state remains intact. Invalid vote content short-circuits signature
parsing, and signature verification shares the VM/authorization cache.

`ForkChain::rollback_to(height)` keeps that selected block and durably removes
every later ledger/index version while retaining every archived branch. Any
selected height, including genesis, is supported; no root/checkpoint introduces
an irreversible state cutoff. Reopen and subsequent fork selection can replay
the retained branches. Specialized wallet/trading indexes, archive compaction
and remaining node services are separate port stages.

The live node admits signed farmer `ProofResponse` envelopes only after native
hash, plot identity, signature, selected-history challenge/difficulty and CPU space
work checks. Pending responses wait for VDF progress and expire after ten block
intervals. Harvester identity, lookup time and farmer routing metadata are excluded
from hashing as in C++. VDF peak reads extend the block tip only through verified
CPU evidence with matching infusion, iterations and input paths; raw metadata
cannot extend it. Additional oracles cover 80 response mutations and 240 native
next-VDF graphs. Active fork proofs and CPU work are cached transiently, rebound
to the configured chain before use and reset on restart. Fork updates read the
active archive range and lazily resolve older headers; retained history is intact.

Historical block download uses explicit `append_sync()` / `update_sync_at()`
entry points. As in native `!is_synced`, only VDF linkage is omitted; space work,
signatures, header transitions and complete ledger execution still apply.
Already verified sync proofs remain eligible when switching to live mode, while
new live blocks require verified VDF points. These flags reset on restart.
`chain::sync::Policy` ports native proof-density root advancement, including weak
fork delays, sync positions, pending jobs and integer widths. Soft roots never
remove rollback history. Its oracle compares 262 native commit schedules.

```rust
let mut forks = mmx_execution::chain::forks::ForkChain::open(path, params)?;
forks.initialize(&assets)?; // once
let root = forks.selected().hash_at(0)?.unwrap();
forks.admit(&candidate_block, &[signed_time_proof])?;
let update = forks.update(root, None)?;
```

`mmx_consensus::Record::decode()` reads original VNX bytes without a JSON
conversion. `calc_hash(full_hash, hash_version)` implements native MMX hashing;
transactions select their own hash version, and solution/result hashes retain
the native version-0 rules. `hash_serialize()` exposes the exact transaction
preimage. Transaction IDs omit solution indices and input flags; content hashes
include them, signatures, static cost and execution results. Execution-result
input flags/solution indices remain excluded even from content hashes.

`is_valid(&ValidationParams)` and `calc_cost()` port the native static checks,
including version-1 duplicate/unused solution rejection, memo/message limits,
JSON metadata restrictions, contract/method flags and wrapped cost arithmetic.
Contract validity is exposed separately: native `Transaction::is_valid()` does
not validate the deployment itself. These checks do not authenticate signatures,
enforce the active transaction version at a height, check balances or authorize
execution. `mmx_execution::transaction` combines them with those checks.

`mmx_consensus::authorization` ports `Contract::validate()` and the sender,
input and operation-user signature selection rules. `authorize_sender()` always
uses solution 0 and a PubKey at the sender address. `authorize_input()` performs
a contract lookup only when `IS_EXEC` is set. `authorize_operation_user()` reads
the user's contract and falls back to PubKey when it is absent; anonymous calls
perform no lookup. Execute and Deposit use the same user authentication rules.

Run these steps at their native positions after static validation: sender
authentication precedes fee debiting, input authentication follows its balance
check, and user authentication follows the target-executable check. The lookup
callback must use chain contract records and charge the native read cost at the
appropriate transaction/engine gas meter. Eagerly authenticating all operations
would change failure order and gas. These functions do not check balances or
fees, run bytecode, or publish state; `mmx_execution::transaction` connects these
steps to execution and settlement. The lower-level `mmx_execution::prepare()`
continues to accept authenticated, funded invocations.

Version 0 multisig validation ignores the declared threshold, non-owner entries
and non-PubKey owner solutions. Version 1 requires the declared threshold to
match, rejects non-owners and rejects non-PubKey owner solutions. Every relevant
signature is checked even after the required count has been reached. Typed errors
preserve native static-failure versus ordinary execution-failure classification;
all sender authentication errors reject the transaction. Signature/public-key
parsing failures remain distinct from a signature that verifies false.

The consensus reader currently accepts the checked-in schema versions only;
older schema conversion/default insertion and exotic VNX dynamic forms are
explicit errors. Hashing supports MMX JSON values plus the covered native
opaque/typed scalar arrays; typed composite Variant array elements are not yet
supported. Strings retain raw bytes, signed and unsigned dynamic integers retain
their tags, and non-native Variant types retain their original payload.
`Record::from_generated()` is convenient for generated declarations, but their
JSON-backed Variant fields cannot represent every native distinction. Use
original bytes for consensus inputs.
`Record::encode()` re-emits current-schema VNX records while preserving original
Variant bytes and raw strings, declaring repeated dynamic classes once per stream; it is used to persist deployments without a JSON
round trip. It remains distinct from hash serialization.

The node defaults to `Store::open_indexed()` / `ForkChain::open_indexed()`, using
SQLite WAL with full synchronous commits and indexed versioned rows. Point reads,
size-first ranges, prefix scans and inclusive historical reads share the journal's
logical behavior. Storage adapters use fallible owned reads so disk failures are
reported. `Store::open()` and `Database::open()` retain the original journal
backend for existing library callers and reference tests.

Indexed forks keep a read snapshot of the original database and store speculative
changes in private disk databases. They do not copy retained history or buffer an
entire replacement in RAM. Publication copies the staged suffix and updates the
tip/version in one SQLite transaction, including replacements larger than 64 MiB.
Snapshot identity/generation checks reject stale or foreign replacements. Failed
writes poison the handle until reopen; uncommitted publication recovers the old
state and committed publication recovers the new state. No history is pruned.

Existing Rust journals import once on indexed open, retaining the source journal.
Journal callers then reject that directory to prevent divergent writable copies.
Bounded header caches avoid repeated historical decoding; selected-chain caches
are cleared on rewind/replacement. The checked-in type registry is parsed once,
with independent registries returned for each native stream. Historical sync
ignores unrelated live block broadcasts, and a periodic update coalesces queued
proof/vote processing as in C++.

Node identity creation and initial schema/import use atomic file publication.
Private branch initialization and writes omit fsyncs until publication into the
primary database's fully synchronous WAL transaction. Unpublished staging
directories are removed after restart. Tests compare the two backends across
pending writes, deletes, ranges, historical reads, every rewind, nested snapshots,
reopen, migration, injected write failures, abrupt process exit before/after
publication, and a 65 MiB branch replacement.

The journal backend uses a **new Rust journal format**, independent of C++ files.
Use a separate data directory. This reference backend retains history in memory
and replays the journal on open; the node uses the indexed backend above.
Reproducing the C++ block/index file format is not a port requirement.
Uncommitted writes are discarded on reopen. Complete records with invalid
checksums are reported as corruption; only an incomplete final record is removed
during recovery.

MMX-specific value and instruction encodings target the little-endian reference
format. Contract storage preserves the original 20-byte contract prefix and
big-endian cell/entry addresses; value-index keys use the full 32-byte contract
address. Address presentation uses the consensus Bech32m codec in `mmx_vm::address`.

## VNX integration

[VNX Rust](https://github.com/mmx-network/vnx-rust) is included as the pinned
[`vendor/vnx-rust`](vendor/vnx-rust) Git submodule. Workspace path dependencies
use its `vnx` runtime and `vnx-codegen` generator. The contract loader retains
original wire bytes; the runtime's opt-in byte-string view is used only for stream
inspection, so non-UTF-8 data never passes through a lossy execution value.
Update the gitlink deliberately
and run the compatibility tests when adopting a new revision.

`mmx-types` compiles the declarations in [`schema`](schema) at build time and
exports `mmx`, `vnx`, `vnx_registry()`, and the `VnxType` trait. The snapshot
contains MMX declarations and the VNX/VNX-addons declarations needed to resolve
their dependencies; source revisions are recorded in
[`schema/origin.json`](schema/origin.json). The VM uses the generated native error
codes, and tests carry an MMX contract binary through VNX serialization into VM
execution.

VNX provides serialization, reflection, runtime, RPC and transport. MMX's `var_t`
and symbol-compressed `instr_t` formats remain application-specific in `mmx-vm`.
Generated declarations do not implement native MMX validation, transaction
hashing, consensus, or node services; `mmx-consensus` supplies the static MMX
behavior separately. VNX wire serialization must not be used as
a substitute for MMX's consensus hash serialization. Fixed byte arrays retain
their declared wire layout; address presentation, amount arithmetic and crypto
operations belong to the MMX port.

To refresh the schema snapshot and native identity fixtures from the reference:

```sh
python3 tools/update_schemas.py --source ~/workspace/mmx-node
```

This copies protocol declarations and reads generated C++ type metadata without
running or linking C++; review schema and fixture changes together.

## Tests and compatibility

Rust tests port the applicable cases from `test/test_database.cpp`,
`test/test_engine.cpp`, `test/vm/engine_tests.cpp`, and
`test/vm/storage_tests.cpp`. The checked-in fixtures also compare **10,632 C++
execution cases**, including successful-execution gas, plus value encodings in
all four metadata modes and compressed instruction bytes. Ordinary tests use
these fixtures without a C++ build dependency.

An additional **21,933 C++ composite trace steps** exercise heap ownership, all
ten scenarios from `test/vm/storage_tests.cpp`, collection instructions, nested
state, rollback, limits, and failures. Traces compare returned values, allocated
addresses, gas, error codes/locations, and every successful persistence write.
Each relevant operation is replayed with gas just below, at, and above its
reference cost; compound operations also sweep intermediate gas limits.

An additional **26,343** C++ trace steps cover the remaining nine opcodes:
type/format conversion matrices, address edge cases, signature scalar limits and
cache eviction, transfers and memos, cached balances, callback effects, remote
return values and stack cleanup, indirect operand ordering, and gas boundaries.
Tests also execute real child bytecode through the host interface, verify that
failed executions hide transfers, and reopen/roll back balance snapshots.

These fixtures validate the covered VM behavior, not complete consensus
compatibility. Extend coverage to transaction/block execution as those layers
are ported. Storage implementation changes must preserve the
same logical reads, state updates, and VM gas independently of physical I/O or
cache behavior.

The execution layer adds **3,710 native C++ execution traces**, comparing complete
state writes, balances, output ordering, spend maps, gas, operation/PC/error codes,
and line information. They cover both sides of hardfork 2, constructors, child
failures, multi-operation rollback, visibility/locks, stored-map copy rejection,
copy recursion, non-UTF-8 names/arguments, metadata conversion, and gas boundaries.
The original `engine_tests.js` and `compiler_tests.js` suites are compiled by the
native compiler during fixture regeneration and then run successfully in Rust,
with matching costs and persisted state. Normal Rust tests need no native tools.
Additional tests cover reopen/forks, deferred commit/abort, atomic multi-table
validation, and dependency depth/width limits.

The consensus layer adds **824 native C++ cases** comparing transaction preimage
bytes, both hash versions and full-hash modes, static validity, costs, contract
sizes and overflow failures. They cover all concrete contracts/operations/
solutions, execution results, signed integers, non-UTF-8 strings, alternate byte
order, opaque floats/tuples, JSON/copy recursion boundaries, memo limits and
500 deterministic transaction combinations. Further tests reject truncated or
forged schemas and verify the different coverage of IDs and content hashes.

Authentication adds **1,232 native C++ cases** covering all contract validation
paths, both multisig versions, signature selection, missing/invalid solutions,
failure reasons/classes, lookup order and injected read-cost boundaries. A further
**418 native crypto cases** compare addresses, verification, scalar/key parsing,
high-S rejection, wrong messages/signatures and zero-cache eviction. Separate
integration binaries prove cache sharing in both directions between authorization
and VM `VERIFY`; signed PubKey and multisig transactions also pass static and
signature checks together.

The transaction layer adds **2,016 native C++ comparisons**. Its generator extracts
the original `Node::validate` and both `Node::execute` methods into a harness with
test storage/indexes. Cases compare acceptance/rejection, failure classes, costs,
fees, execution inputs/outputs, error locations, every VM write, stored contracts,
deployment index costs and settled balances. They include hardfork boundaries,
signature parsing/failure order, fee/read budgets, constructor/child rollback,
deposits, wrapped 128-bit amounts, raw memos/arguments, legacy currency order, and
supplied-result mutations (including ignored diagnostics/input solution indexes).
Additional tests cover prepare/abort, durable commit, reopen, duplicate checks and
rollback of transaction indexes. Consensus fixtures also round-trip losslessly
through the byte-preserving VNX encoder.

Block processing adds **114 native block comparisons**, **468 economic cases**
and **80 VDF winner cases**. The oracle compiles the original block validator,
dependency preparation and transaction methods with serial task scheduling.
Cases compare reservations, dependency/predecessor sets, shared state/spending,
child rollback, deposits, deployment visibility, forged isolated-state results,
fee tiers, body commitments, rewards, every VM write, ordered inputs/outputs,
settled balances and supply counters. Arithmetic cases include 64-bit wrapping,
fee rounding, vote thresholds and reward bounds. VDF cases cover counts, ties,
the oldest hash salt and short history. Durable tests cover whole-block abort,
reopen, index/supply rollback and preservation of pre-existing pending writes.

Header processing adds **570 native header comparisons** and **100 OG/NFT proof
record comparisons**. Header cases compare exact preimages, hashes, content
hashes, static validity, farmer signature errors and contextual scalar checks.
They include hardfork boundaries, actual/sliced type identities, genesis,
signed/128-bit fields, timestamp boundaries, malformed signatures/keys, null and
duplicate proofs, and the distinction between proof shape and plot identity.
Proof cases check coordinate endianness, size limits, plot IDs and identity
corruption. Integration tests prove header tampering and invalid farmer
signatures publish no block state.

Space work adds **612 native comparisons** covering memory/leaf vectors at k8,
k16, k29 and k32; accepted/rejected CPU space proofs; OG/NFT verification;
production plot/post-filter settings; transition mutations; difficulty and
quality arithmetic boundaries; ancestor lookup and complete context resolution.
Integration cases run authenticated, proof-aware block preparation and reject
signature/weight tampering without publishing state. Accepted witnesses use
small k8 plots with overridden minimum ksize so fixture generation stays cheap;
production k29/k32 leaf arithmetic is compared separately. The oracle calls the
original native CPU verifier and copied Node verification/ancestor methods.

VDF work adds **452 native comparisons**: 205 proof/admission/CPU cases, 184
output-linkage cases and 63 combined VDF/space-proof block cases. They compare
exact proof/point preimages and hashes, signatures and failure order, zero/odd
segment counts, production segment size, chunk boundaries, corruption, iteration
wrapping, commit-delay history, signed metadata exemptions and multi-step output
chains. CPU cases run both native hardware dispatch and its fallback. Combined
cases prepare or reject empty block bodies without publishing state on rejection
or abort.

NFT rewards add **212 native comparisons**: 185 target-resolution calls and 27
authenticated block reward-field cases. They execute the original Plot NFT
contract at lock/unlock and hardfork boundaries, compare fallback and return
conversion rules, lookup/failure order, method flags, gas boundaries, ignored
transfers and stored collection failures. Block cases check target/contract
mismatches and parent height at unlock. Tests verify that target calls and
rejected/aborted blocks preserve the ledger snapshot.

Chain fixtures compare **16 native genesis cases** and a **three-block extension
sequence** with two competing blocks at height 2. Genesis includes the actual
mainnet and testnet13 assets, relay failures, network/content mutations, timestamp
and difficulty wrapping, successful/failed result settlement and native
bootstrap exemptions. Extensions combine
original CPU space/VDF verification, header/body validation, contract writes,
signed transfers, rewards and settlement across hardforks 1 and 2. Rust tests
cover reopen, explicit rollback/replacement, archive/index consistency, parameter
binding, torn extension recovery, and failed commit preservation of pending state.
Additional native fixtures cover a correctly signed/proof-verified block with an
incorrect execution cost and a two-block alternate-root branch. Rust tests cover
out-of-order orphans, deferred work, restart, selected-index rollback, atomic
replay failures, deep forks, backwards-sync guards and torn branch replacement.
The fork-choice oracle compares **430 native graphs**, including wrapping votes,
unverified/invalid ancestors, differing root weights, height/hash ties and sync.
Voting adds **290 native cases**: 110 signature/hash cases, 101 validator-history
cases, 69 event sequences and ten signatures for real branch fixtures. Queue
sequences compare every vote count, publication and pending length across proof
verification, membership, duplicate votes, stronger proofs and timeout boundaries.
End-to-end tests switch a tied branch using an authenticated eligible vote,
preserve state on bad signatures, test transient votes across restart, and rewind
and replay every height of the fixture chain with all orphans retained.

The generated bindings also check **638 native C++ type/code hash pairs** and
**644 registry descriptors**, decode public native wire fixtures, round-trip
transactions/contracts, and call a generated MMX client through the VNX runtime.

Additional tests cover malformed/truncated input, invalid versions, atomic batch
validation, single-writer locking, checksum failures, every truncation point in a
final journal record, durable rollback/forks, execution failure, gas exhaustion,
read-only storage, and a counter across reopen/rollback.

To regenerate fixtures from an existing CMake build of the reference node:

```sh
python3 tools/generate_fixtures.py --source ~/workspace/mmx-node
python3 tools/generate_consensus_fixtures.py --source ~/workspace/mmx-node
# Optional: --suite hashes or --suite authorization
python3 tools/generate_transaction_fixtures.py --source ~/workspace/mmx-node
python3 tools/generate_block_fixtures.py --source ~/workspace/mmx-node
python3 tools/generate_header_fixtures.py --source ~/workspace/mmx-node
python3 tools/generate_work_fixtures.py --source ~/workspace/mmx-node
python3 tools/generate_vdf_fixtures.py --source ~/workspace/mmx-node
python3 tools/generate_nft_fixtures.py --source ~/workspace/mmx-node
python3 tools/generate_chain_fixtures.py --source ~/workspace/mmx-node
python3 tools/generate_network_fixtures.py --source ~/workspace/mmx-node
python3 tools/generate_fork_fixtures.py --source ~/workspace/mmx-node
python3 tools/generate_vote_fixtures.py --source ~/workspace/mmx-node
python3 tools/generate_sync_fixtures.py --source ~/workspace/mmx-node
python3 tools/generate_response_fixtures.py --source ~/workspace/mmx-node
python3 tools/generate_next_vdf_fixtures.py --source ~/workspace/mmx-node
# Optional: --build /path/to/mmx-node/build
```

The generator compiles a small oracle against that build's actual libraries. It
does not modify the reference source or use its database. Review fixture changes
alongside any semantic changes to the port.

## Next port stages

1. Sync throughput, peer scheduling/relay and specialized
   history indexes, building on the executable node and durable fork selection.
2. The Rust JavaScript-subset compiler and the remaining original contract suites.
3. Database compaction, cache/flush tuning and benchmarks,
   using an independent disk format while preserving logical state semantics.
4. Remaining node RPCs, wallet and block-production services, carrying over the
   relevant original tests as each component becomes executable.

Licensed under Apache-2.0, following the original MMX project. See [LICENSE](LICENSE).
