import { existsSync } from 'node:fs';
import { defineConfig, loadEnv, type Plugin } from 'vite';
import { webSecurityPlugin } from './deploy/security';

export const PLATFORMS = ['web', 'desktop', 'mock'] as const;
export type Platform = (typeof PLATFORMS)[number];

function isPlatform(value: string): value is Platform {
  return (PLATFORMS as readonly string[]).includes(value);
}

/**
 * The platform to build for. The npm scripts select it with the Vite mode
 * (`--mode web|desktop|mock`), which works in every shell. For other modes
 * (plain `vite`, `vite build`) VITE_PLATFORM chooses it, default web. Both
 * set to different platforms is an error rather than a silent choice.
 */
export function resolvePlatform(mode: string, override: string | undefined): Platform {
  const requested = override?.trim() ?? '';
  if (requested !== '' && !isPlatform(requested)) throw new Error(`unknown VITE_PLATFORM: ${requested}`);
  if (isPlatform(mode)) {
    if (requested !== '' && requested !== mode) {
      throw new Error(`VITE_PLATFORM=${requested} does not match --mode ${mode}: unset VITE_PLATFORM`);
    }
    return mode;
  }
  return requested === '' ? 'web' : (requested as Platform);
}

/** Backend modules imported by src/backend/index.ts, by platform. */
const BACKEND_MODULES: Record<Platform, string> = { mock: './mock', desktop: './desktop', web: './web/index' };
const UNAVAILABLE_BACKEND = '\0mmx-wallet:unavailable-backend';
const UNUSED_THEME = '\0mmx-wallet:unused-theme';

/**
 * Keeps other platforms and unselected styles out of the build output. Rollup
 * loads every dynamically imported module before removing dead branches, so
 * without this a desktop build would still emit the web wallet's WASM file and
 * every style's stylesheet and fonts.
 */
function buildScope(style: string, platform: Platform): Plugin {
  let building = false;
  return {
    name: 'mmx-wallet:build-scope',
    enforce: 'pre',
    configResolved(config) {
      building = config.command === 'build';
    },
    resolveId(source, importer) {
      if (!building || !importer) return null;
      const from = importer.replace(/\\/g, '/');
      if (from.endsWith('/src/backend/index.ts')) {
        const owner = Object.entries(BACKEND_MODULES).find(([, path]) => path === source)?.[0];
        if (owner && owner !== platform) return UNAVAILABLE_BACKEND;
      }
      if (from.endsWith('/src/styles/themes/index.ts')) {
        const theme = /^\.\/([a-z0-9-]+)\/theme\.css$/.exec(source)?.[1];
        if (theme && theme !== style) return UNUSED_THEME;
      }
      return null;
    },
    load(id) {
      if (id === UNAVAILABLE_BACKEND) {
        return "export function createBackend() { return Promise.reject(new Error('unavailable')); }";
      }
      if (id === UNUSED_THEME) return 'export default undefined;';
      return null;
    },
  };
}

export default defineConfig(({ mode }) => {
  const platform = resolvePlatform(mode, process.env.VITE_PLATFORM);
  const env = loadEnv(mode, process.cwd(), 'VITE_');
  const style = (process.env.VITE_WALLET_STYLE ?? env.VITE_WALLET_STYLE ?? 'draft').trim() || 'draft';
  if (!/^[a-z0-9-]+$/.test(style) || !existsSync(new URL(`./src/styles/themes/${style}/theme.css`, import.meta.url))) {
    throw new Error(`unknown VITE_WALLET_STYLE: ${style}`);
  }
  return {
    base: './',
    clearScreen: false,
    define: {
      __PLATFORM__: JSON.stringify(platform),
      __APP_VERSION__: JSON.stringify(process.env.npm_package_version ?? '0.0.0'),
    },
    esbuild: { jsx: 'automatic', jsxImportSource: 'preact' },
    // Web: Content-Security-Policy <meta>, inline-code check, _headers, preview headers.
    plugins: [buildScope(style, platform), ...(platform === 'web' ? [webSecurityPlugin()] : [])],
    build: {
      target: 'es2022',
      sourcemap: false,
      assetsInlineLimit: 0,
      chunkSizeWarningLimit: 2048,
    },
    server: {
      strictPort: true,
      // Build caches and outputs next to the sources are not app code.
      watch: { ignored: ['**/artifacts/**', '**/src-tauri/**', '**/dist/**', '**/test-results/**', '**/tests/.wasm-test/**'] },
    },
  };
});
