# Persistent services

Install services under the account that owns your MMX files. These systemd user
units start at boot when lingering is enabled, restart on exit, stop cleanly with
SIGINT, and send logs to the journal. Each process runs directly under systemd.

Enable lingering once on each host:

```sh
loginctl enable-linger "$USER"
```

If the host requires administrator authorization, use `sudo loginctl enable-linger
USERNAME`. Connect through a normal login session so `systemctl --user` can reach
the user manager. On SSH setups without a runtime environment, set
`XDG_RUNTIME_DIR=/run/user/$(id -u)` after the manager has started.

Use absolute executable paths. The installer validates and writes the unit,
reloads systemd, and optionally enables it; it does not stop or start processes.
Stop the corresponding old screen/manual process before starting its replacement.
Use `--replace` to update an existing installed unit.

For a node with existing native configuration, keep all Rust release binaries
together and let the node start its configured wallet/farmer/harvester children:

```sh
python3 scripts/systemd/install.py --name mmx-rust-node \
  --working-directory /path/to/mmx-node --environment MMX_HOME=/path/to/mmx-home/ \
  --environment RAYON_NUM_THREADS=8 --nice 19 --enable -- \
  /path/to/rust/bin/mmx-node --mmx-root /path/to/mmx-node \
  -c /path/to/mmx-node/config/mainnet /path/to/mmx-home/config/local \
  --data-dir /path/to/rust-ledger
systemctl --user start mmx-rust-node
```

For a remote harvester:

```sh
python3 scripts/systemd/install.py --name mmx-rust-harvester \
  --working-directory /path/to/mmx-node \
  --environment MALLOC_MMAP_THRESHOLD_=131072 --enable -- \
  /path/to/rust/bin/mmx-harvester --farmer FARMER_HOST:11333 \
  --plot-dir /path/to/plots --threads 16 --log-lookups
systemctl --user start mmx-rust-harvester
```

On GNU/glibc builds, this allocator setting returns large temporary proof buffers
to the OS after each lookup. It prevents idle memory retention across worker
heaps; it does not limit memory needed for active proofs. See
[memory measurements](../../docs/performance-parity.md#harvester-idle-memory).

The same installer supports separate wallet, farmer, timelord and SSH tunnel
services: choose a distinct `--name` and supply the command after `--`. Keep
private endpoints on loopback or a private Unix socket. Pass secrets through
private files rather than placing them in a unit's command line or environment.
Use `--after OTHER.service` and `--wants OTHER.service` for local dependencies;
remote services are reached through the normal reconnect/restart paths.
For a ledger on a separate disk, add `--require-mount /path/to/mount` so startup
retries without creating a new database on the root filesystem before that disk
is mounted.
`--require-free /path/to/data 8589934592` also requires 8 GiB of available space
before each start. Keep `check_storage.py` beside the installer when using this
option. These checks inspect mount/free-space metadata, without scanning data.

```sh
systemctl --user status mmx-rust-node
systemctl --user is-enabled mmx-rust-node
systemctl --user restart mmx-rust-node
systemctl --user stop mmx-rust-node
journalctl --user -u mmx-rust-node -f
loginctl show-user "$USER" -p Linger
```

Use GNU binaries on hosts that need OpenCL. Keep the ledger on its own configured
data path, separate from C++ storage. Plot mounts should be present before starting
a harvester; its configured reload interval controls discovery of later mounts.
