# MMX Rust node

Run an MMX full node, serve RPC requests, and farm existing MMX plots with Rust.
This repository includes the node, wallet, farmer, local and remote harvester,
timelord, and command-line tools.

Mainnet sync and solo farming are implemented. The port is still under
active development; review the [current limitations](#current-limitations)
before switching an existing installation.

## Build

Use Linux with Rust **1.89 or newer**, Git, and a C build toolchain. On
Debian/Ubuntu, the build dependencies are `build-essential` and `pkg-config`.
The database needs a local filesystem with Unix file locking and sync support.
A GPU is optional; install its vendor's OpenCL runtime to enable acceleration.

Clone both repositories side by side. The C++ repository supplies the public
network configuration and genesis assets; you do not need to build C++.

```sh
git clone --recurse-submodules https://github.com/mmx-network/mmx-rust-node.git
git clone --depth 1 https://github.com/mmx-network/mmx-node.git
cd mmx-rust-node

CARGO_INCREMENTAL=0 CARGO_PROFILE_RELEASE_DEBUG=0 \
  cargo build --release --locked --jobs 2 -p mmx-node -p mmx-tools -p mmxwallet

export PATH="$PWD/target/release:$PATH"
```

For an existing checkout, run `git submodule update --init --recursive` before
building. Keep the binaries together: the node starts configured services from
its own executable directory.

| Command | Purpose |
| --- | --- |
| `mmx-node` | Full node, peer sync, and RPC server |
| `mmx-wallet` | Private wallet service for balances, spending, and farmer keys |
| `mmx-farmer` | Farmer signing service and harvester connections |
| `mmx-harvester` | Read plot files and submit proofs to a farmer |
| `mmx-timelord` | Compute and publish VDF proofs |
| `mmx` | Query native RPC services; `mmx wallet` runs the wallet CLI |
| `mmxwallet` | Standalone wallet CLI, also available as `mmx wallet` |

Run any command with `--help` for its options.

## Start a node

From the Rust repository directory:

```sh
mmx-node --mmx-root ../mmx-node --network mainnet \
  --data-dir ./mmx-data --no-services
```

Choose `--data-dir` on a fast local disk with room for the growing ledger.
**Use a separate directory from the C++ node:** the on-disk formats differ.
Existing wallet keys and plot files are compatible and can be reused.

The node discovers peers from the network configuration and syncs from genesis.
The C++ default Router config targets eight outgoing peers and requires five
distinct authenticated sync peers; `Router.num_peers_out` and
`Router.min_sync_peers` control those counts.
Historical sync intentionally uses outbound connections only. Incoming peers
still exchange gossip and use the restricted peer RPC service.
If discovery needs help, add `--peer HOST[:PORT]`; supplied peers replace the
configured seed list, and discovery can find more peers from them.
Discovery refreshes during operation, and authenticated outbound peer addresses
are saved for later starts. An existing cache can bootstrap the node without
seeds or fixed peers. Cache writes run in the background; see the
[router configuration reference](docs/development.md#router-configuration)
for storage and discovery controls.

Check progress from another terminal:

```sh
curl -sS http://127.0.0.1:11380/rpc \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"get_status","params":{}}'
```

The result includes the current height, `synced`, `sync_peers`, and
`earliest_height`. Farming starts receiving challenges once the node is synced.
Press Ctrl-C for a clean shutdown; restart with the same data directory to resume.

Peer listening follows the loaded native `Router.json`; RPC listeners use
loopback by default:

| Interface | Default address | Override |
| --- | --- | --- |
| Peer connections | `Router.host` / `Router.port`, mainnet port `11337` | `--p2p-listen` |
| Native VNX RPC | `127.0.0.1:11331` | `--rpc-listen` |
| HTTP JSON-RPC | `http://127.0.0.1:11380/rpc` | `--http-listen` |

The C++ default config sets `Router.host` to `0.0.0.0`; without that setting,
Rust retains its loopback fallback. `--p2p-listen 0.0.0.0:11337` also accepts
incoming mainnet peers when TCP port 11337 is reachable. The native
`Router.max_connections` default of 100 now governs inbound admission, counting
outbound and pending sockets too. See the [router configuration reference](docs/development.md#router-configuration)
for supported settings and remaining differences. Keep wallet and signing
endpoints private.

## Wallet and solo farming

An existing C++ `wallet.dat` and existing `.plot` files can be used directly.
To create a new wallet, choose a private location and back up its recovery words:

```sh
mmx wallet create --file /path/to/private/wallet.dat
mmx wallet address --file /path/to/private/wallet.dat
```

Wallet files contain the seed in plaintext. A wallet passphrase changes key
derivation; it does not encrypt the file. Keep key files and recovery words
private, and supply secrets through interactive prompts or `--input-stdin`.

Without `--file`, wallets live in `$MMX_HOME` or `~/.mmx` (Windows:
`%USERPROFILE%\.mmx`); `mmx wallet list` shows them and `mmx wallet use`
selects one. Each wallet uses 10 addresses unless changed with
`mmx wallet set-addresses <N>` (1 to 100) or `--num-addresses`, and
`mmx wallet rename <NAME>` names it. Both act on the selected wallet or the one
given with `--wallet <FINGERPRINT>`. These settings and an optional chain server
(`"rpc"`, used when `--rpc` is not given) are kept in `mmxwallet.json` in the
same directory.

The desktop wallet (Windows, macOS, Linux deb and AppImage) uses the same wallet
directory, key files and `mmxwallet.json`; the web wallet keeps the recovery
words in the browser's password manager. Both are built from
[`apps/wallet`](apps/wallet): `npm ci && npm run build:wasm`, then
`npm run build:web` for the static site (served with the headers in
[`apps/wallet/deploy`](apps/wallet/deploy/README.md)), or
`scripts/build-linux-container.sh`, `scripts/build-macos.sh` or
`scripts/build-windows.ps1` for the desktop packages.

The block explorer in [`apps/explorer`](apps/explorer/README.md) shares the new
wallet's design and preserves existing explorer links. Run `npm ci && npm run
build` in that directory to build its static site, or `npm run dev` to preview
it on port 5175. It includes paged blocks, transactions, farmers and address
activity, and talks directly to the public WebAPI without a local ledger.

The public website in [`website`](website/README.md) uses the new wallet's visual
style and links to the browser wallet at `https://wallet.mmx.network`.
To build and serve the website and both web apps behind a CDN, use the
[`mmx-apps` Docker image](docs/web-apps-docker.md). One HTTP origin routes
`mmx.network`, `www.mmx.network`, `wallet.mmx.network` and `explore.mmx.network`
by hostname; set `HTTP_PORT` with Compose to use a port from the host's assigned
slice.

For farming, restart the node with the private farmer endpoint configured:

```sh
mmx-node --mmx-root ../mmx-node --network mainnet \
  --data-dir ./mmx-data --no-services --farmer 127.0.0.1:11333
```

The node retries the farmer connection until the signer is available. Start each
service below in a separate terminal:

```sh
mmx-wallet --key-file /path/to/private/wallet.dat \
  --node 127.0.0.1:11331 --listen 127.0.0.1:11335
```

```sh
mmx-farmer --wallet 127.0.0.1:11335 --node 127.0.0.1:11331 \
  --listen 127.0.0.1:11333
```

```sh
mmx-harvester --farmer 127.0.0.1:11333 \
  --plot-dir /path/to/plots --threads 2 --log-lookups
```

The farmer uses the first available wallet account's address for rewards.
Set `--reward-address YOUR_MMX_ADDRESS` on the farmer to choose another address.
Repeat `--plot-dir` to add directories. `--threads` limits workers shared by
parallel plot reads and proof computation. For C10 plots, add `--max-f1 262144`
to permit their worst-case decompression work.

On GNU/glibc builds, prefix the harvester command with
`MALLOC_MMAP_THRESHOLD_=131072` to return large temporary proof buffers after
lookups. The [systemd example](scripts/systemd/README.md) includes this setting.

Inspect the services with:

```sh
mmx node get_height
mmx farmer get_farm_info
mmx harvester get_farm_info
mmx wallet-rpc get_address --args '{"index":0,"offset":0}'
```

The standalone `mmx wallet` / `mmxwallet` network commands use the C++ REST API.
For wallet operations against this Rust node, use the `mmx-wallet` service through
`mmx wallet-rpc`. Its arguments use native VNX JSON; see the
[service reference](docs/development.md#service-implementation-and-compatibility).

## Reuse an existing configuration

The services accept native VNX configuration directories with `-c`, including
parent inheritance and local overrides. Explicit command-line options take
priority over configuration values.

For an existing C++ installation, run from its directory with the Rust binary
directory on `PATH`:

```sh
export MMX_HOME=/path/to/existing-mmx-home/
cd /path/to/mmx-node
mmx-node --mmx-root . --data-dir /path/to/rust-ledger
```

This starts the configured wallet, farmer, and harvester alongside the node.
The native `wallet`, `farmer`, `harvester`, and `timelord` switches control service
startup; the timelord is off by default. Add `--no-services` to manage services
separately. Automatic service startup uses explicit `-c` directories or a local
configuration directory and is disabled in offline and sync-once modes.

The node and standalone services resolve paths like the C++ startup scripts:

| Setting | Behavior |
| --- | --- |
| `MMX_HOME` | Home for local configuration and wallets; defaults to the working directory. A trailing slash is optional. |
| `NETWORK` | Read from `MMX_HOME/NETWORK`, defaulting to `mainnet`; `--network` overrides it. |
| `MMX_DATA` | Base for network state. `MMX_HOME/config/local/MMX_DATA` takes priority over the environment, then defaults to `MMX_HOME`. |
| `MMX_NETWORK` | Explicit network state directory, overriding `MMX_DATA/<network>`. |
| `--mmx-root` | Installation containing public `config/` and `data/` assets, independent of `MMX_HOME`. |

Without `-c`, the node loads installation `config/<network>`, `config/node`, and
`MMX_HOME/config/local` in that order, skipping absent directories. Standalone
wallet, farmer/harvester and timelord services use their corresponding `wallet`,
`farmer` and `timelord` configuration layers. An explicit `-c` list replaces
automatic layer selection. These paths are resolved at startup; existing data
is not moved and the C++ password/config initialization scripts are not run.

Common settings include `Wallet.key_files`, `Farmer.reward_addr`,
`Harvester.plot_dirs`, `Harvester.num_threads`, and `Harvester.my_name`.
Relative wallet filenames resolve under the configured `Wallet.storage_path`
within `MMX_HOME`, with an empty storage path by default. Relative `TimeLord.storage_path` values resolve
under the network state directory. Absolute paths remain absolute.

The ledger uses `Node.database_path` (`db` by default) under the network directory.
`--data-dir` selects an explicit path, independent of those settings. **When
reusing a C++ home, supply a separate Rust ledger directory:** startup rejects a
C++ database before creating Rust files. Keep an existing Rust node's explicit
`--data-dir` to continue using its current ledger.
Stop services that already occupy the same ports before starting replacements.

## Remote harvesters

Run the wallet and node on the farmer host. Set the node's
`--farmer 127.0.0.1:11332` to match the private signing listener below, then
start the farmer with a separate remote harvester listener:

```sh
mmx-farmer --wallet 127.0.0.1:11335 --node 127.0.0.1:11331 \
  --listen 127.0.0.1:11332 --harvester-listen 0.0.0.0:11333
```

On each harvester host:

```sh
mmx-harvester --farmer FARMER_HOST:11333 \
  --plot-dir /path/to/plots --threads 2 --name harvester-1 --log-lookups
```

Only the farmer host needs wallet keys. The remote listener distributes
challenges and accepts proofs and farm reports; it cannot sign blocks or votes.
Allow TCP port 11333 from your harvesters. Use a trusted network or an SSH/VPN
tunnel: native VNX connections do not provide transport encryption.

With native configuration, `allow_remote=true` enables the remote listener.
`Farmer.private_endpoint` and `Farmer.harvester_endpoint` override its two
addresses. To require authentication, configure VNX users on the farmer, enable
`--require-harvester-login`, and give harvesters `--username USER` and
`--password-file /private/path/PASSWD`. Connections restore their subscriptions
after temporary disconnections.

When using the private listener on 11332, query it explicitly:

```sh
mmx farmer get_farm_info --endpoint 127.0.0.1:11332
```

## Timelord

A timelord is optional for running a node or farming. To compute VDF proofs:

```sh
mmx-timelord --node 127.0.0.1:11331 \
  --storage-path /path/to/timelord --reward-addr YOUR_MMX_ADDRESS \
  --listen 127.0.0.1:11336
```

Back up `timelord_sk.dat` in the chosen storage directory to preserve the
identity. Existing C++ timelord identities are supported. Use port 11336 as above
when running a harvester on the same host: both services otherwise default to
control port 11334. For automatic startup, set `TimeLord.listen` accordingly.

On Linux, the default `--cpu auto` selects an allowed SHA-capable Intel E-core
when available. `--cpu N` pins hashing to a logical CPU, and `--cpu any` keeps
normal scheduling. Other CPUs retain normal scheduling. Hashing uses SHA-NI,
ARM SHA2, or a portable CPU fallback.

Benchmark without connecting to a node or writing keys:

```sh
mmx-timelord --benchmark 2000000
```

## GPU acceleration

The node uses OpenCL GPU 0 for VDF verification, and the harvester uses it for
compressed plot proof computation, with automatic CPU fallback. CPU OpenCL
devices are skipped. Timelord hashing runs on the CPU.

On the node or harvester, `--opencl-device INDEX` selects another GPU and
`--opencl-device -1` forces CPU work. Use `--opencl-platform NAME` to filter
platforms. An OpenCL SDK is unnecessary for building; GPU use requires the
system OpenCL loader and vendor driver at runtime.

Use a GNU Linux build for GPU acceleration. The static `linux-x86_64-musl`
archives use CPU work because they cannot load vendor OpenCL drivers. GNU
archives target glibc 2.31 or newer, including Ubuntu 20.04. Check the service's
startup log for `Using OpenCL GPU`; `OpenCL unavailable; using CPU` means
acceleration is inactive.

## RPC and maintenance

HTTP uses JSON-RPC 2.0 at `/rpc`; native VNX RPC is available on port 11331.
For example:

```sh
curl -sS http://127.0.0.1:11380/rpc \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":2,"method":"get_block_hash","params":{"height":100}}'
```

The API includes blocks, headers, transactions, contracts, balances, transaction
validation, and submission. Parameters are named objects using native VNX JSON
representations, including byte arrays. This HTTP endpoint implements a subset
of node methods; C++ WebGUI and REST/WebAPI parity are still in progress.

To serve stored data without contacting peers, restart with `--offline`:

```sh
mmx-node --mmx-root ../mmx-node --data-dir ./mmx-data --offline
```

History is retained by default, allowing rollback to any retained height after a
fork. To reclaim disk space, stop the node and restart with a retention floor:

```sh
mmx-node --mmx-root ../mmx-node --data-dir ./mmx-data \
  --purge-before 5000000 --no-services
```

Choose a height already present in your database. The block and state at that
height remain available. Earlier blocks and transactions disappear from RPC,
and **rollback below the purge height becomes impossible**, including after
restart. A fork below that floor requires an unpurged database or a fresh sync.
Purging runs before listeners start and temporarily needs extra space while
retained data is collected. `get_status.earliest_height` reports the floor.

For a manual rewind, restart with `--rollback-to HEIGHT`. Combine it with
`--offline` to inspect that height without immediately resuming sync.

To update a source installation, stop its services, run `git pull --ff-only` and
`git submodule update --init --recursive`, repeat the release build above, then
restart with your existing configuration and data directory.

For automatic startup and restart after a reboot, use the
[systemd service installer](scripts/systemd/README.md). It supports the node,
separate wallet/farmer services, remote harvesters, timelords, and SSH tunnels.

## Current limitations

- Pool/NFT partial submission and remote proof recomputation remain unimplemented.
- Reverse farmer RPC routing for a C++ node is incomplete. Use the Rust node for
  the farming setup above.
- Wallet pending transactions and input reservations are kept in RAM for the
  wallet service's lifetime; durable pending history is still pending.
- Advanced wallet, offer/swap, and some contract withdrawal methods remain
  incomplete. The C++ REST API and WebGUI are not included.

For development, tests, compatibility details, and measured performance, see the
[development reference](docs/development.md),
[C++ port audit](docs/cpp-port-audit.md), and
[performance comparisons](docs/performance-parity.md).

Licensed under [Apache-2.0](LICENSE).
