import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import {
  contentSecurityPolicy,
  cspMetaTag,
  DEFAULT_RPC_URL,
  findInlineCode,
  injectCspMeta,
  netlifyHeaders,
  securityHeaders,
} from '../../deploy/security';

const deploy = (name: string) => readFileSync(new URL(`../../deploy/${name}`, import.meta.url), 'utf8');
const ORIGIN = new URL(DEFAULT_RPC_URL).origin;

describe('Content-Security-Policy', () => {
  it('allows only the site itself and the chain server', () => {
    const csp = contentSecurityPolicy('https://rpc.example.org');
    expect(csp.split('; ')).toEqual([
      "default-src 'none'",
      "script-src 'self' 'wasm-unsafe-eval'",
      "worker-src 'self'",
      "style-src 'self'",
      "font-src 'self'",
      "img-src 'self' data:",
      "connect-src 'self' https://rpc.example.org",
      "manifest-src 'self'",
      "form-action 'self'",
      "base-uri 'none'",
      "object-src 'none'",
      "require-trusted-types-for 'script'",
      'trusted-types mmx-qr-worker',
      "frame-ancestors 'none'",
    ]);
  });

  it('leaves frame-ancestors out of the <meta> variant', () => {
    const meta = contentSecurityPolicy(ORIGIN, true);
    expect(meta).not.toContain('frame-ancestors');
    expect(contentSecurityPolicy(ORIGIN)).toBe(`${meta}; frame-ancestors 'none'`);
  });

  it('sends the full header set', () => {
    const names = securityHeaders(ORIGIN).map(([n]) => n);
    expect(names).toEqual([
      'Content-Security-Policy',
      'X-Content-Type-Options',
      'Referrer-Policy',
      'Permissions-Policy',
      'Cross-Origin-Opener-Policy',
      'Cross-Origin-Resource-Policy',
      'X-Frame-Options',
      'Strict-Transport-Security',
    ]);
    const permissions = new Map(securityHeaders(ORIGIN)).get('Permissions-Policy')!;
    expect(permissions).toContain('camera=(self)');
    expect(permissions).toContain('clipboard-write=(self)');
    // The Paste buttons read the clipboard; a policy of () makes them fail silently in Chromium.
    expect(permissions).toContain('clipboard-read=(self)');
    for (const feature of ['microphone', 'geolocation', 'payment', 'usb']) expect(permissions).toContain(`${feature}=()`);
  });
});

describe('index.html checks', () => {
  it('finds inline code and styles', () => {
    expect(findInlineCode('<script type="module" crossorigin src="./assets/a.js"></script>')).toEqual([]);
    expect(findInlineCode('<script>alert(1)</script>')).toHaveLength(1);
    expect(findInlineCode('<script type="module">import "x"</script>')).toHaveLength(1);
    expect(findInlineCode('<script src="a.js">alert(1)</script>')).toHaveLength(1);
    expect(findInlineCode('<style>body{}</style>')).toHaveLength(1);
    expect(findInlineCode('<img src="x" onerror="alert(1)">')).toHaveLength(1);
    expect(findInlineCode('<div style="color:red">')).toHaveLength(1);
    expect(findInlineCode('<a href="javascript:alert(1)">')).toHaveLength(1);
    expect(findInlineCode(injectCspMeta('<head><meta charset="utf-8" /></head>', ORIGIN))).toEqual([]);
  });

  it('places the policy right after the charset declaration', () => {
    const html = '<!doctype html><html><head>\n<meta charset="utf-8" />\n<link rel="icon" href="x"></head></html>';
    const out = injectCspMeta(html, ORIGIN);
    expect(out.indexOf(cspMetaTag(ORIGIN))).toBeGreaterThan(out.indexOf('charset'));
    expect(out.indexOf(cspMetaTag(ORIGIN))).toBeLessThan(out.indexOf('rel="icon"'));
    expect(injectCspMeta('<head></head>', ORIGIN)).toContain(cspMetaTag(ORIGIN));
    expect(() => injectCspMeta('<body></body>', ORIGIN)).toThrow();
  });

  it('works with the real index.html', () => {
    const html = readFileSync(new URL('../../index.html', import.meta.url), 'utf8');
    expect(findInlineCode(html)).toEqual([]);
    expect(injectCspMeta(html, ORIGIN)).toContain(cspMetaTag(ORIGIN));
  });
});

describe('deployment files', () => {
  it('_headers is the generated file for the default server', () => {
    expect(deploy('_headers')).toBe(netlifyHeaders(ORIGIN));
  });

  it('Caddy and nginx send every header with the same value', () => {
    const caddy = deploy('Caddyfile');
    const nginx = deploy('nginx-security-headers.conf');
    for (const [name, value] of securityHeaders(ORIGIN)) {
      expect(caddy).toContain(`${name} "${value}"`);
      expect(nginx).toContain(`add_header ${name} "${value}" always;`);
    }
    const site = deploy('nginx.conf');
    expect(site).toContain('application/wasm');
    // `http2 on;` needs nginx 1.25.1+, newer than Ubuntu 24.04 and Debian 12 ship.
    expect(site).not.toMatch(/^\s*http2\s+on\s*;/mu);
    expect(site.match(/^\s*listen\s+(\[::\]:)?443\s+ssl\s+http2;/gmu)).toHaveLength(2);
    expect(site.match(/include snippets\/mmx-wallet-security-headers\.conf;/gu)).toHaveLength(site.match(/location /gu)!.length);
  });
});
