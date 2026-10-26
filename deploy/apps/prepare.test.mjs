import { test } from 'node:test';
import assert from 'node:assert/strict';
import { nginxHeaders } from './prepare.mjs';

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
