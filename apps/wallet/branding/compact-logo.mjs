// The vector rim and glyph keep a soft edge when the bloom is too small to read.
export function compactLogo(svg) {
  return svg
    .replace(/<filter\b[^>]*\bid="glyph-bloom"[\s\S]*?<\/filter>/, '')
    .replace(/<use\b[^>]*\bfilter="url\(#glyph-bloom\)"[^>]*\/>/, '')
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/\n\s*\n/g, '\n');
}
