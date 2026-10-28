# Public HTTP RPC deployment

Use the existing `mmx-node/www/rpc-server` Node.js load balancer, including its
`changeOrigin: true` fix. It forwards public requests directly to
`http://localhost:11380/wapi` and configured remote peers; no protocol translation
is needed. Copy `index.js`, `package.json` and `package-lock.json` from the same
reviewed source, write `remotes.json`, and run `npm ci --omit=dev`.

Run the Rust node as the `mmx` service account, keeping the existing data directory
and network identity. For a dedicated RPC server select:

```text
--http-listen 127.0.0.1:11380 --rpc-cache-mb 4096
```

The equivalent JSON setting is `Node.rpc_cache_size_mb: 4096`; CLI takes
precedence. Local VNX stays on loopback port 11331 and P2P uses port 11337.
Build releases with `MMX_NODE_COMMIT=$(git rev-parse HEAD)` in the environment
so `/node/info` reports the deployed source commit.

Run the Node.js service as `mmx`, with its working directory `/opt/mmx-rpc`,
`RPC_HOST=127.0.0.1`, `RPC_PORT=8081` and `NODE_ENV=production`. Use the host's
installed Node.js executable, restart on failure and enable it at boot. Match the
existing RPC hosts' service hardening (`NoNewPrivileges`, `PrivateTmp`,
`ProtectSystem=strict`, `ProtectHome=true`). `remotes.json` contains the other
public RPC hosts, for example eu1, th1 and th2 for eu2.

Caddy serves normal HTTP on port 8080:

```caddyfile
http://:8080 {
    reverse_proxy 127.0.0.1:8081
}
```

Validate this file before loading it. NAT forwarding is configured outside the
VM. TLS/DNS configuration and shared certificates are separate from this HTTP
listener; no TLS secrets need copying for port 8080.

Verify each hop with bounded point requests:

```sh
curl -i http://127.0.0.1:11380/wapi/node/info
curl -i -H 'x-health-check: true' http://127.0.0.1:8081/node/info
curl -sS http://127.0.0.1:8080/server/status
curl -sS http://127.0.0.1:8080/node/info
curl -sS http://127.0.0.1:11380/rpc -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"get_status","params":{}}'
```

During sync the direct REST health request returns 503. A health-check request
through the proxy stays local; regular requests may use healthy remote peers.
`get_status` remains available for local diagnostics and reports sync height,
peer count and cache byte limit (4,294,967,296 for 4096 MiB). Public readiness
requires sync plus 18 new blocks. Confirm increasing heights and stable service
restart counts after deploying; keep the previous binary/config for rollback.
