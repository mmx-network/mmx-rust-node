# MMX application branding

`mmx-logo.svg` is the canonical color icon: the selected **08 — Wide border**
design, with a violet gradient, larger white X, restrained halo, plain wide
border, and -9-degree rotation. It is a self-contained SVG without embedded
raster images, fonts, or external resources.

The halo is a separate filtered copy beneath the solid, unfiltered glyph so
the X stays sharp at navigation sizes. Avoid filtering the whole logo for
outer shadows; the rail uses a circular CSS box shadow instead.
The artwork has a four-unit transparent gutter so antialiasing at the coin's
rim stays inside the SVG viewport instead of being clipped at its edges.

`mmx-logo-compact.svg` is generated from the canonical SVG by `compact-logo.mjs`.
It retains the same glyph, gradients, border, padding and rotation, and omits
the halo, highlight mask and halo clip. `BrandMark` uses this version at 64 px
and below in the wallet and explorer; larger marks retain the full treatment.
The glyph-only variant remains available for monochrome interface decorations.
The website inserts the canonical SVG into its HTML for the header, footer,
wallet illustration and hero art, with unique IDs for each instance. Its small
placements use the same compact treatment; the hero retains the full SVG.

From `apps/wallet`, after installing dependencies and the local Playwright
Chromium runtime, regenerate the assets with:

```sh
node branding/render.mjs
```

This refreshes the compact SVG, compact favicons for all three apps, the website's full logo copy,
web/touch/maskable PNGs, padded desktop icon SVG/PNG, tracked Tauri PNG/ICO/ICNS
files, and the website's SVG/PNG social preview. Tauri's installed icon encoder
creates the desktop containers in a temporary directory; only the desktop
formats already tracked by the app are retained.

`brandJobs()` exports the same SVG inputs and output dimensions for an offline
SVG renderer. `desktopContainers()` encodes ICO/ICNS after `app-icon.png` has
been rendered. The source SVG remains the authority for all derived assets.

Earlier icon experiments are retained in `../../website/branding/` and can be
viewed on the website dev server at `/branding/`.
