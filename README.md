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
| `mmx-vm` | MMX value types and compact serialization, instruction metadata and compressed bytecode v1, interpreter with heap references and collections, gas accounting, call frames, read-only execution, and contract storage adapters. |

The interpreter executes 39 of the 48 recognized opcodes: copy/clear and cloning,
branches and local calls, unsigned 256-bit arithmetic, comparisons, logical and
bitwise operations, shifts, type/size queries, string/binary concatenation and
slicing, arrays/maps and entry access, SHA-256, and explicit failures.
Multiplication preserves both the legacy overflow rule and the protocol-version-1
fix.

Heap references, arrays, maps, and nested collections execute with allocation,
reference counting, aliasing, entry mutation, lazy storage loads, and recursive
cleanup. `COPY`, `CLR`, `SIZE`, and `CONCAT` include their collection paths.
`CLONE` allocates a copied value and returns a reference; references inside a
collection keep their aliases rather than being recursively deep-cloned.

The 9 remaining opcodes return explicit unsupported errors:

| Area | Opcodes |
| --- | --- |
| Conversion | `CONV` |
| Signature verification | `VERIFY` |
| Assets | `SEND`, `MINT`, `BALANCE` |
| Cross-contract access | `RCALL`, `CREAD` |
| Logs/events | `LOG`, `EVENT` |

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
address. Raw address bytes are used until the address presentation layer is ported.

## VNX integration

VNX serialization is being ported independently in `~/workspace/vnx-rust`.
This workspace does not implement VNX type codes, reflection, object envelopes,
RPC, or transport. MMX's `var_t` and symbol-compressed `instr_t` formats are
application-specific and live in `mmx-vm`. Connect to the VNX Rust crates once
their public API is available; do not duplicate that project's work here.

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

These fixtures validate the covered VM behavior, not complete consensus
compatibility. Extend coverage to host operations and transaction/block execution
as those layers are ported. Storage implementation changes must preserve the
same logical reads, state updates, and VM gas independently of physical I/O or
cache behavior.

Additional tests cover malformed/truncated input, invalid versions, atomic batch
validation, single-writer locking, checksum failures, every truncation point in a
final journal record, durable rollback/forks, execution failure, gas exhaustion,
read-only storage, and a counter across reopen/rollback.

To regenerate fixtures from an existing CMake build of the reference node:

```sh
python3 tools/generate_fixtures.py --source ~/workspace/mmx-node
# Optional: --build /path/to/mmx-node/build
```

The generator compiles a small oracle against that build's actual libraries. It
does not modify the reference source or use its database. Review fixture changes
alongside any semantic changes to the port.

## Next port stages

1. Conversions, crypto verification, host execution hooks, executable contracts,
   and the JavaScript-subset compiler and contract tests.
2. Transaction-scoped storage overlays and cross-contract execution, carrying
   over the remaining engine tests and extending failure/gas differential tests.
3. Scalable database indexing, compaction and flush scheduling, and benchmarks,
   using an independent disk format while preserving logical state semantics.
4. VNX-backed types/serialization, transactions, chain state, consensus,
   networking, RPC, and node services, carrying over the relevant original tests
   as each component becomes executable.

Licensed under Apache-2.0, following the original MMX project. See [LICENSE](LICENSE).
