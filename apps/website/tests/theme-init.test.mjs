import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';

const source = readFileSync(new URL('../public/theme-init.js', import.meta.url), 'utf8');

for (const [name, stored, systemLight, expected] of [
  ['saved light overrides a dark system', 'light', false, 'light'],
  ['saved dark overrides a light system', 'dark', true, 'dark'],
  ['no saved choice follows a light system', null, true, 'light'],
  ['no saved choice follows a dark system', null, false, 'dark'],
  ['invalid saved choice follows the system', 'invalid', true, 'light'],
  ['blocked storage follows the system', 'blocked', true, 'light'],
]) {
  test(`initial theme: ${name}`, () => {
    // Only the head exists: no app, body, theme button or module has loaded.
    const root = { dataset: { scheme: 'dark' } };
    const meta = { content: '#07060b' };
    runInNewContext(source, {
      document: {
        documentElement: root,
        querySelector(selector) {
          assert.equal(selector, 'meta[name="theme-color"]');
          return meta;
        },
      },
      localStorage: {
        getItem(key) {
          assert.equal(key, 'mmx-website-scheme');
          if (stored === 'blocked') throw new Error('Storage unavailable');
          return stored;
        },
      },
      window: {
        matchMedia(query) {
          assert.equal(query, '(prefers-color-scheme: light)');
          return { matches: systemLight };
        },
      },
    });
    assert.equal(root.dataset.scheme, expected);
    assert.equal(meta.content, expected === 'light' ? '#f6f5fb' : '#07060b');
  });
}

test('both entry pages load the classic bootstrap before the module and body', () => {
  for (const page of ['../index.html', '../branding/index.html']) {
    const html = readFileSync(new URL(page, import.meta.url), 'utf8');
    const script = '<script src="/theme-init.js"></script>';
    assert.ok(html.includes(script));
    assert.ok(html.indexOf(script) > html.indexOf('<meta name="theme-color"'));
    assert.ok(html.indexOf(script) < html.indexOf('<script type="module"'));
    assert.ok(html.indexOf(script) < html.indexOf('<body>'));
  }
});
