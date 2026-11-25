// Keep the face, rim, and glyph shading in small icons and omit the grain.
export function compactLogo(svg) {
  return svg
    .replace(/<filter\b[^>]*\bid="surface-grain"[\s\S]*?<\/filter>/, '')
    .replace(/<circle\b[^>]*\bfilter="url\(#surface-grain\)"[^>]*\/>/, '')
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/\n\s*\n/g, '\n');
}
