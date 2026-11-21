import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { latestWalletRelease, walletDownloadLinks } from '../src/wallet-download.js';

const base = 'https://github.com/mmx-network/mmx-rust-node/releases';
const fallback = `${base}?q=wallet-v&expanded=true`;
const release = (tag_name, published_at, extra = {}) => ({
  tag_name, published_at, draft: false, prerelease: false, ...extra,
});
const response = (releases, next = false) => new Response(JSON.stringify(releases), {
  headers: next ? { Link: '<https://api.github.com/repos/mmx-network/mmx-rust-node/releases?per_page=100&page=2>; rel="next"' } : {},
});

test('selects the newest published wallet, excluding node releases, drafts and prereleases', async () => {
  const url = await latestWalletRelease(async () => response([
    release('node-v2.0.0', '2026-11-04T00:00:00Z'),
    release('wallet-v1.0.3-build.43', '2026-11-03T00:00:00Z', { draft: true }),
    release('wallet-v1.0.3-build.42', '2026-11-02T00:00:00Z', { prerelease: true }),
    release('wallet-v1.0.1-build.40', '2026-10-27T00:00:00Z'),
    release('wallet-v1.0.2-build.41', '2026-10-28T00:00:00Z', { html_url: 'https://example.org/' }),
    release('wallet-v1.0.4-build.44', 'invalid date'),
    release('wallet-v../../node', '2026-11-05T00:00:00Z'),
    null,
  ]));
  assert.equal(url, `${base}/tag/wallet-v1.0.2-build.41`);
});

test('follows pagination and uses publication date across pages', async () => {
  const requests = [];
  const url = await latestWalletRelease(async (url, options) => {
    requests.push(url);
    assert.equal(options.credentials, 'omit');
    assert.equal(options.redirect, 'error');
    assert.ok(options.signal instanceof AbortSignal);
    return requests.length === 1
      ? response([release('node-v2.0.0', '2026-11-04'), release('wallet-v1.0.1-build.40', '2026-10-27')], true)
      : response([release('wallet-v1.0.2-build.41', '2026-10-28')]);
  });
  assert.equal(url, `${base}/tag/wallet-v1.0.2-build.41`);
  assert.deepEqual(requests.map((url) => new URL(url).searchParams.get('page')), ['1', '2']);
});

test('never substitutes a node release when there is no stable wallet release', async () => {
  assert.equal(await latestWalletRelease(async () => response([release('node-v2.0.0', '2026-11-04')])), null);
});

class Link extends EventTarget {
  href = fallback;
}
function click(link, extra = {}) {
  const event = Object.assign(new Event('click', { cancelable: true }), { button: 0, ...extra });
  link.dispatchEvent(event);
  return event;
}
const settle = () => new Promise((resolve) => setImmediate(resolve));

test('both links share one intent-triggered lookup and clicks wait for its result', async () => {
  const links = [new Link(), new Link()];
  const navigations = [];
  let requests = 0;
  let finish;
  walletDownloadLinks(links, () => {
    requests++;
    return new Promise((resolve) => { finish = resolve; });
  }, (url) => navigations.push(url));
  assert.equal(requests, 0);
  links[0].dispatchEvent(new Event('pointerenter'));
  links[1].dispatchEvent(new Event('focus'));
  assert.equal(click(links[1]).defaultPrevented, true);
  assert.deepEqual(navigations, []);
  finish(response([release('wallet-v1.0.2-build.41', '2026-10-28')]));
  await settle();
  assert.equal(requests, 1);
  assert.ok(links.every((link) => link.href === `${base}/tag/wallet-v1.0.2-build.41`));
  assert.deepEqual(navigations, [links[0].href]);
});

for (const [name, fetcher] of [
  ['rate limit', async () => new Response('', { status: 403 })],
  ['timeout', async () => { throw new DOMException('Timed out', 'TimeoutError'); }],
  ['invalid response', async () => response({ message: 'unavailable' })],
  ['no matching release', async () => response([])],
]) {
  test(`keeps the wallet-only fallback on ${name}`, async () => {
    const link = new Link();
    const navigations = [];
    walletDownloadLinks([link], fetcher, (url) => navigations.push(url));
    click(link);
    await settle();
    assert.equal(link.href, fallback);
    assert.deepEqual(navigations, [fallback]);
  });
}

test('modified clicks and middle clicks retain native link behavior', () => {
  const link = new Link();
  walletDownloadLinks([link], () => { assert.fail('Unexpected lookup'); }, () => { assert.fail('Unexpected navigation'); });
  for (const extra of [{ ctrlKey: true }, { metaKey: true }, { shiftKey: true }, { altKey: true }, { button: 1 }]) {
    assert.equal(click(link, extra).defaultPrevented, false);
  }
});

test('both static links remain restricted to wallet releases without JavaScript', () => {
  const html = readFileSync(new URL('../index.html', import.meta.url), 'utf8');
  const links = [...html.matchAll(/<a\b[^>]*data-wallet-download[^>]*>/g)];
  assert.equal(links.length, 2);
  for (const [link] of links) assert.ok(link.includes('href="' + fallback.replace('&', '&amp;') + '"'));
});
