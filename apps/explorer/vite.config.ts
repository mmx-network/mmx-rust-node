import { defineConfig, loadEnv } from 'vite';
import { fileURLToPath } from 'node:url';
import { rpcOrigin } from './src/lib/config';

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), 'VITE_');
  const rpc = rpcOrigin(process.env.VITE_RPC_URL ?? env.VITE_RPC_URL);
  const policy = `default-src 'none'; script-src 'self'; style-src 'self'; font-src 'self'; img-src 'self' data:; connect-src 'self' ${rpc}; base-uri 'none'; form-action 'self'; object-src 'none'`;
  return {
    base: './',
    clearScreen: false,
    define: { __PLATFORM__: JSON.stringify('web') },
    resolve: {
      alias: { '@wallet': fileURLToPath(new URL('../wallet/src', import.meta.url)) },
      // Shared wallet components must use this app's Preact instance, including
      // when the wallet also has its own node_modules directory.
      dedupe: [
        'preact',
        '@fontsource-variable/inter',
        '@fontsource-variable/space-grotesk',
        '@fontsource-variable/jetbrains-mono',
      ],
    },
    esbuild: { jsx: 'automatic', jsxImportSource: 'preact' },
    plugins: [
      {
        name: 'explorer:security',
        transformIndexHtml: {
          order: 'post',
          handler: (_, ctx) =>
            ctx.server
              ? []
              : [
                  {
                    tag: 'meta',
                    attrs: { 'http-equiv': 'Content-Security-Policy', content: policy },
                    injectTo: 'head-prepend',
                  },
                ],
        },
        generateBundle() {
          this.emitFile({
            type: 'asset',
            fileName: '_headers',
            source: `/*\n  Content-Security-Policy: ${policy}; frame-ancestors 'none'\n  X-Content-Type-Options: nosniff\n  X-Frame-Options: DENY\n  Referrer-Policy: no-referrer\n\n/assets/*\n  Cache-Control: public, max-age=31536000, immutable\n\n/\n  Cache-Control: no-cache\n/index.html\n  Cache-Control: no-cache\n`,
          });
        },
      },
    ],
    build: { target: 'es2022', sourcemap: false, assetsInlineLimit: 0 },
    server: { strictPort: true },
  };
});
