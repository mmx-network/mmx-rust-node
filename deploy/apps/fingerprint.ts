import { createHash } from 'node:crypto';
import { existsSync, readFileSync } from 'node:fs';
import { extname, join } from 'node:path';

const files = [
  'favicon.svg', 'apple-touch-icon.png', 'icon-192.png', 'icon-512.png',
  'icon-maskable-512.png', 'theme-init.js',
];

type Asset = { type: 'asset'; fileName: string; source: string | Uint8Array };

/** Shared build-only plugin; the small structural types avoid requiring Vite at the repo root. */
export function fingerprintPublicAssets() {
  let publicDir = '';
  let base = '/';
  let assetsDir = 'assets';
  return {
    name: 'mmx:fingerprint-public-assets',
    apply: 'build' as const,
    enforce: 'post' as const,
    configResolved(config: { publicDir: string; base: string; build: { assetsDir: string } }) {
      publicDir = config.publicDir;
      base = config.base;
      assetsDir = config.build.assetsDir;
    },
    generateBundle(
      this: { emitFile(asset: Asset): string },
      _options: unknown,
      bundle: Record<string, { type: string; source?: string | Uint8Array }>,
    ) {
      const urls = new Map<string, string>();
      for (const name of files) {
        const path = join(publicDir, name);
        if (!existsSync(path)) continue;
        const source = readFileSync(path);
        const hash = createHash('sha256').update(source).digest('hex').slice(0, 16);
        const extension = extname(name);
        const fileName = `${assetsDir}/${name.slice(0, -extension.length)}-${hash}${extension}`;
        this.emitFile({ type: 'asset', fileName, source });
        urls.set(name, `${base}${fileName}`);
      }

      // The manifests stay at their original URLs so their relative start_url
      // and scope (and installed app identity) remain unchanged.
      for (const name of ['manifest.webmanifest', 'site.webmanifest']) {
        const path = join(publicDir, name);
        if (!existsSync(path)) continue;
        const manifest = JSON.parse(readFileSync(path, 'utf8'));
        for (const icon of manifest.icons ?? []) {
          const url = urls.get(icon.src.replace(/^(?:\.\/|\/)/, ''));
          if (url) icon.src = url;
        }
        this.emitFile({ type: 'asset', fileName: name, source: `${JSON.stringify(manifest, null, 2)}\n` });
      }

      for (const [name, output] of Object.entries(bundle)) {
        if (output.type !== 'asset' || !name.endsWith('.html') || output.source === undefined) continue;
        const html = typeof output.source === 'string' ? output.source : new TextDecoder().decode(output.source);
        // Rewrite URLs in place: in particular, the classic theme bootstrap
        // must remain synchronous, in the head, ahead of the app and styles.
        output.source = html.replace(/\b(src|href)=(['"])(?:\.\/|\/)([^'"?#]+)\2/g,
          (match, attribute, quote, path) => {
            const url = urls.get(path);
            return url ? `${attribute}=${quote}${url}${quote}` : match;
          });
      }
    },
  };
}
