const rimColor = '#bf91f2';
const glyphColor = '#fff8ff';

// A solid rim and slightly heavier glyph stay readable at dock and UI sizes.
export function compactLogo(svg) {
  const rim = [
    `<circle cx="128" cy="128" r="123.5" fill="none" stroke="${rimColor}" stroke-opacity="0.62" stroke-width="9"/>`,
    '<circle cx="128" cy="128" r="113" fill="none" stroke="#d0aff6" stroke-opacity="0.24" stroke-width="3.5"/>',
  ].join('\n    ');

  return svg
    .replace(/<desc>[\s\S]*?<\/desc>/, '<desc>Compact MMX with a violet face and a high-contrast glyph.</desc>')
    .replace(/<filter\b[^>]*\bid="glyph-(?:shadow|bloom)"[\s\S]*?<\/filter>/g, '')
    .replace(/<use\b[^>]*\bfilter="url\(#glyph-(?:shadow|bloom)\)"[^>]*\/>/g, '')
    .replace(/<radialGradient\b[^>]*\bid="rim"[\s\S]*?<\/radialGradient>/, '')
    .replace(/<circle\b[^>]*\bfill="url\(#rim\)"[^>]*\/>/, rim)
    .replace(/<use href="#mmx-glyph" fill="[^"]+"\/>/,
      `<use href="#mmx-glyph" fill="${glyphColor}" stroke="${glyphColor}" stroke-width="2" stroke-linejoin="round"/>`)
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/\n\s*\n/g, '\n');
}

// Fit the four accents and the main strokes to the 16px tab grid.
export function faviconLogo(svg) {
  const face = svg.match(/<linearGradient\b[^>]*\bid="base"[\s\S]*?<\/linearGradient>/)?.[0];
  if (!face) throw new Error('The MMX logo needs a base gradient');
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256" width="16" height="16">
  <title>MMX</title>
  <defs>
    ${face}
  </defs>
  <circle cx="128" cy="128" r="124" fill="url(#base)"/>
  <circle cx="128" cy="128" r="117" fill="none" stroke="${rimColor}" stroke-opacity="0.62" stroke-width="14"/>
  <path fill="${glyphColor}" transform="scale(16)" d="M5.25 5H7L11.75 11.25L9.25 11.75L5 6Q4.5 5.25 5.25 5ZM8.5 4.5L11.25 4L10 7.25ZM5 12L6.25 9L7.75 11.5ZM6 2H7.25V3.5H6ZM8 1.75H9.25V3.25H8ZM7 13H8.25V14.5H7ZM9 12.75H10.25V14.25H9Z"/>
</svg>
`;
}
