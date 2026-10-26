# MMX apps Docker image

`mmx-apps` builds and serves the new web wallet and explorer. One nginx process
serves both sites on the same HTTP port, selected by the request's `Host` header:

| Hostname | App |
| --- | --- |
| `wallet.mmx.network` | `apps/wallet`, including its production Rust/WASM core |
| `explore.mmx.network` | `apps/explorer` |

TLS terminates at the CDN. The origin does not redirect HTTP to HTTPS or need
certificates. Visitors use HTTPS at the CDN, including for the wallet's browser
crypto and clipboard features. Browsers contact `https://rpc.mmx.network`
directly; this container only serves static files and has no node or database.

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
number instead. Set both CDN origins to the instance IP and assigned HTTP port,
preserving `wallet.mmx.network` and `explore.mmx.network` as their `Host` headers.

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

Replace `23147` with any port assigned to the instance. Both sites share that
port; configure each CDN origin as the instance's IP and this HTTP port.
The CDN must preserve the public `Host` header, or set it to the corresponding
hostname in the table. Unrecognized hosts return 404; `/healthz` returns 200
regardless of hostname for load balancers and the built-in health check.

Test each host locally:

```sh
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
or debug symbols, then builds both frontends sequentially at low priority.
Cargo/npm download caches persist in BuildKit; build tools, sources and
dependencies are absent from the small nginx runtime image. `.dockerignore`
limits the build context to source inputs and excludes local `.env` files,
generated WASM, desktop build output and dependency directories.

## Configuration

| Setting | When | Default | Purpose |
| --- | --- | --- | --- |
| `MMX_APPS_IMAGE` | Compose startup/build | `ghcr.io/mmx-network/mmx-apps:latest` | Image to pull, run or build; accepts a pinned tag/digest |
| `HTTP_PORT` | Compose startup | `8080` | Published host port within the instance's assigned slice |
| `HTTP_BIND` | Compose startup | `0.0.0.0` | Host interface for the published port |
| `PORT` | Container startup | `8080` | Internal HTTP listener, integer from 1024 to 65535 |
| `WALLET_HOST` | Container startup | `wallet.mmx.network` | Wallet hostname, without scheme, path or port |
| `EXPLORER_HOST` | Container startup | `explore.mmx.network` | Explorer hostname, without scheme, path or port |
| `VITE_RPC_URL` | Image build | `https://rpc.mmx.network` | Public chain WebAPI used by both apps |

Usually only `HTTP_PORT` needs changing. Platforms using host networking or
expecting a `PORT` variable can change the internal listener too:

```sh
docker run -d --name mmx-apps -e PORT=23147 -p 23147:23147 mmx-apps
```

Compose also accepts `PORT`, and maps `HTTP_PORT` to that listener. Hostnames
must be distinct. Changing runtime settings does not require rebuilding.
The chain server is embedded in JavaScript and each app's Content Security
Policy, so changing it requires a new image:

```sh
docker build --build-arg VITE_RPC_URL=https://rpc.example.org -t mmx-apps .
```

With Compose, set `VITE_RPC_URL` when running `docker compose build`. This URL
is public browser configuration. Use a public HTTPS REST/WebAPI server that
allows both app origins through CORS; the Rust node's JSON-RPC `/rpc` endpoint
is a different interface. The apps also accept loopback HTTP for local testing.

## CDN caching

Both apps emit content-hashed JavaScript, CSS and font filenames. The wallet's
WASM file is content-hashed too. nginx sends:

| Response | `Cache-Control` |
| --- | --- |
| Successful `/assets/*`, including 206/304 | `public, max-age=31536000, immutable` |
| HTML, favicon, web manifest and other unversioned files | `no-cache` |
| Errors, unknown hosts and health checks | `no-store` |

`no-cache` allows storage but requires revalidation before reuse. nginx supports
ETag/Last-Modified conditional requests. HTML and compressible assets are
precompressed during the image build; gzip clients receive `Content-Encoding:
gzip` and `Vary: Accept-Encoding`. Other clients receive the original files.
WASM is served as `application/wasm`.

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

## Verify

```sh
node --test deploy/apps/prepare.test.mjs
python3 deploy/apps/test-image.py mmx-apps
```

The smoke test starts disposable, non-root containers with read-only root
filesystems and ephemeral loopback host ports. It checks both default and
overridden hostnames/ports, security headers, successful/error/conditional cache
policies, gzip, WASM MIME/range responses, and rejection of invalid settings.
It makes no requests to a chain server and does not access ledger/plot storage.
The [web apps image workflow](../.github/workflows/apps.yml) runs the build and
these tests on relevant source changes.

## Add docs and website later

Add their build steps to the `web-build` stage and copy their static outputs
into separate directories under `/srv/mmx`. Add a `server` block for each
hostname in `deploy/apps/nginx.conf.template`; add policy/precompression inputs
to `prepare.mjs` if appropriate. All sites can share the existing HTTP port.
Keep separate hostnames and generated policies, especially for the wallet.
