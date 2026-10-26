import { test } from 'node:test';
import assert from 'node:assert/strict';
import { inlineLogos } from './inline-logo.mjs';

test('inlines independent vector logos with local, collision-free references', () => {
  const sizes = [36, 188, 58, 36];
  const html = inlineLogos(sizes.map(size =>
    `<img data-mmx-logo src="/mmx-logo.svg" width="${size}" height="${size}" alt="">`).join('\n'));
  const logos = [...html.matchAll(/<svg\b[^>]*>[\s\S]*?<\/svg>/g)];
  assert.equal(logos.length, sizes.length);
  const allIds = [...html.matchAll(/\bid="([^"]+)"/g)].map(match => match[1]);
  assert.equal(new Set(allIds).size, allIds.length);
  for (const [index, [svg]] of logos.entries()) {
    assert.ok(svg.includes(`width="${sizes[index]}" height="${sizes[index]}"`));
    assert.ok(svg.includes('aria-hidden="true"'));
    const ids = new Set([...svg.matchAll(/\bid="([^"]+)"/g)].map(match => match[1]));
    const refs = [...svg.matchAll(/url\(#([^)]+)\)|href="#([^"]+)"/g)];
    assert.ok(refs.length > 0);
    for (const reference of refs) assert.ok(ids.has(reference[1] ?? reference[2]));
    assert.ok(!/<image\b|<img\b|<script\b|<style\b|\sstyle=/.test(svg));
  }
});

test('leaves other images alone and rejects dimensionless logo placeholders', () => {
  const html = '<img src="/other.svg" alt="Other image">';
  assert.equal(inlineLogos(html), html);
  assert.throws(() => inlineLogos('<img data-mmx-logo src="/mmx-logo.svg">'), /dimensions/);
});
