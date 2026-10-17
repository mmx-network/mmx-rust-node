# MMX Rust node

A Rust port of [`mmx-node`](https://github.com/madMAx43v3r/mmx-node), starting
with the smart contract engine and versioned database. This is an early library
workspace; networking, consensus, wallet services, and a node executable are still
to be ported.

The behavioral reference is `../mmx-node` (`~/workspace/mmx-node`), currently at
`ca6438b37c08007106d58efe4a990f6ecb276649`. Port consensus behavior from the
reference implementation and its tests, including protocol-specific differences.
Consensus compatibility is required: identical transaction/block acceptance,
execution results, state transitions, gas and limits, failure behavior, and
protocol-version rules. The database's physical disk format is independent and
does not need to match C++. Do not silently replace unsupported behavior with
approximations.

## Build and run

Requires Rust 1.89 or newer. The disk backend currently targets Unix filesystems
with file locking, directory sync, and file sync support.

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
```

The example executes MMX instructions, charges gas, stages contract state, and
commits it to disk. Running it again reopens the database and increments the same
counter. It does not require the C++ repository or a running MMX node.

## Workspace

| Crate | Implemented |
| --- | --- |
| `mmx-db` | Named versioned tables, size-first key ordering, inclusive historical reads, snapshot scans, atomic batches, rollback, and a durable journal with exclusive writer locking, CRC32 checks, guarded record lengths, and torn-tail recovery. |
| `mmx-types` | Generated MMX/VNX wire types, type registry, typed RPC clients and service dispatchers, built from the VNI snapshot using the VNX submodule. |
| `mmx-consensus` | Native hashes/costs and authorization, header authentication, CPU proof-of-space verification, difficulty/challenge/weight transitions and ancestor lookup. |
| `mmx-crypto` | Public-key address hashing and native ECDSA verification, with one signature cache shared by authorization and VM `VERIFY`. |
| `mmx-execution` | Contract loading/calls and constructors; transaction validation plus block-body commitments, reservations, shared execution, fee ordering, rewards/supply and deferred atomic settlement. |
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
changes durable. This split allows a future transaction/block executor to decide
when state becomes durable. For now, use a fresh engine per invocation and let the
caller serialize access to the storage backend.

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
visited ancestor's proof hash. VDF work/output linkage, NFT reward-target
resolution, archival indexes, genesis initialization and chain selection remain
to be ported. A successful body validation is not a complete block acceptance
decision.

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
proofs/transitions. Both APIs still require VDF work/output linkage and NFT
reward-target verification before accepting a block; the iteration-count check
alone does not establish VDF work.

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
Variant bytes and raw strings; it is used to persist deployments without a JSON
round trip. It remains distinct from hash serialization.

The disk backend uses a **new Rust journal format**, not the C++ database files.
Use a separate data directory. It retains all history in memory and replays the
journal on open; scalable indexing, compaction, and flush scheduling are later
work. Reproducing the C++ block/index file format is not a port requirement.
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
# Optional: --build /path/to/mmx-node/build
```

The generator compiles a small oracle against that build's actual libraries. It
does not modify the reference source or use its database. Review fixture changes
alongside any semantic changes to the port.

## Next port stages

1. VDF work/output linkage, NFT reward targets, genesis, archival indexes and
   durable chain-state application/reorgs,
   building on block-body validation and atomic settlement.
2. The Rust JavaScript-subset compiler and the remaining original contract suites.
3. Scalable database indexing, compaction and flush scheduling, and benchmarks,
   using an independent disk format while preserving logical state semantics.
4. Chain selection, networking, RPC and node services using
   the generated types and VNX runtime, carrying over the relevant original tests
   as each component becomes executable.

Licensed under Apache-2.0, following the original MMX project. See [LICENSE](LICENSE).
