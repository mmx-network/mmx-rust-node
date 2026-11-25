# MMX application branding

`mmx-logo.svg` is the canonical color icon. It uses the **08 — Wide border**
geometry with a violet face, inset rim, close shadow, and light glyph bloom. It is a
self-contained SVG without embedded raster images, fonts, or external resources.

The shadow and bloom sit beneath the glyph. The artwork has a four-unit transparent
gutter so antialiasing at the rim stays inside the SVG viewport. The website
hero and wallet rail use circular CSS box shadows around the mark.

`mmx-logo-compact.svg` is generated from the canonical SVG by `compact-logo.mjs`.
It uses a slightly heavier glyph and solid rim strokes to keep the mark readable
at 64 px and below. Favicons use a single rim and glyph positions fitted to a
16 px grid. Larger marks retain the shadow and bloom.
The glyph-only variant remains available for monochrome interface decorations.
The website inserts the canonical SVG into its HTML for the header, footer,
wallet illustration and hero art, with unique IDs for each instance. Its small
placements use the same compact treatment; the hero retains the full SVG.

From `apps/wallet`, after installing dependencies and the local Playwright
Chromium runtime, regenerate the assets with:

```sh
node branding/render.mjs
```

This refreshes the compact SVG, favicons for all three apps, the website's full
logo copy, web/touch/maskable PNGs, padded desktop icon SVG/PNG, tracked Tauri
PNG/ICO/ICNS files, and the website's SVG/PNG social preview. Tauri encodes the
full and compact desktop artwork in a temporary directory. The packaged ICO and
ICNS files use compact frames through 64 px and full artwork at larger sizes.

`brandJobs()` exports the same SVG inputs and output dimensions for an offline
SVG renderer. `desktopContainers()` encodes ICO/ICNS once `app-icon.png` and
`window-icon.png` are available. The source SVG remains the authority for all
derived assets.

Earlier icon experiments are retained in `../../website/branding/` and can be
viewed on the website dev server at `/branding/`.
