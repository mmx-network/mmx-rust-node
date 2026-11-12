# MMX apps Docker image

`mmx-apps` builds and serves the MMX website, new web wallet and explorer. One
HAProxy frontend enforces shared connection/request limits, then nginx serves
all three sites through a private Unix socket, selected by the request's
`Host` header:

| Hostname | App |
| --- | --- |
| `mmx.network`, `www.mmx.network` | `apps/website`, the public MMX website |
| `wallet.mmx.network` | `apps/wallet`, including its production Rust/WASM core |
| `explore.mmx.network` | `apps/explorer` |

TLS terminates at the CDN. The origin does not redirect HTTP to HTTPS or need
certificates. Visitors use HTTPS at the CDN, including for the wallet's browser
crypto and clipboard features. Browsers contact `https://rpc.mmx.network`
directly; the website queries the public GitHub releases API only on desktop
wallet download intent, selecting published `wallet-v` releases independently
of node releases. This container only
serves static files and has no node or database. The website links to the wallet
at `https://wallet.mmx.network`.

## Pull and run on Gigahost

The published image is `ghcr.io/mmx-network/mmx-apps:latest` (Linux amd64).
No source checkout or build tools are needed on the server:

Public packages can be pulled without a login. If the package is private, run
`docker login ghcr.io` first, or configure a registry credential on Gigahost:
your GitHub username and a classic personal access token with `read:packages`.
GitHub initially creates packages as private; an administrator can change this
in the [package settings](https://github.com/orgs/mmx-network/packages/container/mmx-apps/settings).

```sh
docker pull ghcr.io/mmx-network/mmx-apps:latest
docker run -d --name mmx-apps --restart unless-stopped \
  --read-only --tmpfs /tmp:rw,noexec,nosuid,size=16m \
  --cap-drop ALL --security-opt no-new-privileges:true \
  -p 23147:8080 ghcr.io/mmx-network/mmx-apps:latest
```

Replace `23147` with a port assigned to your instance. In Gigahost's container
settings, use `ghcr.io/mmx-network/mmx-apps:latest` as the image, expose container
HTTP port `8080`, and map it to the assigned host port. If the platform expects
the application to listen on the assigned port directly, set `PORT` to that
number instead. Set each CDN origin to the instance IP and assigned HTTP port,
preserving its public hostname as the `Host` header.

With this repository's Compose file, pull and start without building:

```sh
docker compose pull
HTTP_PORT=23147 docker compose up -d --no-build
```

The workflow publishes `latest` and `sha-<full commit SHA>` tags after both
default/custom-RPC image smoke tests pass on `main`. Pull requests and other
branches only validate. To pin a release, use its SHA tag or digest, including
through `MMX_APPS_IMAGE` with Compose. To update, pull again and recreate the
container; with Compose, repeat the two commands above.

On platforms that import an image into a VM disk, restarting the instance can
reuse the old imported image. Select the new SHA tag and redeploy/reimport the
image when applying an update.

For an existing package, link `mmx-network/mmx-rust-node` under its repository
source and grant this repository write access under **Manage Actions access**
in the package settings so the workflow can publish with `GITHUB_TOKEN`.

## Build from source

From the repository root (Docker with BuildKit and Compose required):

```sh
git submodule update --init --recursive
nice -n 19 ionice -c 3 docker compose build
HTTP_PORT=23147 docker compose up -d
```

The `vendor/vnx-rust` submodule is needed for Cargo workspace resolution.
The sibling C++ repository is not needed to build or run this image.

Replace `23147` with any port assigned to the instance. All three sites share that
port; configure each CDN origin as the instance's IP and this HTTP port.
The CDN must preserve the public `Host` header, or set it to the corresponding
hostname in the table. Unrecognized hosts return 404; `/healthz` returns 200
regardless of hostname for load balancers and the built-in health check.

Test each host locally:

```sh
curl -I -H 'Host: mmx.network' http://127.0.0.1:23147/
curl -I -H 'Host: www.mmx.network' http://127.0.0.1:23147/
curl -I -H 'Host: wallet.mmx.network' http://127.0.0.1:23147/
curl -I -H 'Host: explore.mmx.network' http://127.0.0.1:23147/
```

Without Compose:

```sh
nice -n 19 ionice -c 3 docker build -t mmx-apps .
docker run -d --name mmx-apps --restart unless-stopped \
  --read-only --tmpfs /tmp:rw,noexec,nosuid,size=16m \
  --cap-drop ALL --security-opt no-new-privileges:true \
  -p 23147:8080 mmx-apps
```

Docker builds the wallet core with two Rust jobs, no incremental compilation
or debug symbols, then builds all three frontends sequentially at low priority.
Cargo/npm download caches persist in BuildKit; build tools, sources and
dependencies are absent from the small HAProxy/nginx runtime image. `.dockerignore`
limits the build context to source inputs and excludes local `.env` files,
generated WASM, desktop build output and dependency directories.

Error and request logs use the inherited standard error stream. nginx does not
reopen `/dev/stdout`, `/dev/stderr` or `/var/log/nginx/error.log`, which can be
inaccessible after a VM supervisor opens root-owned console pipes and drops to
the image's `nginx` user. The image stays non-root and supports a read-only root
filesystem.

## Configuration

| Setting | When | Default | Purpose |
| --- | --- | --- | --- |
| `MMX_APPS_IMAGE` | Compose startup/build | `ghcr.io/mmx-network/mmx-apps:latest` | Image to pull, run or build; accepts a pinned tag/digest |
| `HTTP_PORT` | Compose startup | `8080` | Published host port within the instance's assigned slice |
| `HTTP_BIND` | Compose startup | `0.0.0.0` | Host interface for the published port |
| `PORT` | Container startup | `8080` | Internal HTTP listener, integer from 1024 to 65535 |
| `WALLET_HOST` | Container startup | `wallet.mmx.network` | Wallet hostname, without scheme, path or port |
| `EXPLORER_HOST` | Container startup | `explore.mmx.network` | Explorer hostname, without scheme, path or port |
| `WEBSITE_HOST` | Container startup | `mmx.network` | Website hostname, without scheme, path or port |
| `WEBSITE_WWW_HOST` | Container startup | `www.mmx.network` | Website alias, without scheme, path or port |
| `TRUSTED_PROXY_CIDRS` | Container startup | Empty (no trusted proxies) | Space/comma-separated proxy IPs/CIDRs allowed to supply visitor IPs |
| `CLIENT_IP_HEADER` | Container startup | `X-Forwarded-For` | Single-IP header overwritten by the trusted proxy |
| `VITE_RPC_URL` | Image build | `https://rpc.mmx.network` | Public chain WebAPI used by both apps |

Usually only `HTTP_PORT` needs changing. Platforms using host networking or
expecting a `PORT` variable can change the internal listener too:

```sh
docker run -d --name mmx-apps -e PORT=23147 -p 23147:23147 mmx-apps
```

Compose also accepts `PORT`, and maps `HTTP_PORT` to that listener. All four
hostnames must be distinct, ignoring case. Changing runtime settings does not
require rebuilding. The website's wallet, explorer and canonical public links
continue to point at the production domains when runtime hosts are overridden.
The chain server is embedded in JavaScript and each app's Content Security
Policy, so changing it requires a new image:

```sh
docker build --build-arg VITE_RPC_URL=https://rpc.example.org -t mmx-apps .
```

With Compose, set `VITE_RPC_URL` when running `docker compose build`. This URL
is public browser configuration. Use a public HTTPS REST/WebAPI server that
allows both app origins through CORS; the Rust node's JSON-RPC `/rpc` endpoint
is a different interface. The apps also accept loopback HTTP for local testing.

## Wallet website connections

[Website connections](wallet-connect.md) are disabled by default in the wallet.
The image serves `/connect.html` with COOP `unsafe-none` so an external website
can communicate with its popup; the main wallet keeps COOP `same-origin`.
`/wallet-sdk.js` is a small ESM loader for the content-hashed SDK under
`/assets/sdk/`. All SDK paths have CORS `*`; the hashed code is immutable and
cached for a year. The popup and
loader revalidate on every load. Preserve these path-specific headers in CDN
overrides.

## Connection and request limits

Each image instance enforces the public RPC defaults, shared across the website,
wallet, explorer, aliases, unknown hosts and health checks:

| Scope | Concurrent incoming connections | Requests per 10 seconds |
| --- | ---: | ---: |
| Entire instance | 4,096 | — |
| IPv4 address | 10 | 100 |
| IPv6 /64 | 10 | 100 |
| IPv6 /48, additionally | 100 | 1,000 |

HAProxy admits direct TCP clients before reading HTTP, including sockets that
have sent no request. Excess connections close immediately. Request budgets
use rolling `http_req_rate(10s)` counters; excess requests return `429`,
`Retry-After: 10` and `Cache-Control: no-store`. Denied requests count toward
their enclosing prefix's request budget too. Restarting an instance resets its
counters; instances do not share a cluster quota.

Completely idle sockets close after 15 seconds. Once HTTP starts, headers have
a 5-second deadline. Request bodies have a separate absolute 5-second deadline,
including when bytes keep arriving; the existing 1 KiB body limit still applies
to both fixed-length and chunked uploads. Every HTTP response has
`Connection: close`, and its socket closes after the response. The origin is
HTTP/1 only; CDN-to-browser TLS and connection policies remain at the CDN.

By default, counters use the incoming socket's source IP and ignore all
forwarded-client headers. A CDN, NAT or proxy therefore shares a source budget.
To use original visitor IPs, set `TRUSTED_PROXY_CIDRS` to the exact proxy ranges
and `CLIENT_IP_HEADER` to a header that those proxies **overwrite** with one
validated visitor IP. For example, for an operator-controlled proxy:

```sh
TRUSTED_PROXY_CIDRS='192.0.2.10/32 2001:db8:ffff::/48' \
CLIENT_IP_HEADER=X-Forwarded-For docker compose up -d
```

Replace the example ranges with the actual proxy sources visible to the
container. Never trust all addresses or a network containing direct callers.
Missing, malformed or multi-address visitor headers from a trusted proxy return
400. Untrusted callers cannot choose their budget with forged headers. The
private origin socket accepts normalized client metadata only from HAProxy;
nginx logs the resulting address through its existing inherited console stream.

For a trusted HTTP proxy, the global cap still counts real incoming TCP sockets.
Visitor concurrency can be attributed only after its HTTP headers arrive, so
it counts active origin requests/connections for that visitor and returns 429
on overflow. An origin cannot count or close visitor connections held at the
CDN edge; configure edge policies there when those limits are required too.

HAProxy and nginx run as the existing non-root user. A small supervisor stops
both when either exits, and tini forwards shutdown signals and reaps workers.
Nginx's backend listens only on `/tmp/mmx-apps/nginx.sock`; the exposed port and
health check stay the same. The runtime still supports a read-only root with
the existing 16 MiB `/tmp` tmpfs. Budget container memory and file descriptors
for the intended concurrent load; the connection caps are admission limits,
not a memory reservation.

## CDN caching

All sites emit content-hashed JavaScript, CSS, font, favicon and touch-icon
filenames. Manifest icons and the website's synchronous theme bootstrap are
content-hashed too. The shared build plugin rewrites the HTML and manifest
references to these immutable `/assets/` URLs. Manifests keep their original
URLs so relative app start URLs, scopes and installed app identities stay stable.
The wallet's WASM file is content-hashed too. nginx sends:

| Response | `Cache-Control` |
| --- | --- |
| Successful `/assets/*`, including 206/304 | `public, max-age=31536000, immutable` |
| HTML, web manifest and unversioned fallback files | `no-cache` |
| Errors, unknown hosts and health checks | `no-store` |

`no-cache` allows storage but requires revalidation before reuse. nginx supports
ETag/Last-Modified conditional requests. HTML and compressible assets are
precompressed with gzip level 6 during the image build; gzip clients receive
`Content-Encoding: gzip` and `Vary: Accept-Encoding`. This also applies to CDN
and reverse-proxy requests carrying `Via`, using `gzip_proxied any`. The image
serves the precompressed files without per-request compression work. Clients
that do not accept gzip receive the original files. WASM is served as
`application/wasm`.

Original icon and theme-script URLs remain available for older pages and
automatic icon discovery, with revalidation. New pages use the fingerprinted
URLs; changing the source bytes changes the URL without requiring a CDN purge.

Configure the CDN to honor these cache headers, with the hostname in its cache
key. Do not override HTML with a long edge/browser TTL, apply a blanket cache
rule to errors, or override the app security headers. Turn off HTML/JavaScript
rewriting, script injection and third-party analytics for the wallet. The image
translates each app's generated `_headers` policy into nginx configuration at
build time, including the selected RPC origin and `frame-ancestors 'none'`.

Deploy new images atomically by replacing the container. Hashed assets need no
purge; HTML revalidation discovers the new release. Each image contains one
release: already-open tabs may need a refresh if they request an old lazy-loaded
chunk that is no longer at the origin and was never cached by the CDN. For
uninterrupted older sessions, retain previous hashed assets in CDN/object
storage across deployments. Keep the previous image tag for rollback.

## Search discovery

Each site serves its own `/robots.txt` and `/sitemap.xml`. Crawlers can access
the homepage and all rendering assets; only `/healthz` is excluded. Sitemaps
list the canonical HTTPS homepage on each production domain, including when
runtime hostname overrides are used for testing. The website's `www` alias
declares `https://mmx.network/` as canonical as well.

All three homepages include descriptive titles and descriptions, canonical
links, Open Graph/social-card metadata, and `WebSite` structured data for their
site names. Structured data uses HTML microdata, so no inline scripts or CSP
exceptions are needed. The public website's content is already static HTML;
the wallet and explorer also provide descriptions and resource links when
JavaScript is disabled.

The wallet's screens and explorer's block, transaction, address and farmer
views use URL fragments. Only the app homepages are listed in their sitemaps;
fragments are not separate indexable documents. Making individual public
explorer records searchable would require regular URL routes, crawlable
record content, per-record metadata, and meaningful HTTP status codes. It is
separate from homepage discovery.

After deploying the image, verify the production domains in Google Search
Console and submit these sitemaps:

- `https://mmx.network/sitemap.xml`
- `https://wallet.mmx.network/sitemap.xml`
- `https://explore.mmx.network/sitemap.xml`

Use URL Inspection to check each homepage's rendered content and canonical URL
and request recrawling. Ensure the CDN passes through crawler files and does
not challenge search crawlers, require login, or add `noindex` headers. HTTPS
and HTTP-to-HTTPS redirects are configured at the CDN, which terminates TLS.

## Verify

```sh
node --test deploy/apps/prepare.test.mjs
nice -n 19 ionice -c 3 docker build --target limits-test .
python3 deploy/apps/test-image.py mmx-apps
```

The smoke test starts disposable, non-root containers with read-only root
filesystems and ephemeral loopback host ports. It checks all three sites,
including the website's `www` alias, default and overridden hostnames/ports,
separate site content and policies, security headers, successful/error/conditional cache
policies, gzip, WASM MIME/range responses, and rejection of invalid settings.
It also checks each site's robots rules, sitemap XML, canonical and social
metadata, and structured site name on both production and overridden hosts.
It also starts nginx with root-owned inherited console pipes and verifies both
HTTP delivery and request logging after the supervisor drops privileges.
It makes no requests to a chain server and does not access ledger/plot storage.
The [web apps image workflow](../.github/workflows/apps.yml) runs the build and
these tests on relevant source changes.

The `limits-test` build target uses the image's actual HAProxy/nginx binaries
and configs to check IPv4 and IPv6 /64 and /48 aggregation, the full 4,096-socket
budget, shared request rates, forwarded-header spoofing, trusted proxy identity,
socket closure and trickled uploads. Its Python dependency and fixtures are
absent from the final runtime image. Its isolated loopback source fixture uses
PROXY headers to generate IPv6 identities; no shipped listener accepts PROXY
protocol from callers. The normal image smoke tests honor 429 backoff during
their deliberately large batches of content checks.

## Add another static site

Add its build steps to the `web-build` stage and copy its static output
into a separate directory under `/srv/mmx`. Add a `server` block for its
hostname in `deploy/apps/nginx.conf.template`; add policy/precompression inputs
to `prepare.mjs` if appropriate. All sites can share the existing HTTP port.
Keep separate hostnames and generated policies, especially for the wallet.
