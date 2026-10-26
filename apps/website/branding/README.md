# MMX icon studies

Open `http://127.0.0.1:5176/branding/` while the website dev server is running.
The page compares native SVG icons on light and dark backgrounds, including
16, 24, 36, and 58 px samples. Each SVG is self-contained, with no fonts,
embedded raster images, or external resources.

The baseline is `../public/mmx-logo.svg`, copied from the hero's colors,
140-degree base gradient, upper-left radial light, violet border, inset rim
highlight, glyph proportions, and -9-degree rotation. It is currently used by
the website's header, footer, and wallet illustration for review.

The latest round uses a 30% larger mark, filling about 81% of the coin's height
before rotation, compared with about 62% in the baseline:

- `larger-halo.svg`: a broad violet halo and brighter glow close to the glyph.
- `simple-coin.svg`: the stronger halo with one smooth, three-pixel beveled rim.
- `inset-coin.svg`: an engraved glyph, inner shadow and highlight, and the same
  simple rim.

The six earlier studies are retained in an expandable comparison:

- `coin-edge.svg`: a beveled rim and fine coin-edge marks.
- `violet-light.svg`: a brighter violet gradient.
- `x-halo.svg`: a violet halo behind the white glyph.
- `raised-x.svg`: a cast shadow beneath the glyph.
- `indigo-dusk.svg`: a cooler indigo gradient.

These studies are dev preview sources and are not entry points in the
production website build. Wallet/explorer branding, favicons, and installed
app icons retain their existing assets pending a selected direction.
