import { createServer, type Server } from 'node:http';
import type { Page } from '@playwright/test';
import { APP_PORT, APP_URL } from './env';
import { capture, goTo, MOBILE, openFundedWallet } from './app';
import { expect, test } from './fixtures';
import { NATIVE, testAddress } from './mock-rpc';

const DAPP = `http://127.0.0.1:${APP_PORT + 2}`;
const TO = testAddress('connected-recipient'),
  CONTRACT = testAddress('connected-contract');
let server: Server;
test.beforeAll(async () => {
  server = createServer((_req, res) => {
    res.writeHead(200, { 'Content-Type': 'text/html', 'Cross-Origin-Opener-Policy': 'same-origin-allow-popups' });
    res.end(`<!doctype html><title>MMX test application</title><button id="connect">Connect wallet</button><button id="send">Request transaction</button><script type="module">
      import {connect} from '${APP_URL}mmx-wallet.js';
      window.messages=[];window.addEventListener('message',e=>window.messages.push(e.data));
      document.querySelector('#connect').onclick=()=>{window.error=null;window.connectionPromise=connect({walletUrl:'${APP_URL}'}).then(c=>{window.connection=c;window.account=c.account;return c}).catch(e=>{window.error=e.code})};
      document.querySelector('#send').onclick=()=>{window.error=null;window.result=null;window.sending=window.connection.sendTransaction(window.tx,{gasLimit:1000000}).then(r=>window.result=r).catch(e=>window.error=e.code)};
    </script>`);
  });
  await new Promise<void>((resolve) => server.listen(APP_PORT + 2, '127.0.0.1', resolve));
});
test.afterAll(async () => {
  await new Promise<void>((resolve, reject) => server.close((e) => (e ? reject(e) : resolve())));
});
async function start(site: Page) {
  await site.goto(DAPP);
  const popup = site.waitForEvent('popup');
  await site.getByRole('button', { name: 'Connect wallet', exact: true }).click();
  return popup;
}
async function enable(page: Page) {
  await goTo(page, 'Settings');
  const toggle = page.getByRole('switch', { name: 'Allow website connections' });
  await expect(toggle).toHaveAttribute('aria-checked', 'false');
  await toggle.click();
  await expect(toggle).toBeEnabled();
  await expect(toggle).toHaveAttribute('aria-checked', 'true');
}
async function connect(site: Page) {
  const popup = await start(site);
  await expect(popup.getByText('Ready to connect', { exact: true })).toBeVisible();
  await popup.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect.poll(() => popup.isClosed()).toBe(true);
  await expect.poll(() => site.evaluate(() => (window as unknown as { account?: unknown }).account)).toBeDefined();
  return popup;
}
async function requested(site: Page, tx: unknown) {
  await site.evaluate((tx) => {
    (window as unknown as { tx: unknown }).tx = tx;
  }, tx);
  const popup = site.waitForEvent('popup');
  await site.getByRole('button', { name: 'Request transaction' }).click();
  return popup;
}
async function error(site: Page) {
  return site.evaluate(() => (window as unknown as { error?: string }).error);
}
async function result(site: Page) {
  return site.evaluate(() => (window as unknown as { result?: { txid: string } }).result);
}
const transfer = { outputs: [{ __type: 'mmx.txout_t', address: TO, contract: NATIVE, amount: '1000000' }] };
async function quote(popup: Page) {
  await popup.getByRole('button', { name: 'Continue', exact: true }).click();
  await expect(popup.getByRole('heading', { name: 'Confirm transaction' })).toBeVisible();
}

test('connections are off by default and denied popups close without sharing an account', async ({ page, rpc, context }) => {
  await openFundedWallet(page, (a) => rpc.fund({ address: a, amount: '50000000' }));
  const site = await context.newPage();
  const popup = await start(site);
  await expect.poll(() => error(site)).toBe('website_connections_disabled');
  await expect.poll(() => popup.isClosed()).toBe(true);
  expect(await site.evaluate(() => (window as unknown as { account?: unknown }).account)).toBeUndefined();
  expect(await rpc.broadcasts()).toEqual([]);
  await enable(page);
  await page.reload();
  await expect(page.getByRole('switch', { name: 'Allow website connections' })).toHaveAttribute('aria-checked', 'true');
});

test('connecting, sending and rejecting each use one new popup and close it', async ({ page, rpc, context, consoleGuard }) => {
  const wallet = await openFundedWallet(page, (a) => rpc.fund({ address: a, amount: '50000000' }), { passphrase: 'extra layer' });
  await enable(page);
  const site = await context.newPage();
  await consoleGuard.attach(site);
  let popup = await start(site);
  await consoleGuard.attach(popup);
  await expect(popup.getByText('Ready to connect', { exact: true })).toBeVisible();
  expect(await site.evaluate(() => (window as unknown as { account?: unknown }).account)).toBeUndefined();
  await capture(popup, 'connection-consent');
  await popup.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect.poll(() => popup.isClosed()).toBe(true);
  const account = await site.evaluate(() => (window as unknown as { account: { address: string } }).account);
  expect(account.address).toBe(wallet.addresses[0]);
  const args = ['12345678901234567890', { address: 'literal', amount: '42' }, -12];
  const tx = {
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
  };
  popup = await requested(site, tx);
  await consoleGuard.attach(popup);
  const wasmRequests: string[] = [];
  popup.on('request', (r) => {
    if (r.url().endsWith('.wasm')) wasmRequests.push(r.url());
  });
  await expect(popup.getByRole('heading', { name: 'Review transaction' })).toBeVisible();
  await expect(popup.getByText('Deposit to contract', { exact: true })).toBeVisible();
  await expect(popup.locator('.connection__deposit')).toContainText('1');
  await expect(popup.locator('pre')).toHaveCount(0);
  await expect(popup.getByText('__type', { exact: false })).toHaveCount(0);
  await capture(popup, 'connection-contract-preview');
  expect((await rpc.requests()).filter((r) => r.method === 'POST')).toEqual([]);
  await quote(popup);
  await expect(popup.getByText('Maximum network fee', { exact: true })).toBeVisible();
  await expect(popup.getByText('“12345678901234567890”', { exact: true })).toBeVisible();
  await capture(popup, 'connection-contract-confirm');
  expect((await rpc.requests()).filter((r) => r.method === 'POST')).toEqual([]);
  await popup.getByRole('button', { name: 'Send now', exact: true }).click();
  await expect.poll(() => popup.isClosed()).toBe(true);
  await expect.poll(() => result(site)).toMatchObject({ txid: expect.stringMatching(/^[0-9A-F]{64}$/) });
  const [broadcast] = await rpc.broadcasts();
  expect(JSON.parse(broadcast!.raw).execute[0].args).toEqual(args);
  expect(broadcast!.submissions).toBe(1);
  expect(wasmRequests).toEqual([]);
  const messages = await site.evaluate(() => JSON.stringify((window as unknown as { messages: unknown[] }).messages));
  expect(messages).not.toContain(wallet.words);
  expect(messages).not.toContain('extra layer');
  expect(messages).not.toContain('signature');
  popup = await requested(site, transfer);
  await expect(popup.locator('.connection__large-amount')).toContainText('1');
  await popup.getByRole('button', { name: 'Reject', exact: true }).click();
  await expect.poll(() => popup.isClosed()).toBe(true);
  await expect.poll(() => error(site)).toBe('user_rejected');
  expect(await rpc.broadcasts()).toHaveLength(1);
  // The public connection survives a completed/rejected request; signing keys
  // remain in the original tab rather than in any of the closed popups.
  popup = await requested(site, transfer);
  await quote(popup);
  await expect(popup.getByRole('dialog', { name: 'Unlock for spending' })).toHaveCount(0);
  await popup.getByRole('button', { name: 'Reject', exact: true }).click();
  await expect.poll(() => popup.isClosed()).toBe(true);
  await expect(page.getByRole('button', { name: 'Lock', exact: true }).first()).toBeVisible();
});

for (const passphrase of [undefined, 'timed popup unlock']) {
  test(`a locked wallet connects publicly and retains a popup unlock until timeout (${passphrase ? 'passphrase' : 'regular'})`, async ({page,rpc,context,consoleGuard}) => {
    await page.clock.install();
    const wallet=await openFundedWallet(page,(address)=>rpc.fund({address,amount:'50000000'}),{passphrase});
    await enable(page);
    await page.getByRole('button',{name:'Lock',exact:true}).first().click();
    await expect(page.getByRole('button',{name:'Unlock',exact:true}).first()).toBeVisible();
    await page.evaluate(()=>{
      const channel=new BroadcastChannel('mmx-wallet-connect/v2');
      const state=window as unknown as {privateMessages:unknown[];spy:BroadcastChannel};
      state.privateMessages=[];state.spy=channel;channel.onmessage=({data})=>state.privateMessages.push(data);
    });
    const site=await context.newPage();
    let popup=await start(site);
    await consoleGuard.attach(popup);
    await expect(popup.getByRole('button',{name:'Connect',exact:true})).toBeVisible();
    await expect(popup.getByRole('dialog')).toHaveCount(0);
    await popup.getByRole('button',{name:'Connect',exact:true}).click();
    await expect.poll(()=>popup.isClosed()).toBe(true);
    await expect(page.getByRole('button',{name:'Unlock',exact:true}).first()).toBeVisible();
    const tx={outputs:[{...transfer.outputs[0],memo:'A simple transfer'}]};
    popup=await requested(site,tx);
    await popup.setViewportSize({width:480,height:720});
    await consoleGuard.attach(popup);
    const noScroll=async()=>expect(await popup.locator('.focus-layout__main').evaluate(e=>e.scrollHeight-e.clientHeight)).toBeLessThanOrEqual(1);
    await expect(popup.getByRole('button',{name:'Unlock to review',exact:true})).toBeVisible();
    await noScroll();
    await popup.getByRole('button',{name:'Unlock to review',exact:true}).click();
    const dialog=popup.getByRole('dialog',{name:'Unlock for spending'});
    await dialog.getByLabel(passphrase?'Passphrase':'Recovery phrase',{exact:true}).fill(passphrase??wallet.words);
    await dialog.getByRole('button',{name:'Unlock',exact:true}).click();
    await expect(popup.getByRole('heading',{name:'Confirm transaction'})).toBeVisible();
    await expect(page.getByRole('button',{name:'Lock',exact:true}).first()).toBeVisible();
    await noScroll();
    await capture(popup,'connection-transfer-default-confirm');
    await popup.getByRole('button',{name:'Send now',exact:true}).click();
    await expect.poll(()=>popup.isClosed()).toBe(true);
    await page.clock.fastForward('04:00');
    popup=await requested(site,transfer);
    await quote(popup);
    await expect(popup.getByRole('dialog')).toHaveCount(0);
    await popup.getByRole('button',{name:'Send now',exact:true}).click();
    await expect.poll(()=>popup.isClosed()).toBe(true);
    await page.clock.fastForward('04:00');
    await expect(page.getByRole('button',{name:'Lock',exact:true}).first()).toBeVisible();
    await page.clock.fastForward('01:30');
    await expect(page.getByRole('button',{name:'Unlock',exact:true}).first()).toBeVisible();
    // The encrypted unlock request is single-use, including after a later lock.
    const messages=await page.evaluate(()=>{
      const state=window as unknown as {privateMessages:Record<string,unknown>[];spy:BroadcastChannel};
      const key=state.privateMessages.find(m=>m.method==='unlockKey');
      const call=state.privateMessages.find(m=>m.method==='unlock');
      if(!call || !key) throw new Error('No handoff observed');
      state.spy.postMessage({...key,sentAt:Date.now()});
      state.spy.postMessage({...call,sentAt:Date.now()});
      return JSON.stringify(state.privateMessages);
    });
    expect(messages).not.toContain(wallet.words);
    if(passphrase) expect(messages).not.toContain(passphrase);
    popup=await requested(site,transfer);
    await expect(popup.getByRole('button',{name:'Unlock to review',exact:true})).toBeVisible();
    await expect(page.getByRole('button',{name:'Unlock',exact:true}).first()).toBeVisible();
    await popup.getByRole('button',{name:'Reject',exact:true}).click();
    expect(await rpc.broadcasts()).toHaveLength(2);
  });
}

test('a preparation failure after unlocking stays visible and can retry without another unlock', async ({page,rpc,context,consoleGuard}) => {
  consoleGuard.allow(/status of 503/u);
  await openFundedWallet(page,(address)=>rpc.fund({address,amount:'50000000'}),{passphrase:'keep review open'});
  await enable(page);
  await page.getByRole('button',{name:'Lock',exact:true}).first().click();
  const site=await context.newPage();await connect(site);
  const popup=await requested(site,transfer);
  await consoleGuard.attach(popup);
  await popup.getByRole('button',{name:'Unlock to review',exact:true}).click();
  const dialog=popup.getByRole('dialog',{name:'Unlock for spending'});
  await dialog.getByLabel('Passphrase',{exact:true}).fill('keep review open');
  await rpc.failNext({path:'/node/info',mode:'status',status:503,body:'temporary failure'});
  await dialog.getByRole('button',{name:'Unlock',exact:true}).click();
  await expect(popup.getByText('The network server reported a problem. Try again in a moment.',{exact:true})).toBeVisible();
  expect(popup.isClosed()).toBe(false);
  await expect(popup.getByRole('dialog')).toHaveCount(0);
  await expect(popup.getByRole('button',{name:'Send now',exact:true})).toHaveCount(0);
  expect((await rpc.requests()).filter(r=>r.method==='POST')).toEqual([]);
  await quote(popup);
  await expect(popup.getByRole('dialog')).toHaveCount(0);
  expect((await rpc.requests()).filter(r=>r.method==='POST')).toEqual([]);
  await popup.getByRole('button',{name:'Reject',exact:true}).click();
  await expect.poll(()=>popup.isClosed()).toBe(true);
  expect(await rpc.broadcasts()).toEqual([]);
});

test('connecting only unlocks when its public addresses are unknown', async ({page,rpc,context}) => {
  const wallet=await openFundedWallet(page,(address)=>rpc.fund({address,amount:'5000000'}),{passphrase:'discover addresses'});
  await enable(page);
  await page.evaluate((id)=>{
    const root=JSON.parse(localStorage.getItem('mmx-wallet/v1')!);delete root.wallets[id].addresses;
    localStorage.setItem('mmx-wallet/v1',JSON.stringify(root));
  },wallet.id);
  await page.close();
  const site=await context.newPage();
  let popup=await start(site);
  await popup.getByRole('button',{name:'Unlock and connect',exact:true}).click();
  const dialog=popup.getByRole('dialog',{name:'Unlock for spending'});
  await dialog.getByLabel('Passphrase',{exact:true}).fill('discover addresses');
  await dialog.getByRole('button',{name:'Unlock',exact:true}).click();
  await expect.poll(()=>popup.isClosed()).toBe(true);
  await expect.poll(()=>site.evaluate(()=>(window as unknown as {account:{address:string}}).account?.address)).toBe(wallet.addresses[0]);
  popup=await start(site);
  await expect(popup.getByRole('button',{name:'Connect',exact:true})).toBeVisible();
  await expect(popup.getByRole('dialog')).toHaveCount(0);
  await popup.getByRole('button',{name:'Connect',exact:true}).click();
  await expect.poll(()=>popup.isClosed()).toBe(true);
  expect(await rpc.broadcasts()).toEqual([]);
});

test('a cold mobile request unlocks locally and shows a formatted transfer before confirmation', async ({
  page,
  rpc,
  context,
  consoleGuard,
}) => {
  await openFundedWallet(page, (a) => rpc.fund({ address: a, amount: '3000000' }), { passphrase: 'popup passphrase' });
  await enable(page);
  await page.close();
  const site = await context.newPage();
  let popup = await start(site);
  await popup.setViewportSize(MOBILE);
  await consoleGuard.attach(popup);
  await popup.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect.poll(() => popup.isClosed()).toBe(true);
  popup = await requested(site, transfer);
  await popup.setViewportSize(MOBILE);
  await consoleGuard.attach(popup);
  await expect(popup.locator('.connection__large-amount')).toContainText('1');
  await expect(popup.locator('.connection__recipient .address')).toContainText(TO);
  await expect(popup.locator('pre')).toHaveCount(0);
  await capture(popup, 'connection-transfer-mobile');
  await popup.getByRole('button', { name: 'Unlock to review', exact: true }).click();
  let dialog = popup.getByRole('dialog', { name: 'Unlock for spending' });
  await dialog.getByLabel('Passphrase', { exact: true }).fill('popup passphrase');
  await dialog.getByRole('button', { name: 'Unlock', exact: true }).click();
  await expect(popup.getByRole('heading', { name: 'Confirm transaction' })).toBeVisible();
  await capture(popup, 'connection-transfer-mobile-confirm');
  await popup.getByRole('button', { name: 'Send now', exact: true }).click();
  await expect.poll(() => popup.isClosed()).toBe(true);
  await expect.poll(() => result(site)).toMatchObject({ txid: expect.any(String) });
  // A fresh, independently unlocked popup must retain the first transaction's
  // inputs and maximum fee while the chain still reports its old balance.
  popup = await requested(site, transfer);
  await popup.getByRole('button', { name: 'Unlock to review', exact: true }).click();
  dialog = popup.getByRole('dialog', { name: 'Unlock for spending' });
  await dialog.getByLabel('Passphrase', { exact: true }).fill('popup passphrase');
  await dialog.getByRole('button', { name: 'Unlock', exact: true }).click();
  await expect(popup.getByText('Not enough funds for this amount and the network fee.',{exact:true})).toBeVisible();
  expect(popup.isClosed()).toBe(false);
  await expect(popup.getByRole('button',{name:'Send now',exact:true})).toHaveCount(0);
  await popup.getByRole('button',{name:'Reject',exact:true}).click();
  await expect.poll(() => popup.isClosed()).toBe(true);
  expect(await rpc.broadcasts()).toHaveLength(1);
});

test('a manual lock cancels a quote and closes the approval window', async ({ page, rpc, context }) => {
  await openFundedWallet(page, (a) => rpc.fund({ address: a, amount: '50000000' }));
  await enable(page);
  const site = await context.newPage();
  await connect(site);
  const popup = await requested(site, transfer);
  await quote(popup);
  await page.getByRole('button', { name: 'Lock', exact: true }).first().click();
  await expect.poll(() => error(site)).toBe('locked');
  await expect.poll(() => popup.isClosed()).toBe(true);
  expect(await rpc.broadcasts()).toEqual([]);
});

test('disabling connections cancels approval and revokes the connection even after re-enabling', async ({ page, rpc, context }) => {
  await openFundedWallet(page, (a) => rpc.fund({ address: a, amount: '50000000' }));
  await enable(page);
  const site = await context.newPage();
  await connect(site);
  let popup = await requested(site, transfer);
  await quote(popup);
  const toggle = page.getByRole('switch', { name: 'Allow website connections' });
  await toggle.click();
  await expect.poll(() => error(site)).toBe('website_connections_disabled');
  await expect.poll(() => popup.isClosed()).toBe(true);
  expect(await rpc.broadcasts()).toEqual([]);
  await expect(toggle).toBeEnabled();
  await toggle.click();
  await expect(toggle).toBeEnabled();
  popup = await requested(site, transfer);
  await expect.poll(() => error(site)).toBe('disconnected');
  await expect.poll(() => popup.isClosed()).toBe(true);
});

test('blocking a site closes its popup and only a new connection can resume after unblocking', async ({ page, rpc, context }) => {
  await openFundedWallet(page, (a) => rpc.fund({ address: a, amount: '50000000' }));
  await enable(page);
  const site = await context.newPage();
  await connect(site);
  let popup = await requested(site, transfer);
  await quote(popup);
  await popup.getByRole('button', { name: 'Block this website' }).click();
  await expect.poll(() => error(site)).toBe('website_blocked');
  await expect.poll(() => popup.isClosed()).toBe(true);
  await expect(page.getByRole('button', { name: `Unblock ${DAPP}` })).toBeVisible();
  expect(await rpc.broadcasts()).toEqual([]);
  popup = await start(site);
  await expect.poll(() => error(site)).toBe('website_blocked');
  await expect.poll(() => popup.isClosed()).toBe(true);
  await page.getByRole('button', { name: `Unblock ${DAPP}` }).click();
  await connect(site);
});

test('the website URL input fills its settings row on desktop and mobile', async ({ page, rpc }) => {
  await openFundedWallet(page, (a) => rpc.fund({ address: a, amount: '50000000' }));
  await goTo(page, 'Settings');
  for (const width of [1280, 540, 390, 320]) {
    await page.setViewportSize({ width, height: 900 });
    const row = page.getByRole('group', { name: 'Block a website', exact: true });
    const control = row.locator('.input');
    await row.scrollIntoViewIfNeeded();
    const outer = await row.boundingBox(),
      input = await control.boundingBox();
    expect(input!.width).toBeGreaterThan(outer!.width - 4);
    await capture(page, `website-settings-${width}`);
  }
});
