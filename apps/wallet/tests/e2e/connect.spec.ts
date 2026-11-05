import { createServer, type Server } from 'node:http';
import type { Page } from '@playwright/test';
import { APP_PORT, APP_URL } from './env';
import { capture, goTo, MOBILE, openFundedWallet } from './app';
import { expect, test } from './fixtures';
import { NATIVE, testAddress } from './mock-rpc';

const DAPP = `http://127.0.0.1:${APP_PORT + 2}`;
const TO = testAddress('connected-recipient');
const CONTRACT = testAddress('connected-contract');
let server: Server;
test.beforeAll(async () => {
  server = createServer((_req, res) => {
    res.writeHead(200, { 'Content-Type': 'text/html', 'Cross-Origin-Opener-Policy': 'same-origin-allow-popups' });
    res.end(`<!doctype html><title>MMX test application</title><button id="connect">Connect wallet</button>
      <button id="send">Request transaction</button><script type="module">
      import { connect } from '${APP_URL}mmx-wallet.js';
      window.messages=[];window.addEventListener('message', e=>window.messages.push(e.data));
      document.querySelector('#connect').onclick=()=>{window.error=null;window.connectionPromise=connect({walletUrl:'${APP_URL}'}).then(c=>{window.connection=c;window.account=c.account;return c}).catch(e=>{window.error=e.code})};
      document.querySelector('#send').onclick=()=>{window.error=null;window.result=null;window.sending=window.connection.sendTransaction(window.tx,{gasLimit:1000000}).then(r=>window.result=r).catch(e=>window.error=e.code)};
      </script>`);
  });
  await new Promise<void>((resolve) => server.listen(APP_PORT + 2, '127.0.0.1', resolve));
});
test.afterAll(async () => {
  await new Promise<void>((resolve, reject) => server.close((e) => (e ? reject(e) : resolve())));
});

async function start(site: Page): Promise<Page> {
  await site.goto(DAPP);
  const popup = site.waitForEvent('popup');
  await site.getByRole('button', { name: 'Connect wallet', exact: true }).click();
  return popup;
}
async function enableConnections(page: Page) {
  await goTo(page, 'Settings');
  const toggle = page.getByRole('switch', { name: 'Allow website connections' });
  await expect(toggle).toHaveAttribute('aria-checked', 'false');
  await toggle.click();
  await expect(toggle).toHaveAttribute('aria-checked', 'true');
  await expect(toggle).toBeEnabled();
}
async function requested(site: Page, tx: unknown) {
  await site.evaluate((tx) => {
    (window as unknown as { tx: unknown }).tx = tx;
  }, tx);
  await site.getByRole('button', { name: 'Request transaction' }).click();
}
const transfer = { outputs: [{ __type: 'mmx.txout_t', address: TO, contract: NATIVE, amount: '1000000' }] };
async function result(site: Page) {
  return site.evaluate(
    () => (window as unknown as { result?: { txid: string }; error?: string; account?: unknown; messages: unknown[] }).result,
  );
}

test('website connections start off and expose no account before the setting is enabled', async ({ page, rpc, context }) => {
  await openFundedWallet(page, (address) => rpc.fund({ address, amount: '50000000' }));
  const site = await context.newPage();
  const popup = await start(site);
  await expect(popup.getByText('Website connections are off.', { exact: false })).toBeVisible();
  await expect(popup.getByLabel('Wallet', { exact: true })).toHaveCount(0);
  await expect.poll(() => site.evaluate(() => (window as unknown as { error: string }).error)).toBe('website_connections_disabled');
  expect(await site.evaluate(() => (window as unknown as { account?: unknown }).account)).toBeUndefined();
  expect(await rpc.broadcasts()).toEqual([]);
  await popup.close();
  await enableConnections(page);
  await page.reload();
  await expect(page.getByRole('switch', { name: 'Allow website connections' })).toHaveAttribute('aria-checked', 'true');
});

test('disabling connections in Settings cancels a transaction already awaiting approval', async ({ page, rpc, context }) => {
  await openFundedWallet(page, (address) => rpc.fund({ address, amount: '50000000' }));
  await enableConnections(page);
  const site = await context.newPage();
  const popup = await start(site);
  await popup.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect(popup.getByRole('heading', { name: 'Wallet connected' })).toBeVisible();
  await requested(site, transfer);
  await popup.getByRole('button', { name: 'Review transaction', exact: true }).click();
  await expect(popup.getByRole('heading', { name: 'Confirm transaction' })).toBeVisible();
  await page.getByRole('switch', { name: 'Allow website connections' }).click();
  await expect.poll(() => site.evaluate(() => (window as unknown as { error: string }).error)).toBe('website_connections_disabled');
  await expect(popup.getByRole('button', { name: 'Approve and send' })).toHaveCount(0);
  expect(await rpc.broadcasts()).toEqual([]);
  await popup.close();
});

test('blocking a website cancels its approval and persists until unblocked in Settings', async ({ page, rpc, context }) => {
  await openFundedWallet(page, (address) => rpc.fund({ address, amount: '50000000' }));
  await enableConnections(page);
  const site = await context.newPage();
  let popup = await start(site);
  await popup.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect(popup.getByRole('heading', { name: 'Wallet connected' })).toBeVisible();
  await requested(site, transfer);
  await popup.getByRole('button', { name: 'Review transaction', exact: true }).click();
  await expect(popup.getByRole('heading', { name: 'Confirm transaction' })).toBeVisible();
  await popup.getByRole('button', { name: 'Block this website' }).click();
  await expect.poll(() => site.evaluate(() => (window as unknown as { error: string }).error)).toBe('website_blocked');
  await expect(popup.getByRole('button', { name: 'Approve and send' })).toHaveCount(0);
  await expect(page.getByRole('button', { name: `Unblock ${DAPP}` })).toBeVisible();
  expect(await rpc.broadcasts()).toEqual([]);
  await popup.close();
  popup = await start(site);
  await expect(popup.getByText('This website is blocked.', { exact: false })).toBeVisible();
  await expect(popup.getByLabel('Wallet', { exact: true })).toHaveCount(0);
  await expect.poll(() => site.evaluate(() => (window as unknown as { error: string }).error)).toBe('website_blocked');
  await popup.close();
  await page.getByRole('button', { name: `Unblock ${DAPP}` }).click();
  popup = await start(site);
  await popup.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect(popup.getByRole('heading', { name: 'Wallet connected' })).toBeVisible();
  await popup.close();
});

test('reuses an unlocked wallet, signs a custom call after approval, and keeps secrets off the channel', async ({
  page,
  rpc,
  context,
  consoleGuard,
}) => {
  const wallet = await openFundedWallet(page, (address) => rpc.fund({ address, amount: '50000000' }), { passphrase: 'test extra layer' });
  await enableConnections(page);
  const site = await context.newPage();
  await consoleGuard.attach(site);
  const popup = await start(site);
  await consoleGuard.attach(popup);
  const assets: string[] = [];
  popup.on('request', (r) => assets.push(r.url()));
  await expect(popup.getByLabel('Wallet', { exact: true })).toContainText('already unlocked');
  expect(await site.evaluate(() => (window as unknown as { account?: unknown }).account)).toBeUndefined();
  await capture(popup, 'connect-select');
  await popup.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect(popup.getByRole('heading', { name: 'Wallet connected' })).toBeVisible();
  await expect(popup.getByRole('dialog', { name: 'Unlock for spending' })).toHaveCount(0);
  const account = await site.evaluate(() => (window as unknown as { account: { address: string; network: string } }).account);
  expect(account.address).toBe(wallet.addresses[0]);
  const args = ['12345678901234567890', { address: 'literal', amount: '42' }, -12];
  await requested(site, {
    execute: [
      {
        __type: 'mmx.operation.Deposit',
        address: CONTRACT,
        currency: NATIVE,
        amount: '1000000',
        method: 'deposit',
        user: account.address,
        args,
      },
    ],
  });
  await expect(popup.getByRole('heading', { name: 'Review transaction' })).toBeVisible();
  expect((await rpc.requests()).filter((r) => r.method === 'POST')).toEqual([]);
  await popup.getByRole('button', { name: 'Review transaction', exact: true }).click();
  await expect(popup.getByRole('heading', { name: 'Confirm transaction' })).toBeVisible();
  await expect(popup.getByText('Maximum network fee')).toBeVisible();
  await capture(popup, 'connect-review');
  expect((await rpc.requests()).filter((r) => r.method === 'POST')).toEqual([]);
  await popup.getByRole('button', { name: 'Approve and send' }).click();
  await expect.poll(() => result(site)).toMatchObject({ txid: expect.stringMatching(/^[0-9A-F]{64}$/) });
  const [broadcast] = await rpc.broadcasts();
  const signed = JSON.parse(broadcast!.raw);
  expect(signed.execute[0].args).toEqual(args);
  expect(signed.execute[0].address).toBe(CONTRACT);
  expect(broadcast!.submissions).toBe(1);
  const messages = await site.evaluate(() => JSON.stringify((window as unknown as { messages: unknown[] }).messages));
  expect(messages).not.toContain(wallet.words);
  expect(messages).not.toContain('test extra layer');
  expect(messages).not.toContain('signature');
  expect(assets.some((url) => url.endsWith('.wasm'))).toBe(false);
  await requested(site, transfer);
  await popup.getByRole('button', { name: 'Reject', exact: true }).click();
  await expect.poll(() => site.evaluate(() => (window as unknown as { error: string }).error)).toBe('user_rejected');
  expect(await rpc.broadcasts()).toHaveLength(1);
  await popup.close();
  await expect(page.getByRole('button', { name: 'Lock', exact: true }).first()).toBeVisible();
});

test('falls back to unlocking in the popup when the original wallet tab has closed', async ({ page, rpc, context, consoleGuard }) => {
  await page.setViewportSize(MOBILE);
  await openFundedWallet(page, (address) => rpc.fund({ address, amount: '50000000' }), { passphrase: 'popup passphrase' });
  await enableConnections(page);
  await page.close();
  const site = await context.newPage();
  const popup = await start(site);
  await popup.setViewportSize(MOBILE);
  await consoleGuard.attach(popup);
  await expect(popup.getByLabel('Wallet', { exact: true })).toContainText('unlock here');
  await popup.getByRole('button', { name: 'Connect', exact: true }).click();
  const dialog = popup.getByRole('dialog', { name: 'Unlock for spending' });
  await dialog.getByLabel('Passphrase', { exact: true }).fill('popup passphrase');
  await dialog.getByRole('button', { name: 'Unlock', exact: true }).click();
  await expect(popup.getByRole('heading', { name: 'Wallet connected' })).toBeVisible();
  await requested(site, transfer);
  await popup.getByRole('button', { name: 'Review transaction', exact: true }).click();
  await expect(popup.getByRole('heading', { name: 'Confirm transaction' })).toBeVisible();
  await capture(popup, 'connect-mobile-fallback');
  await popup.getByRole('button', { name: 'Approve and send' }).click();
  await expect.poll(() => result(site)).toMatchObject({ txid: expect.any(String) });
  await popup.close();
});

test('a lock in the signing tab invalidates the reviewed custom transaction', async ({ page, rpc, context }) => {
  await openFundedWallet(page, (address) => rpc.fund({ address, amount: '50000000' }));
  await enableConnections(page);
  const site = await context.newPage();
  const popup = await start(site);
  await expect(popup.getByLabel('Wallet', { exact: true })).toContainText('already unlocked');
  await popup.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect(popup.getByRole('heading', { name: 'Wallet connected' })).toBeVisible();
  await requested(site, transfer);
  await popup.getByRole('button', { name: 'Review transaction', exact: true }).click();
  await expect(popup.getByRole('heading', { name: 'Confirm transaction' })).toBeVisible();
  await page.getByRole('button', { name: 'Lock', exact: true }).first().click();
  await expect.poll(() => site.evaluate(() => (window as unknown as { error: string }).error)).toBe('locked');
  await expect(popup.getByRole('button', { name: 'Approve and send' })).toHaveCount(0);
  expect(await rpc.broadcasts()).toEqual([]);
  await popup.close();
});
