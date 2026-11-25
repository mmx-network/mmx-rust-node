// Regenerate every application icon from the selected, self-contained SVG.
// From apps/wallet: node branding/render.mjs
// Optional: --logo <png> also exports the unpadded logo at 256 px.
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { compactLogo, faviconLogo } from './compact-logo.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const app = resolve(here, '..');
const website = resolve(app, '../website');
const explorer = resolve(app, '../explorer');
const execute = promisify(execFile);

const document = (size, title, body) =>
  `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${size} ${size}" width="${size}" height="${size}"><title>${title}</title>${body}</svg>\n`;

/** Also exported so an offline SVG renderer can use exactly the same inputs. */
export async function brandJobs() {
  const svg = await readFile(join(here, 'mmx-logo.svg'), 'utf8');
  const compact = compactLogo(svg);
  const favicon = faviconLogo(svg);
  // At common dock sizes, use a 44-pixel face in a 48-pixel canvas.
  // Larger app-menu artwork keeps its existing padding and effects.
  const dock = compact.replace('viewBox="0 0 256 256"', 'viewBox="-7 -7 270 270"');
  const body = svg.replace(/<svg[^>]*>/, '').replace(/<\/svg>\s*$/, '')
    .replace(/<title>.*?<\/title>/, '').replace(/<desc>.*?<\/desc>/, '').trim();
  const appIcon = document(1024, 'MMX Wallet',
    `<g transform="translate(96 96) scale(3.25)">${body}</g>`);
  const square = (scale) => {
    const offset = 128 * (1 - scale);
    return document(256, 'MMX',
      `<rect width="256" height="256" fill="#1d102f"/><g transform="translate(${offset} ${offset}) scale(${scale})">${body}</g>`);
  };
  // The circle remains inside Android's maskable safe zone at 72% scale.
  const touch = square(0.88);
  const maskable = square(0.72);
  await writeFile(join(here, 'app-icon.svg'), appIcon);
  await writeFile(join(here, 'mmx-logo-compact.svg'), compact);
  for (const destination of [
    join(app, 'public/favicon.svg'), join(explorer, 'public/favicon.svg'),
    join(website, 'public/favicon.svg'),
  ]) await writeFile(destination, favicon);
  await writeFile(join(website, 'public/mmx-logo.svg'), svg);

  const share = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1200 630" width="1200" height="630">
  <title>MMX Network</title>
  <defs><radialGradient id="share-light"><stop stop-color="#4c1d95" stop-opacity=".5"/><stop offset="1" stop-color="#07060b" stop-opacity="0"/></radialGradient></defs>
  <rect width="1200" height="630" fill="#07060b"/>
  <ellipse cx="500" cy="300" rx="680" ry="480" fill="url(#share-light)"/>
  <g transform="translate(124 187)">${body}</g>
  <g fill="#efecf8" font-family="Space Grotesk, Inter, DejaVu Sans, sans-serif">
    <text x="434" y="287" font-size="66" font-weight="600" letter-spacing="-2">MMX Network</text>
    <text x="434" y="345" font-size="28" fill="#b4aecb">Decentralized by design.</text>
    <text x="434" y="405" font-size="19" fill="#b197fc">mmx.network</text>
  </g>
</svg>\n`;
  await writeFile(join(website, 'branding/share.svg'), share);

  const jobs = [{ svg: appIcon, size: 1024, out: join(here, 'app-icon.png') }];
  // Window managers scale this down; use the compact, almost full-canvas logo.
  jobs.push({ svg: compact, size: 512, out: join(app, 'src-tauri/icons/window-icon.png') });
  for (const owner of [app, website]) {
    for (const size of [192, 512]) {
      jobs.push({ svg: appIcon, size, out: join(owner, 'public', `icon-${size}.png`) });
    }
  }
  for (const owner of [app, website, explorer]) {
    jobs.push({ svg: touch, size: 180, out: join(owner, 'public/apple-touch-icon.png') });
  }
  jobs.push({ svg: maskable, size: 512, out: join(app, 'public/icon-maskable-512.png') });
  for (const [name, size] of [
    ['32x32.png', 32], ['48x48.png', 48], ['64x64.png', 64], ['128x128.png', 128],
    ['128x128@2x.png', 256], ['icon.png', 512],
  ]) jobs.push({ svg: size <= 64 ? dock : appIcon, size, out: join(app, 'src-tauri/icons', name) });
  jobs.push({ svg: share, width: 1200, height: 630, out: join(website, 'public/share.png') });
  const logoIndex = process.argv.indexOf('--logo');
  if (logoIndex > 0 && process.argv[logoIndex + 1]) {
    jobs.push({ svg, size: 256, out: resolve(process.argv[logoIndex + 1]) });
  }
  return jobs;
}

function icoFrames(icon) {
  return Array.from({ length: icon.readUInt16LE(4) }, (_, index) => {
    const entry = 6 + index * 16;
    const size = icon[entry] || 256;
    const length = icon.readUInt32LE(entry + 8);
    const offset = icon.readUInt32LE(entry + 12);
    return { size, data: icon.subarray(offset, offset + length) };
  });
}

function mergeIco(full, compact) {
  const frames = icoFrames(full);
  const smaller = new Map(icoFrames(compact).map(({ size, data }) => [size, data]));
  const header = Buffer.from(full.subarray(0, 6 + frames.length * 16));
  let offset = header.length;
  const images = frames.map(({ size, data }, index) => {
    const image = (size <= 64 && smaller.get(size)) || data;
    const entry = 6 + index * 16;
    header.writeUInt32LE(image.length, entry + 8);
    header.writeUInt32LE(offset, entry + 12);
    offset += image.length;
    return image;
  });
  return Buffer.concat([header, ...images]);
}

function icnsChunks(icon) {
  const result = new Map();
  for (let offset = 8; offset < icon.length;) {
    const type = icon.toString('ascii', offset, offset + 4);
    const length = icon.readUInt32BE(offset + 4);
    if (length < 8 || offset + length > icon.length) throw new Error('Invalid ICNS chunk length');
    result.set(type, icon.subarray(offset, offset + length));
    offset += length;
  }
  return result;
}

function mergeIcns(full, compact) {
  const chunks = icnsChunks(full);
  const smaller = icnsChunks(compact);
  // 16 and 32 point artwork, including Retina images and legacy alpha masks.
  for (const type of ['is32', 's8mk', 'il32', 'l8mk', 'ic11', 'ic12']) {
    if (chunks.has(type) && smaller.has(type)) chunks.set(type, smaller.get(type));
  }
  const icon = Buffer.concat([full.subarray(0, 8), ...chunks.values()]);
  icon.writeUInt32BE(icon.length, 4);
  return icon;
}

/** Use Tauri to encode the full and compact artwork, then select frames by size. */
export async function desktopContainers() {
  const output = await mkdtemp(join(tmpdir(), 'mmx-brand-icons-'));
  const icons = join(app, 'src-tauri/icons');
  const full = join(output, 'full');
  const compact = join(output, 'compact');
  try {
    for (const [input, destination] of [
      [join(here, 'app-icon.png'), full],
      [join(icons, 'window-icon.png'), compact],
    ]) {
      await execute(process.execPath, [
        join(app, 'node_modules/@tauri-apps/cli/tauri.js'), 'icon', input, '--output', destination,
      ], { cwd: app, maxBuffer: 1024 * 1024 });
    }
    for (const [name, merge] of [['icon.ico', mergeIco], ['icon.icns', mergeIcns]]) {
      const fullIcon = await readFile(join(full, name));
      const compactIcon = await readFile(join(compact, name));
      await writeFile(join(icons, name), merge(fullIcon, compactIcon));
    }
  } finally {
    await rm(output, { recursive: true, force: true });
  }
}

async function render() {
  const jobs = await brandJobs();
  const { chromium } = await import('playwright');
  const browser = await chromium.launch();
  try {
    for (const job of jobs) {
      const width = job.width ?? job.size;
      const height = job.height ?? job.size;
      const page = await browser.newPage({ viewport: { width, height }, deviceScaleFactor: 1 });
      const src = `data:image/svg+xml;base64,${Buffer.from(job.svg).toString('base64')}`;
      await page.setContent(`<!doctype html><html><body style="margin:0;background:transparent"><img src="${src}" width="${width}" height="${height}" style="display:block"></body></html>`);
      await page.waitForFunction(() => document.images[0]?.complete);
      await writeFile(job.out, await page.screenshot({ omitBackground: true, clip: { x: 0, y: 0, width, height } }));
      await page.close();
      console.log(job.out);
    }
  } finally {
    await browser.close();
  }
  await desktopContainers();
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) await render();
