// Renders the raster brand assets from the SVG sources with the locally cached
// Playwright Chromium (no browser download).
//
//   node branding/render.mjs              writes branding/app-icon.png and the web icons in public/
//   node branding/render.mjs --logo <png> also renders mmx-logo.svg at 256 px (for comparisons)
import { readFile, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';

const here = dirname(fileURLToPath(import.meta.url));
const app = resolve(here, '..');

const glyph = (await readFile(join(here, 'mmx-mark.svg'), 'utf8')).match(/ d="([^"]+)"/)[1];

// Full-bleed square variant for platforms that apply their own mask (iOS, Android maskable).
const square = (scale) => {
  const offset = 128 - 128 * scale;
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256">
  <rect width="256" height="256" fill="#5E289F"/>
  <g transform="translate(${offset} ${offset}) scale(${scale})"><path fill="#FFFFFF" d="${glyph}"/></g>
</svg>`;
};

const jobs = [
  { svg: await readFile(join(here, 'app-icon.svg'), 'utf8'), size: 1024, out: join(here, 'app-icon.png') },
  { svg: await readFile(join(here, 'app-icon.svg'), 'utf8'), size: 512, out: join(app, 'public', 'icon-512.png') },
  { svg: await readFile(join(here, 'app-icon.svg'), 'utf8'), size: 192, out: join(app, 'public', 'icon-192.png') },
  { svg: square(0.78), size: 180, out: join(app, 'public', 'apple-touch-icon.png') },
  { svg: square(0.72), size: 512, out: join(app, 'public', 'icon-maskable-512.png') },
];

const logoIndex = process.argv.indexOf('--logo');
if (logoIndex > 0 && process.argv[logoIndex + 1]) {
  jobs.push({
    svg: await readFile(join(here, 'mmx-logo.svg'), 'utf8'),
    size: 256,
    out: resolve(process.argv[logoIndex + 1]),
  });
}

const browser = await chromium.launch();
try {
  for (const job of jobs) {
    const page = await browser.newPage({ viewport: { width: job.size, height: job.size }, deviceScaleFactor: 1 });
    const src = `data:image/svg+xml;base64,${Buffer.from(job.svg).toString('base64')}`;
    await page.setContent(
      `<!doctype html><html><body style="margin:0;background:transparent">` +
        `<img src="${src}" width="${job.size}" height="${job.size}" style="display:block"></body></html>`,
    );
    await page.waitForFunction(() => document.images[0]?.complete);
    const png = await page.screenshot({ omitBackground: true, clip: { x: 0, y: 0, width: job.size, height: job.size } });
    await writeFile(job.out, png);
    await page.close();
    console.log(`${job.out} (${job.size}px)`);
  }
} finally {
  await browser.close();
}
