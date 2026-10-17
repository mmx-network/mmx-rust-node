# Network bootstrap fixtures

`config/default`, `config/mainnet`, `config/testnet13` and `data` are original
tracked configuration/assets from mmx-node commit
`ca6438b37c08007106d58efe4a990f6ecb276649`. Binary files are the original bytes,
not re-encoded Rust records. `layered` and `overlay` are synthetic configurations
for inheritance, leaf/JSON precedence, partial objects and array appends.

`../network.jsonl` records native C++ `vnx::read_config_tree`, generated
`ChainParams`/`Transaction` text decoding and current-schema serialization.
It covers full network configurations, defaults, wrapping signed/unsigned casts,
unknown keys, addresses, 128-bit amounts, enum text and execution results.
The independent genesis oracle in `../chain.jsonl` verifies the resulting full
bootstrap records, block hashes and ledger state.

Regenerate both original bytes and the network/text oracle with:

```sh
python3 tools/generate_network_fixtures.py --source ../mmx-node
```

Regeneration needs the native CMake test build; ordinary Rust tests need no C++
checkout or compiler.
