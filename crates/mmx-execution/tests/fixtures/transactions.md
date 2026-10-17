`transactions.jsonl` is generated from mmx-node
`ca6438b37c08007106d58efe4a990f6ecb276649` by
`tools/generate_transaction_fixtures.py`. The generator reads the original
`Node::validate` and both `Node::execute` definitions and compiles their bodies
unchanged against the existing native libraries. Only the class qualifier is
renamed. The harness supplies snapshot balances, contracts, transaction indexes,
explicit locks and native StorageCache/StorageRAM. No reference database, wallet,
running node or original source file is modified.

Base rows contain the native parameter/transaction wires, contracts, indexes,
balances and height. Subsequent rows name a `seed` and supply a mutated execution
result, native updated static cost and content hash. Common snapshot fields are
inherited. Supplied-result rows retain their original unsigned transaction ID
and signatures, as native Transaction::update does.

Accepted rows contain the native execution result (or use `submitted_result`),
VM writes, native Transaction::get_tx_index read cost, and a complete settled
balance map. Settlement follows the outputs-first, clamped-input rule in
Node::apply/Block::get_inputs for a single transaction. Rejected rows include
their native static/ordinary exception class and diagnostic text. State writes
and ledger changes are forbidden on rejection; charged failures retain only fee
settlement and the new transaction index.

Rust tests enforce consensus-relevant execution results and also compare line
information. Computed diagnostic text may differ and is checked for its native
160-byte limit; native consensus deliberately does not enforce message/line
matching. When a supplied result is accepted, its original diagnostic fields are
retained. Full block scheduling, block-wide funding reservations, rewards,
supply accounting and archival indexes are outside this harness/API.
