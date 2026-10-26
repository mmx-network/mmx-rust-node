// Regenerate every application icon from the selected, self-contained SVG.
// From apps/wallet: node branding/render.mjs
// Optional: --logo <png> also exports the unpadded logo at 256 px.
import { copyFile, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';

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
  for (const destination of [
    join(app, 'public/favicon.svg'), join(explorer, 'public/favicon.svg'),
    join(website, 'public/favicon.svg'), join(website, 'public/mmx-logo.svg'),
  ]) await writeFile(destination, svg);

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
  for (const owner of [app, website]) {
    for (const size of [192, 512]) {
      jobs.push({ svg: appIcon, size, out: join(owner, 'public', `icon-${size}.png`) });
    }
    jobs.push({ svg: touch, size: 180, out: join(owner, 'public/apple-touch-icon.png') });
  }
  jobs.push({ svg: maskable, size: 512, out: join(app, 'public/icon-maskable-512.png') });
  for (const [name, size] of [
    ['32x32.png', 32], ['64x64.png', 64], ['128x128.png', 128],
    ['128x128@2x.png', 256], ['icon.png', 512],
  ]) jobs.push({ svg: appIcon, size, out: join(app, 'src-tauri/icons', name) });
  jobs.push({ svg: share, width: 1200, height: 630, out: join(website, 'public/share.png') });
  const logoIndex = process.argv.indexOf('--logo');
  if (logoIndex > 0 && process.argv[logoIndex + 1]) {
    jobs.push({ svg, size: 256, out: resolve(process.argv[logoIndex + 1]) });
  }
  return jobs;
}

/** Use Tauri's encoder, retaining only the desktop formats tracked by the app. */
export async function desktopContainers() {
  const output = await mkdtemp(join(tmpdir(), 'mmx-brand-icons-'));
  try {
    await execute(process.execPath, [
      join(app, 'node_modules/@tauri-apps/cli/tauri.js'), 'icon',
      join(here, 'app-icon.png'), '--output', output,
    ], { cwd: app, maxBuffer: 1024 * 1024 });
    for (const name of ['icon.ico', 'icon.icns']) {
      await copyFile(join(output, name), join(app, 'src-tauri/icons', name));
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
