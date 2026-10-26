# MMX Explorer

The public explorer, rebuilt with the new wallet's Obsidian design. The app
imports the wallet's actual theme, local fonts, icons and UI components; there
is no separate copy of the design system. It does not import wallet state,
signing code, Tauri or WASM.

Blocks, Transactions and Farmers are available as text links across the top on
desktop and mobile.

The Blocks overview shows average TPS over the latest 20 blocks, calculated
from the already-fetched transaction counts and observed block intervals.

## Develop and build

Node.js 22.18+ (24 recommended). From this checkout:

```sh
cd apps/explorer
npm ci --no-audit --no-fund
npm run dev                 # http://127.0.0.1:5175
npm run build               # static files in dist/
npm run preview             # preview the production build
```

The wallet's sources must be present beside this directory, as in this
repository. Its dependencies do not need to be installed. No Rust build or
ledger directory is required. For modest local resource use, prefix build/test
commands with `env GOMAXPROCS=2 UV_THREADPOOL_SIZE=2 nice -n 10 ionice -c 3`.

The chain server defaults to `https://rpc.mmx.network`, like the wallet and the
C++ explorer. Set `VITE_RPC_URL` at build time to use another public WebAPI:

```sh
VITE_RPC_URL=https://rpc.example.org npm run build
```

HTTPS is required; loopback HTTP is accepted for local development/tests. The
server must allow this site's origin with CORS. This app uses the public C++
REST/WebAPI paths (`/headers`, `/transactions`, `/farmer`, etc.); the Rust
node's `/rpc` JSON-RPC endpoint is a different interface.

## Navigation and paging

Existing `/#/explore/…` wallet links keep working. Search accepts block heights,
block hashes, transaction IDs, checksummed MMX addresses and farmer public keys.
A 64-character hash is checked with `/header` before opening a transaction;
server errors stay visible instead of being treated as a missing block.

Tables show 20 rows per page. Block history uses `offset` to load any older
height range, with the initial height stored in the URL so new blocks do not
shift subsequent pages. Genesis is fetched separately because the C++ headers
list excludes it. Back/forward, refresh and bookmarks retain the selected page.

Other lists page through the existing bounded response windows: the latest 100
transactions, top 200 farmers, latest 100 blocks per farmer, and approximately
200 address-history entries (C++ finishes the final block). They do not scan
the chain to emulate offset APIs that the public server does not expose. Any
older block or transaction can still be opened directly through search or a
block's transaction list. Individual block transactions, inputs, outputs,
balances, NFTs and reward addresses are paged locally too.

Automatic updates pause while browsing older result pages or while the tab is
hidden. Return to page one or use **Refresh data** to fetch fresh results.
The app retains only the current response, not a growing page/history cache.
Dark, light and system appearance settings are available in the top bar.

## Tests

```sh
npm test                    # unit regressions; at most two workers
npx playwright install chromium
npm run test:e2e            # production build + synthetic public API; one worker
```

The browser suite starts and stops its own servers on 5185/5186. Override
`EXPLORER_TEST_APP_PORT` and `EXPLORER_TEST_RPC_PORT` if needed. It never connects
to a live node or reads ledger/plot storage. The test build is in `dist/e2e`;
run `npm run build` again for a production build with the public chain server.

See [performance notes](../../docs/performance-parity.md#explorer) for native
request intervals, deliberate differences and remaining comparisons.

## Hosting

The [`mmx-apps` Docker image](../../docs/web-apps-docker.md) builds and serves
the explorer and wallet together on a configurable HTTP port behind a CDN.

Serve `dist/` as a static site. Hash routing needs no server-side route rewrite
and works under a subdirectory. The build emits a Content Security Policy into
`index.html` and a `_headers` file suitable for static hosts supporting that
format. Fonts and scripts are served locally.

For nginx or Caddy, translate the emitted headers into the host configuration,
including `frame-ancestors 'none'`. Cache hashed `/assets/*` files for a year,
serve `index.html` with `Cache-Control: no-cache`, and replace releases
atomically. Keep this explorer on its own origin. No server-side secrets,
database access or wallet keys are needed.
