import { readFileSync } from 'node:fs';
import { defineConfig } from 'vite';
import { inlineLogos } from './branding/inline-logo.mjs';
import { fingerprintPublicAssets } from '../../deploy/apps/fingerprint.ts';

// Keep the standalone preview's policy aligned with the nginx response policy.
// frame-ancestors is enforced by the HTTP header, not by the meta element.
const headers = readFileSync(new URL('./public/_headers', import.meta.url), 'utf8');
const policy = headers.match(/Content-Security-Policy: (.+)/)[1]
  .replace(/; frame-ancestors 'none'/, '');

export default defineConfig({
  base: '/',
  clearScreen: false,
  plugins: [fingerprintPublicAssets(), {
    name: 'website:logos',
    transformIndexHtml: { order: 'pre', handler: inlineLogos },
  }, {
    name: 'website:security',
    transformIndexHtml: {
      order: 'post',
      handler: (_, ctx) => ctx.server ? [] : [{
        tag: 'meta',
        attrs: { 'http-equiv': 'Content-Security-Policy', content: policy },
        injectTo: 'head-prepend',
      }],
    },
  }],
  build: { target: 'es2022', sourcemap: false, assetsInlineLimit: 0 },
});
