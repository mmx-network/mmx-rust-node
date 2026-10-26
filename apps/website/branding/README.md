# MMX icon studies

Open `http://127.0.0.1:5176/branding/` while the website dev server is running.
The page compares native SVG icons on light and dark backgrounds, including
16, 24, 36, and 58 px samples. Each SVG is self-contained, with no fonts,
embedded raster images, or external resources.

The selected design is **08 — Wide border** (`simple-coin.svg`). Its canonical
application source is `../../wallet/branding/mmx-logo.svg`; the website,
wallet/explorer interface, favicons, and platform icons derive from that SVG.
The website hero uses that same asset without adding another rotation or border.

The original hero treatment is retained in `hero-match.svg` for comparison.

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
production website build. See `../../wallet/branding/README.md` to regenerate
application assets from the selected canonical SVG.
