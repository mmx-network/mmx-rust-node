#!/usr/bin/env node
// Captures a fixed set of screens for one style against the mock backend, for
// visual review of themes. Needs a mock dev server (VITE_PLATFORM=mock vite),
// because only development builds honour ?style=.
//
//   node scripts/style-shots.mjs --style obsidian --out /tmp/shots [--base http://127.0.0.1:5391/] [--only overview,send]
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { parseArgs } from 'node:util';
import { chromium } from 'playwright';

const { values } = parseArgs({
  options: {
    style: { type: 'string', default: 'obsidian' },
    out: { type: 'string' },
    base: { type: 'string', default: 'http://127.0.0.1:5391/' },
    only: { type: 'string' },
    schemes: { type: 'string', default: 'light,dark' },
    scale: { type: 'string', default: '1' },
  },
});
if (!values.out) throw new Error('--out is required');
mkdirSync(values.out, { recursive: true });
const only = values.only ? new Set(values.only.split(',')) : null;
const want = (name) => !only || only.has(name);

const DESKTOP = { width: 1440, height: 900 };
const MOBILE = { width: 390, height: 844 };
const SETTLE = 900;

function url(params, hash) {
  const q = new URLSearchParams({ style: values.style, ...params });
  return `${values.base}?${q}#/${hash}`;
}

async function ready(page) {
  await page.waitForSelector('#app > *', { timeout: 15000 });
  await page.waitForFunction(() => !document.querySelector('.app-loading'), null, { timeout: 15000 });
  await page.evaluate(() => document.fonts.ready);
}

async function open(page, hash, params = {}) {
  await page.goto(url(params, hash));
  await ready(page);
  await page.waitForTimeout(SETTLE);
}

async function shot(page, name) {
  await page.waitForTimeout(250);
  await page.screenshot({ path: join(values.out, `${name}.png`), animations: 'disabled' });
}

async function overviewLoaded(page) {
  await page.getByText('Total balance').first().waitFor({ timeout: 15000 });
  await page.waitForFunction(() => document.querySelectorAll('.skeleton').length === 0, null, { timeout: 20000 }).catch(() => {});
  await page.waitForTimeout(SETTLE);
}

const errors = [];

async function desktop(browser, scheme) {
  const ctx = await browser.newContext({ viewport: DESKTOP, colorScheme: scheme, deviceScaleFactor: Number(values.scale) });
  const page = await ctx.newPage();
  page.on('console', (m) => m.type() === 'error' && errors.push(`${scheme} desktop: ${m.text()}`));
  page.on('pageerror', (e) => errors.push(`${scheme} desktop: ${e.message}`));
  const p = (n) => `desktop-${scheme}-${n}`;

  if (want('overview') || want('switcher')) {
    await open(page, 'overview');
    await overviewLoaded(page);
    if (want('overview')) await shot(page, p('overview'));
    if (want('switcher')) {
      await page.locator('button.switcher:visible').first().click();
      await shot(page, p('switcher'));
      await page.keyboard.press('Escape');
    }
  }
  if (want('activity') || want('detail')) {
    await open(page, 'activity');
    await page.waitForFunction(() => document.querySelectorAll('.skeleton').length === 0, null, { timeout: 20000 }).catch(() => {});
    await page.waitForTimeout(SETTLE);
    if (want('activity')) await shot(page, p('activity'));
    if (want('detail')) {
      const items = page.locator('.activity-list .list-item, .activity-list button').filter({ hasText: /Received|Sent|Moved/u });
      if ((await items.count()) > 0) {
        await items.first().click();
        await page.waitForTimeout(SETTLE);
        await shot(page, p('detail'));
      }
    }
  }
  if (want('send') || want('review')) {
    await open(page, 'send');
    const contacts = page.getByRole('option');
    await page.getByLabel('To').fill('a');
    await page.waitForTimeout(300);
    if ((await contacts.count()) > 0) await contacts.first().click();
    else await page.getByLabel('To').fill('mmx18n5fgj6n57t47ju8khdvfvcphyesr4dehft3qgkcqqrwzs0v36eqcktqvd');
    await page.getByLabel('Amount').fill('12.5');
    await page.getByLabel('Memo').fill('Invoice 2041');
    await page.waitForTimeout(400);
    if (want('send')) await shot(page, p('send'));
    if (want('review')) {
      await page.getByRole('button', { name: 'Review' }).click();
      await page.getByRole('region', { name: 'Transfer details' }).waitFor({ timeout: 15000 }).catch(() => {});
      await page.waitForTimeout(SETTLE);
      await shot(page, p('review'));
    }
  }
  if (want('receive')) {
    await open(page, 'receive');
    await shot(page, p('receive'));
  }
  if (want('contacts')) {
    await open(page, 'contacts');
    await shot(page, p('contacts'));
  }
  if (want('settings')) {
    await open(page, 'settings');
    await shot(page, p('settings'));
  }
  if (want('create')) {
    await open(page, 'create');
    await shot(page, p('create'));
  }
  if (want('unlock')) {
    // The "Savings" mock wallet has a passphrase and starts locked.
    await open(page, 'overview');
    await page.locator('button.switcher:visible').first().click();
    const savings = page.getByRole('menuitemradio', { name: /Savings/u });
    if ((await savings.count()) > 0) {
      await savings.first().click();
      await page.waitForTimeout(SETTLE);
      await shot(page, p('unlock'));
    }
  }
  if (want('web-welcome')) {
    await open(page, 'welcome', { platform: 'web' });
    await shot(page, p('web-welcome'));
  }
  if (want('web-unlock')) {
    await open(page, 'unlock', { platform: 'web' });
    await shot(page, p('web-unlock'));
  }
  await ctx.close();
}

async function mobile(browser, scheme) {
  const ctx = await browser.newContext({
    viewport: MOBILE,
    colorScheme: scheme,
    deviceScaleFactor: Number(values.scale),
    isMobile: true,
    hasTouch: true,
  });
  const page = await ctx.newPage();
  page.on('pageerror', (e) => errors.push(`${scheme} mobile: ${e.message}`));
  const p = (n) => `mobile-${scheme}-${n}`;
  if (want('m-overview')) {
    await open(page, 'overview');
    await overviewLoaded(page);
    await shot(page, p('overview'));
  }
  if (want('m-send')) {
    await open(page, 'send');
    await shot(page, p('send'));
  }
  if (want('m-activity')) {
    await open(page, 'activity');
    await page.waitForTimeout(SETTLE);
    await shot(page, p('activity'));
  }
  if (want('m-more')) {
    await open(page, 'overview');
    await page.getByRole('button', { name: 'More', exact: true }).click();
    await shot(page, p('more'));
  }
  await ctx.close();
}

const browser = await chromium.launch();
try {
  for (const scheme of values.schemes.split(',')) {
    await desktop(browser, scheme);
    await mobile(browser, scheme);
  }
} finally {
  await browser.close();
}
if (errors.length) {
  console.log(`console errors (${errors.length}):`);
  for (const e of [...new Set(errors)].slice(0, 20)) console.log(`  ${e}`);
}
console.log(`screenshots in ${values.out}`);
