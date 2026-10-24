# Hosting the MMX web wallet

The web wallet is a static site. Recovery words stay in the user's browser and
password manager; the site only serves files and the browser talks directly to
the chain server.

## Build

Requirements: Node.js 22+, Rust with the `wasm32-unknown-unknown` target and
`wasm-bindgen-cli` 0.2.127.

```sh
cd apps/wallet
npm ci
npm run build:wasm
npm run build:web          # output: dist/web
```

The chain server is fixed at build time. Default: `https://rpc.mmx.network`.
To use another server:

```sh
VITE_RPC_URL=https://rpc.example.org npm run build:web
```

PowerShell:

```powershell
$env:VITE_RPC_URL='https://rpc.example.org'; npm run build:web
```

Web builds run in Vite mode `web`, so the variable can also be kept in
`apps/wallet/.env.web` or `.env.web.local` (`VITE_RPC_URL=https://…`).
`.env.production` is not read.

It must be HTTPS and must send `Access-Control-Allow-Origin` for the wallet's
origin (or `*`) on `GET` and `POST` responses. The build writes the
Content-Security-Policy for that server into `index.html` and
`dist/web/_headers`.

## Serve

Serve the contents of `dist/web` over HTTPS only, with these headers on every
response (ready-made in this directory):

| File | Server |
| --- | --- |
| `Caddyfile` | Caddy 2 |
| `nginx.conf` + `nginx-security-headers.conf` | nginx |
| `_headers` (also generated into `dist/web`) | Netlify, Cloudflare Pages |

The files here contain the policy for `https://rpc.mmx.network`. With a
different `VITE_RPC_URL`, copy the `Content-Security-Policy` line from the
generated `dist/web/_headers` into your server configuration.

`nginx.conf` works with the nginx packages of current distributions (for
example 1.22 on Debian 12, 1.24 on Ubuntu 24.04). nginx 1.25.1 and later warn
that the `http2` parameter of `listen` is deprecated; the warning is harmless,
or replace the parameter with a `http2 on;` line.

Requirements for any other server:

- Send every header listed in `_headers`, including `Content-Security-Policy`
  with `frame-ancestors 'none'` and `Strict-Transport-Security`.
- Serve `.wasm` as `application/wasm` and `.webmanifest` as
  `application/manifest+json`.
- Cache `/assets/*` for a long time (file names change with every build) and
  serve `index.html` with `Cache-Control: no-cache` so updates reach users
  immediately.
- Serve the site from its own origin (a dedicated host name). Do not add
  analytics, third-party scripts, fonts or a service worker, and do not inject
  anything into the pages (some CDNs offer this as "optimizations": turn them
  off).

## Update

Build the new version and replace the served files in one step (for example by
switching a symlink to the new directory). Old asset files can be removed after
a day.

## Check

```sh
curl -sI https://wallet.example.org/ | grep -i -E 'content-security|strict-transport|x-frame|permissions'
```

Then open the site, open the browser's developer console and confirm there are
no errors.
