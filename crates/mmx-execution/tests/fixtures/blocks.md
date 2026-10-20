These fixtures were generated against mmx-node
`ca6438b37c08007106d58efe4a990f6ecb276649` by
`tools/generate_block_fixtures.py`. The generator reuses the transaction harness,
extracts the original block `Node::validate`, `new_exec_context`, `prepare_context`,
dependency traversal, reward calculation and VDF winner selection methods, and
compiles them unchanged except for class qualifiers against the existing native
libraries. Native Block/BlockHeader methods, VM caches, signatures and transaction
validation/execution are used directly. The source tree and native data are not
modified.

The harness supplies original chain balances, contracts, indexes, the parent and
resolved VDF payout. Prefetch is a no-op. Thread tasks execute serially and wait/
signal methods are no-ops; native wait-map preparation remains intact and its
lock/predecessor sets are compared. Serial block order satisfies that graph.
NFT reward-target resolution is outside the harness. No full proof/VDF or chain
selection is performed. Rust's body API requires a separately verified header;
these cases exercise the shared body/economic checks, not unported header checks.

`blocks.jsonl` contains full snapshot data and original wires. Results record
acceptance/rejection, dependency locks/predecessors, every committed VM write,
ordered Block::get_inputs/get_outputs, balances and supply counters after the
native outputs-first/clamped-input settlement. Forged-result cases deliberately
compute transaction results against isolated caches, then submit them to the
actual block validator. Expected rejection ensures VM state/spending is shared.
New deployments and ledger outputs are not made visible during body validation.

`economics.tsv` columns are project ratio numerator/denominator, fixed project
reward, minimum reward, adjustment divisor/tick/interval, reward activation,
parent base reward/vote sum/vote count/fee buffer/fees, block height, proof
presence, fees, then expected project reward, buffer deduction, new buffer, new
base reward and final farmer reward. Inputs cover boundary values through
u64::MAX; wrapping arithmetic follows the compiled native implementation.

`vdf_winners.jsonl` contains a payout interval, ancestor header wires newest first
and an optional winner as hex (empty means absent). It uses the original winner
method over the supplied ancestry, including incomplete history and hash ties.

The six `parallel_groups_{8,2,1}_v{0,1}` cases were generated against native
`5626ac2b7cd26aa9626ea3b2cf864b4cfe6fbcd2`. Each runs eight transactions with
64 SHA256 VM operations, persistent writes and state-dependent minting, spread
over eight independent contracts, two interleaved dependency chains, or one
contended contract. Later calls start after initialization and must observe the
preceding committed VM state. Their C++ serial outputs and dependency graphs
are compared with Rust's parallel and retained original serial paths.
