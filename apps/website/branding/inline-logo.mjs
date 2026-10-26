import { readFileSync } from 'node:fs';
import { compactLogo } from '../../wallet/branding/compact-logo.mjs';

const source = new URL('../../wallet/branding/mmx-logo.svg', import.meta.url);

// Keep the website's logos in the document's vector drawing tree. In particular,
// the hero must not scale an SVG image's intermediate filtered bitmap.
export function inlineLogos(html) {
  let index = 0;
  return html.replace(/<img\b[^>]*\bdata-mmx-logo\b[^>]*>/g, (placeholder) => {
    const width = /\bwidth="(\d+)"/.exec(placeholder)?.[1];
    const height = /\bheight="(\d+)"/.exec(placeholder)?.[1];
    if (!width || !height) throw new Error('MMX logo placeholders need explicit dimensions');
    const prefix = `mmx-logo-${++index}-`;
    const svg = readFileSync(source, 'utf8');
    return (Number(width) <= 64 ? compactLogo(svg) : svg)
      .replace(/\bwidth="256"/, `width="${width}"`)
      .replace(/\bheight="256"/, `height="${height}"`)
      .replace('<svg ', '<svg data-mmx-logo="" aria-hidden="true" focusable="false" ')
      .replace(/<title>[\s\S]*?<\/title>\s*|<desc>[\s\S]*?<\/desc>\s*/g, '')
      .replace(/\bid="([^"]+)"/g, (_, id) => `id="${prefix}${id}"`)
      .replace(/url\(#([^)]+)\)/g, (_, id) => `url(#${prefix}${id})`)
      .replace(/\bhref="#([^"]+)"/g, (_, id) => `href="#${prefix}${id}"`);
  });
}
