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

- `larger-halo.svg`: a restrained violet halo with the original thin border.
- `simple-coin.svg`: the same soft halo with a plain, six-pixel violet border.
- `inset-coin.svg`: pale lilac engraving with deeper inner shadows, a bright
  lower-edge highlight, and the same plain wide rim.

The six earlier studies are retained in an expandable comparison:

- `coin-edge.svg`: a beveled rim and fine coin-edge marks.
- `violet-light.svg`: a brighter violet gradient.
- `x-halo.svg`: a violet halo behind the white glyph.
- `raised-x.svg`: a cast shadow beneath the glyph.
- `indigo-dusk.svg`: a cooler indigo gradient.

These studies are dev preview sources and are not entry points in the
production website build. Wallet/explorer branding, favicons, and installed
app icons retain their existing assets pending a selected direction.
