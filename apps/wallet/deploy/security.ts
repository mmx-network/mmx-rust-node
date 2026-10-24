/**
 * Security headers of the hosted web wallet (docs/wallet-app.md §6.6) and the
 * Vite plugin that applies them to web builds:
 *
 * - injects the Content-Security-Policy as a <meta> tag into index.html
 *   (`connect-src` = the chain server origin from VITE_RPC_URL),
 * - fails the build if index.html contains inline script or style,
 * - writes `_headers` (Netlify / Cloudflare Pages) next to the build output,
 * - sends the full header set from `vite preview` (end-to-end tests).
 *
 * deploy/_headers, deploy/Caddyfile and deploy/nginx-security-headers.conf
 * contain the same values for the default chain server (checked by
 * tests/unit/web-security.test.ts).
 */
import type { Plugin } from 'vite';
import { DEFAULT_RPC_URL, parseRpcUrl } from '../src/backend/web/rpc-url';

export { DEFAULT_RPC_URL };

/**
 * Features the wallet does not use are disabled; copying to the clipboard stays
 * available. Only features every Chromium platform knows are listed (unknown
 * names are reported as errors in the browser console).
 */
const PERMISSIONS_POLICY = [
  'accelerometer=()',
  'autoplay=()',
  'camera=()',
  'clipboard-read=()',
  'clipboard-write=(self)',
  'display-capture=()',
  'encrypted-media=()',
  'fullscreen=()',
  'gamepad=()',
  'geolocation=()',
  'gyroscope=()',
  'magnetometer=()',
  'microphone=()',
  'midi=()',
  'payment=()',
  'picture-in-picture=()',
  'publickey-credentials-create=()',
  'publickey-credentials-get=()',
  'screen-wake-lock=()',
  'usb=()',
].join(', ');

/**
 * Content-Security-Policy. `connect-src` includes 'self' because the wallet
 * core (.wasm) is fetched from the site itself. The <meta> variant omits
 * `frame-ancestors`, which browsers only honour as a header.
 */
export function contentSecurityPolicy(rpcOrigin: string, forMeta = false): string {
  const directives = [
    "default-src 'none'",
    "script-src 'self' 'wasm-unsafe-eval'",
    "style-src 'self'",
    "font-src 'self'",
    "img-src 'self' data:",
    `connect-src 'self' ${rpcOrigin}`,
    "manifest-src 'self'",
    "form-action 'self'",
    "base-uri 'none'",
    "object-src 'none'",
    "require-trusted-types-for 'script'",
    "trusted-types 'none'",
  ];
  if (!forMeta) directives.push("frame-ancestors 'none'");
  return directives.join('; ');
}

/** Response headers for every file of the site. */
export function securityHeaders(rpcOrigin: string): Array<[string, string]> {
  return [
    ['Content-Security-Policy', contentSecurityPolicy(rpcOrigin)],
    ['X-Content-Type-Options', 'nosniff'],
    ['Referrer-Policy', 'no-referrer'],
    ['Permissions-Policy', PERMISSIONS_POLICY],
    ['Cross-Origin-Opener-Policy', 'same-origin'],
    ['Cross-Origin-Resource-Policy', 'same-origin'],
    ['X-Frame-Options', 'DENY'],
    ['Strict-Transport-Security', 'max-age=63072000; includeSubDomains'],
  ];
}

/** Hashed build assets never change; the page itself is always revalidated so updates arrive at once. */
export const CACHE_IMMUTABLE = 'public, max-age=31536000, immutable';
export const CACHE_REVALIDATE = 'no-cache';

/** `_headers` file for Netlify and Cloudflare Pages. */
export function netlifyHeaders(rpcOrigin: string): string {
  const lines = ['/*'];
  for (const [name, value] of securityHeaders(rpcOrigin)) lines.push(`  ${name}: ${value}`);
  lines.push('', '/assets/*', `  Cache-Control: ${CACHE_IMMUTABLE}`);
  lines.push('', '/', `  Cache-Control: ${CACHE_REVALIDATE}`);
  lines.push('', '/index.html', `  Cache-Control: ${CACHE_REVALIDATE}`);
  return lines.join('\n') + '\n';
}

/** Problems that would break under the CSP or weaken it: inline code and styles. */
export function findInlineCode(html: string): string[] {
  const problems: string[] = [];
  for (const m of html.matchAll(/<script\b([^>]*)>([\s\S]*?)<\/script\s*>/giu)) {
    const attrs = m[1] ?? '';
    if (!/\ssrc\s*=/iu.test(attrs)) problems.push('inline <script>');
    else if ((m[2] ?? '').trim() !== '') problems.push('<script src> with inline content');
  }
  if (/<style\b/iu.test(html)) problems.push('inline <style>');
  for (const m of html.matchAll(/<[a-z][^>]*>/giu)) {
    const tag = m[0];
    if (/\son[a-z]+\s*=/iu.test(tag)) problems.push(`inline event handler in ${tag.slice(0, 40)}`);
    if (/\sstyle\s*=/iu.test(tag)) problems.push(`style attribute in ${tag.slice(0, 40)}`);
    if (/javascript:/iu.test(tag)) problems.push(`javascript: URL in ${tag.slice(0, 40)}`);
  }
  return problems;
}

function escapeAttribute(value: string): string {
  return value.replace(/&/gu, '&amp;').replace(/"/gu, '&quot;').replace(/</gu, '&lt;').replace(/>/gu, '&gt;');
}

/** The <meta> tag carrying the policy. */
export function cspMetaTag(rpcOrigin: string): string {
  return `<meta http-equiv="Content-Security-Policy" content="${escapeAttribute(contentSecurityPolicy(rpcOrigin, true))}" />`;
}

/** Inserts the CSP <meta> right after the charset declaration (before anything that loads). */
export function injectCspMeta(html: string, rpcOrigin: string): string {
  const tag = cspMetaTag(rpcOrigin);
  const charset = /<meta\s+charset\s*=\s*["']?utf-8["']?\s*\/?>/iu.exec(html);
  if (charset) {
    const end = charset.index + charset[0].length;
    return `${html.slice(0, end)}\n    ${tag}${html.slice(end)}`;
  }
  const head = /<head[^>]*>/iu.exec(html);
  if (!head) throw new Error('index.html has no <head>');
  const end = head.index + head[0].length;
  return `${html.slice(0, end)}\n    ${tag}${html.slice(end)}`;
}

/** Vite plugin for VITE_PLATFORM=web builds (see the file header). */
export function webSecurityPlugin(): Plugin {
  let origin = '';
  return {
    name: 'mmx-wallet:web-security',
    enforce: 'post',
    configResolved(config) {
      const raw = (config.env.VITE_RPC_URL as string | undefined) ?? process.env.VITE_RPC_URL;
      origin = parseRpcUrl(raw || DEFAULT_RPC_URL).origin;
    },
    transformIndexHtml: {
      order: 'post',
      handler(html, ctx) {
        if (ctx.server) return html;
        return injectCspMeta(html, origin);
      },
    },
    generateBundle(_options, bundle) {
      const page = bundle['index.html'];
      if (!page || page.type !== 'asset') this.error('web build: index.html missing from the output');
      const html = typeof page.source === 'string' ? page.source : new TextDecoder().decode(page.source);
      const problems = findInlineCode(html);
      if (problems.length > 0) this.error(`index.html must not contain inline code: ${problems.join('; ')}`);
      if (!html.includes(cspMetaTag(origin))) this.error('index.html is missing the Content-Security-Policy');
      this.emitFile({ type: 'asset', fileName: '_headers', source: netlifyHeaders(origin) });
    },
    configurePreviewServer(server) {
      const headers = securityHeaders(origin);
      server.middlewares.use((_req, res, next) => {
        for (const [name, value] of headers) res.setHeader(name, value);
        next();
      });
    },
  };
}
