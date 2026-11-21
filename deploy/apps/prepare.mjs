import { readdir, readFile, writeFile, mkdir } from 'node:fs/promises';
import { resolve, join, extname } from 'node:path';
import { pathToFileURL } from 'node:url';
import { gzipSync } from 'node:zlib';

/** Translate each build's global _headers policy, keeping its RPC origin. */
export function nginxHeaders(source, path = null) {
  const headers = new Map();
  let global = false;
  let matches = false;
  for (const line of source.split('\n')) {
    if (!line.trim()) continue;
    if (!line.startsWith(' ')) {
      global = line === '/*';
      matches = global || (path !== null && (line === path || (line.endsWith('/*') && path.startsWith(line.slice(0, -1)))));
      continue;
    }
    if (!matches) continue;
    const match = /^  ([A-Za-z][A-Za-z0-9-]*): (.+)$/.exec(line);
    if (!match || /[\x00-\x1f\x7f$]/.test(match[2])) {
      throw new Error(`Unsafe or malformed global header: ${line}`);
    }
    const [, name, value] = match;
    if (name.toLowerCase() === 'cache-control') {
      if (global) throw new Error('Cache-Control belongs to nginx locations');
      continue;
    }
    headers.set(name, value);
  }
  if (![...headers].some(([name, value]) => name.toLowerCase() === 'content-security-policy' && value.includes("frame-ancestors 'none'"))) {
    throw new Error('The build must supply Content-Security-Policy with frame-ancestors');
  }
  return [...headers].map(([name, value]) => `add_header ${name} "${value.replace(/[\\"]/g, '\\$&')}" always;\n`).join('');
}

const compressible = new Set(['.html', '.js', '.css', '.wasm', '.svg', '.json', '.webmanifest']);

async function precompress(directory) {
  // Only visit our small build outputs, sequentially; never source or node data.
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) await precompress(path);
    else if (entry.isFile() && compressible.has(extname(entry.name))) {
      const raw = await readFile(path);
      const compressed = gzipSync(raw, { level: 6 });
      if (compressed.length < raw.length) await writeFile(`${path}.gz`, compressed);
    }
  }
}

export async function prepare(root) {
  const generated = join(root, 'deploy/apps/generated');
  await mkdir(generated, { recursive: true });
  for (const [name, output] of [
    ['wallet', 'apps/wallet/dist/web'],
    ['explorer', 'apps/explorer/dist'],
    ['website', 'apps/website/dist'],
  ]) {
    const directory = join(root, output);
    const source = await readFile(join(directory, '_headers'), 'utf8');
    const headers = nginxHeaders(source, name === 'wallet' ? '/' : null);
    await writeFile(join(generated, `${name}-headers.conf`), headers);
    if (name === 'wallet') {
      for (const [kind,path] of [['connect','/connect.html'],['sdk','/wallet-sdk.js'],['sdk-asset','/assets/sdk/']]) {
        await writeFile(join(generated, `wallet-${kind}-headers.conf`), nginxHeaders(source,path));
      }
    }
    await precompress(directory);
  }
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  await prepare(resolve(process.argv[2] ?? '.'));
}
