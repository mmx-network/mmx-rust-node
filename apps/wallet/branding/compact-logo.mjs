// Derive navigation artwork from the canonical design, keeping its geometry and
// gradients while omitting effects that lose definition below 64 CSS pixels.
export function compactLogo(svg) {
  return svg
    .replace(/<desc>[\s\S]*?<\/desc>/, '<desc>Compact MMX mark with the selected violet gradient, wide border, and nine-degree rotation.</desc>')
    .replace(/<filter\b[^>]*\bid="soft-halo"[\s\S]*?<\/filter>/, '')
    .replace(/<mask\b[^>]*\bid="rim"[\s\S]*?<\/mask>/, '')
    .replace(/<clipPath\b[^>]*\bid="face-clip"[\s\S]*?<\/clipPath>/, '')
    .replace(/<circle\b[^>]*\bmask="url\(#rim\)"[^>]*\/>/, '')
    .replace(/<use\b[^>]*\bfilter="url\(#soft-halo\)"[^>]*\/>/, '')
    .replace(/ clip-path="url\(#face-clip\)"/, '')
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/\n\s*\n/g, '\n');
}
