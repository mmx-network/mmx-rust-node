# MMX application branding

`mmx-logo.svg` is the canonical color icon: the selected **08 — Wide border**
design, with a violet gradient, larger white X, restrained halo, plain wide
border, and -9-degree rotation. It is a self-contained SVG without embedded
raster images, fonts, or external resources.

`BrandMark` imports this SVG for colored icons in the wallet and explorer. The
glyph-only variant remains available for monochrome interface decorations.
The website uses a generated copy for its header, footer, wallet illustration,
and hero art.

From `apps/wallet`, after installing dependencies and the local Playwright
Chromium runtime, regenerate the assets with:

```sh
node branding/render.mjs
```

This refreshes the favicon SVGs for all three apps, the website's logo copy,
web/touch/maskable PNGs, padded desktop icon SVG/PNG, tracked Tauri PNG/ICO/ICNS
files, and the website's SVG/PNG social preview. Tauri's installed icon encoder
creates the desktop containers in a temporary directory; only the desktop
formats already tracked by the app are retained.

`brandJobs()` exports the same SVG inputs and output dimensions for an offline
SVG renderer. `desktopContainers()` encodes ICO/ICNS after `app-icon.png` has
been rendered. The source SVG remains the authority for all derived assets.

Earlier icon experiments are retained in `../../website/branding/` and can be
viewed on the website dev server at `/branding/`.
