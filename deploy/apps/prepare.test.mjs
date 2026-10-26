import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { gunzipSync } from 'node:zlib';
import { nginxHeaders, prepare } from './prepare.mjs';

const csp = "default-src 'none'; connect-src 'self' https://rpc.example.org:8443; frame-ancestors 'none'";

test('uses the built policy, and leaves caching to nginx', () => {
  const output = nginxHeaders(`/*\n  Content-Security-Policy: ${csp}\n  X-Frame-Options: DENY\n\n/assets/*\n  Cache-Control: public, max-age=31536000, immutable\n`);
  assert.equal(output, `add_header Content-Security-Policy "${csp}" always;\nadd_header X-Frame-Options "DENY" always;\n`);
});

test('quotes header values without allowing nginx variable interpolation', () => {
  assert.match(nginxHeaders(`/*\n  Content-Security-Policy: ${csp}\n  Example: a"b\\c\n`), /Example "a\\"b\\\\c" always;/);
  for (const value of ['$host', 'bad\rvalue', 'bad\u0000value']) {
    assert.throws(() => nginxHeaders(`/*\n  Content-Security-Policy: ${csp}\n  Example: ${value}\n`), /Unsafe/);
  }
});

test('fails closed on missing policies, framing protection or conflicting caching', () => {
  for (const source of ['', '/*\n  Content-Security-Policy: default-src self\n', `/index.html\n  Content-Security-Policy: ${csp}\n`]) {
    assert.throws(() => nginxHeaders(source), /Content-Security-Policy/);
  }
  assert.throws(() => nginxHeaders(`/*\n  Content-Security-Policy: ${csp}\n  Cache-Control: public\n`), /Cache-Control/);
});

test('prepares all three sites with isolated policies and precompressed HTML', async () => {
  const root = await mkdtemp(join(tmpdir(), 'mmx-apps-prepare-'));
  try {
    const sites = [
      ['wallet', 'apps/wallet/dist/web', csp],
      ['explorer', 'apps/explorer/dist', csp.replace('rpc.example.org:8443', 'explore.example.org')],
      ['website', 'apps/website/dist', "default-src 'self'; connect-src 'none'; frame-ancestors 'none'"],
    ];
    for (const [name, output, policy] of sites) {
      const directory = join(root, output);
      await mkdir(directory, { recursive: true });
      await writeFile(join(directory, '_headers'), `/*\n  Content-Security-Policy: ${policy}\n`);
      await writeFile(join(directory, 'index.html'), `<title>${name}</title>`.repeat(100));
    }
    await prepare(root);
    for (const [name, output, policy] of sites) {
      assert.equal(await readFile(join(root, 'deploy/apps/generated', `${name}-headers.conf`), 'utf8'),
        `add_header Content-Security-Policy "${policy}" always;\n`);
      assert.deepEqual(gunzipSync(await readFile(join(root, output, 'index.html.gz'))),
        await readFile(join(root, output, 'index.html')));
    }
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
