# MMX website

The public website for `mmx.network` and `www.mmx.network`, recreated from
`mmx-website` with the new web wallet's Obsidian visual style. It uses the
wallet's MMX branding, Space Grotesk, Inter, and JetBrains Mono, with dark and
light schemes. Fonts and assets are self-hosted; no chain API or third-party
scripts are needed. The website links to `https://wallet.mmx.network`.

## Develop and build

Use Node.js 22.18 or newer, from this directory:

```sh
npm ci
npm run dev
```

The local URL is `http://127.0.0.1:5176`. To check the production build:

```sh
nice -n 19 ionice -c 3 npm run build
npm run preview
```

Vite emits `dist/` with content-hashed CSS, JavaScript, and fonts. The page's
content and navigation work without JavaScript; the small script enables the
theme switch, follows the system scheme until a choice is made, and remembers
that choice when browser storage is available. The wallet preview is an
illustration of the welcome screen, not an embedded wallet.

For the SVG icon experiments, open `/branding/` on the dev server. The
[icon studies](branding/README.md) include the hero treatment and variants
with different gradients, borders, or glyph lighting, shown at several sizes.

## Content and deployment

Edit `index.html` for content and links, `src/style.css` for presentation, and
`src/main.js` for theme behavior. The network description follows the C++ node
README and MMX documentation; the wallet description follows `docs/wallet-app.md`.
The copy distinguishes consensus-driven supply from a guarantee of price
stability and avoids presenting potential transaction capacity as measured
throughput. The node download link continues to point to the published C++
node releases; Rust source and the new web apps are linked separately. The
existing SafeTrade and NonKYC market links are retained from `mmx-website`.

`public/` includes crawler metadata, the manifest, the wallet's branding/icons,
and the original website's social preview image. `public/_headers` is the
production policy source; its CSP is also embedded in the built HTML. There
are no API connections, inline scripts, or inline styles. Only fingerprinted
`/assets/` files receive immutable caching in the image.

The root Dockerfile builds this site into the shared `mmx-apps` image. nginx
selects it using `WEBSITE_HOST` and `WEBSITE_WWW_HOST` on the same HTTP port as
the wallet and explorer, with a separate security policy. Unknown files return
404. See [the deployment guide](../../docs/web-apps-docker.md) for origin/CDN
configuration and image verification.
