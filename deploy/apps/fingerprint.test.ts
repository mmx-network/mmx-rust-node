import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fingerprintPublicAssets } from './fingerprint.ts';

test('fingerprints public assets, preserves bootstrap execution order and updates manifest icons', () => {
  const directory = mkdtempSync(join(tmpdir(), 'mmx-fingerprint-'));
  try {
    writeFileSync(join(directory, 'theme-init.js'), 'document.documentElement.dataset.scheme = "light";');
    writeFileSync(join(directory, 'apple-touch-icon.png'), Buffer.from([137, 80, 78, 71]));
    writeFileSync(join(directory, 'manifest.webmanifest'), JSON.stringify({
      start_url: './', scope: './', icons: [{ src: 'apple-touch-icon.png', sizes: '180x180' }],
    }));
    const html = '<head><script src="/theme-init.js"></script><link rel="apple-touch-icon" href="./apple-touch-icon.png"><link rel="stylesheet" href="/assets/app-hash.css"></head><body><script type="module" src="/assets/app-hash.js"></script></body>';
    const build = (base: string) => {
      const plugin = fingerprintPublicAssets();
      plugin.configResolved({ publicDir: directory, base, build: { assetsDir: 'assets' } });
      const bundle = { 'index.html': { type: 'asset', source: html } };
      const emitted = new Map<string, string | Uint8Array>();
      plugin.generateBundle.call({ emitFile(asset) { emitted.set(asset.fileName, asset.source); return asset.fileName; } }, {}, bundle);
      return { html: bundle['index.html'].source, emitted };
    };
    for (const base of ['/', './']) {
      const result = build(base);
      const theme = [...result.emitted.keys()].find((path) => path.endsWith('.js'))!;
      const icon = [...result.emitted.keys()].find((path) => path.endsWith('.png'))!;
      assert.match(theme, /^assets\/theme-init-[a-f0-9]{16}\.js$/);
      assert.match(icon, /^assets\/apple-touch-icon-[a-f0-9]{16}\.png$/);
      assert.ok(result.html.startsWith(`<head><script src="${base}${theme}"></script>`));
      assert.ok(result.html.includes(`href="${base}${icon}"`));
      assert.ok(result.html.includes('href="/assets/app-hash.css"'));
      assert.deepEqual(result.emitted.get(icon), readFileSync(join(directory, 'apple-touch-icon.png')));
      const manifest = JSON.parse(result.emitted.get('manifest.webmanifest') as string);
      assert.equal(manifest.start_url, './');
      assert.equal(manifest.scope, './');
      assert.equal(manifest.icons[0].src, `${base}${icon}`);
      assert.equal(manifest.icons[0].sizes, '180x180');
      assert.deepEqual([...build(base).emitted.keys()], [...result.emitted.keys()]);
    }
    const before = [...build('/').emitted.keys()];
    writeFileSync(join(directory, 'theme-init.js'), 'document.documentElement.dataset.scheme = "dark";');
    const after = [...build('/').emitted.keys()];
    assert.notEqual(before[1], after[1]);
    assert.equal(before[0], after[0]);
    // Keep unversioned aliases available for existing bookmarks and discovery.
    assert.ok(readFileSync(join(directory, 'apple-touch-icon.png')).length);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
