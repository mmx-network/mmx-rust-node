// At small sizes a lighter, unfiltered shadow keeps the glyph defined.
export function compactLogo(svg) {
  return svg
    .replace(/<filter\b[^>]*\bid="glyph-shadow"[\s\S]*?<\/filter>/, '')
    .replace(/(<use\b[^>]*?) filter="url\(#glyph-shadow\)"/, (_, shadow) =>
      shadow.replace(/\bopacity="[^"]+"/, 'opacity="0.35"'))
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/\n\s*\n/g, '\n');
}
