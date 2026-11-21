import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { sanitizeSettings } from '../../src/lib/appstate';
import { websiteAccess, websiteOrigin } from '../../src/lib/websites';
import { boundedJson, envelope, messageRequest, transactionRequest } from '../../src/connect/protocol';
import { connect } from '../../src/connect/sdk';
import { WebsiteSessions, WEBSITE_SESSION_TTL } from '../../src/connect/sessions';
import { memoryStorage } from '../../src/backend/web/memory-storage';
import { testAddress } from '../e2e/mock-rpc';

const SITE = 'https://example.com';
const WALLET = 'https://wallet.mmx.network';
const ID = 'a'.repeat(32);
const ACCOUNT = { walletId: '123', address: 'test-address', addresses: ['test-address'], network: 'testnet' };

describe('website permissions and message bounds', () => {
  it('accepts exact UTF-8 messages and rejects lone UTF-16 surrogates before signing', () => {
    for (const message of ['hello\nworld', '😀'.repeat(1024), '\ufffd'])
      expect(messageRequest({ network: 'mainnet', message })).toBe(true);
    for (const message of ['', '😀'.repeat(1025), '\ud800', '\udfff', 'hello\ud800world'])
      expect(messageRequest({ network: 'mainnet', message })).toBe(false);
  });
  it('defaults off for missing, malformed and desktop settings; only explicit true opts in', () => {
    for (const value of [undefined, null, 1, 'true', false]) {
      expect(websiteAccess(sanitizeSettings({ allowWebsiteConnections: value }, 'web'), SITE)).toBe('website_connections_disabled');
    }
    expect(websiteAccess(sanitizeSettings({ allowWebsiteConnections: true }, 'web'), SITE)).toBeNull();
    expect(websiteAccess(sanitizeSettings({ allowWebsiteConnections: true }, 'desktop'), SITE)).toBe('website_connections_disabled');
  });
  it('blocks exact origins, keeps the list while disabled and sanitizes stored entries', () => {
    const settings = sanitizeSettings(
      { allowWebsiteConnections: true, blockedWebsites: [SITE, SITE, `${SITE}/path`, 'http://example.com', {}, 'https://elsewhere.com'] },
      'web',
    );
    expect(settings.blockedWebsites).toEqual([SITE, 'https://elsewhere.com']);
    expect(websiteAccess(settings, SITE)).toBe('website_blocked');
    expect(websiteAccess(settings, 'https://example.com:443')).toBe('invalid_argument');
    expect(websiteAccess(settings, 'https://example.com:444')).toBeNull();
    expect(websiteAccess(settings, 'https://sub.example.com')).toBeNull();
    expect(websiteAccess({ ...settings, allowWebsiteConnections: false }, SITE)).toBe('website_blocked');
    expect(websiteOrigin('http://127.0.0.1:8000')).toBe('http://127.0.0.1:8000');
    expect(websiteOrigin('https://user:pass@example.com')).toBeNull();
  });
  it('rejects protocol confusion, non-JSON trees, excessive work and unsafe numbers', () => {
    const message = { protocol: 'mmx-wallet', version: 2, channel: ID, type: 'request' };
    expect(envelope(message, ID)).toBe(true);
    expect(envelope({ ...message, version: 1 }, ID)).toBe(false);
    expect(envelope(message, 'b'.repeat(32))).toBe(false);
    const loop: Record<string, unknown> = {};
    loop.self = loop;
    for (const v of [
      loop,
      { n: NaN },
      { n: Infinity },
      { n: 2 ** 53 },
      new Date(),
      { bad: undefined },
      Array(5000).fill(1),
      'ü'.repeat(32769),
    ])
      expect(boundedJson(v)).toBe(false);
    let deep: unknown = 0;
    for (let i = 0; i < 34; i++) deep = [deep];
    expect(boundedJson(deep)).toBe(false);
    expect(transactionRequest({ network: 'test', transaction: { execute: [] }, feeLevel: 1, gasLimit: 0 })).toBe(true);
    for (const extra of [{ feeLevel: 4 }, { gasLimit: -1 }, { gasLimit: 2 ** 32 }, { unexpected: true }])
      expect(transactionRequest({ network: 'test', transaction: {}, ...extra })).toBe(false);
  });
});

describe('public website connection grants', () => {
  const account = { ...ACCOUNT, address: testAddress('grant'), addresses: [testAddress('grant')], decimals: 6 };
  it('binds grants to the approved origin/account, expires and revokes them without storing secrets', () => {
    let now = 1_800_000_000_000;
    const storage = memoryStorage();
    const sessions = new WebsiteSessions(storage, () => now);
    const first = sessions.create(SITE, account, 'Main');
    const second = sessions.create('https://elsewhere.com', account, 'Main');
    expect(sessions.get(first.id, SITE)?.account).toEqual(account);
    expect(sessions.get(first.id, 'https://evil.example')).toBeNull();
    sessions.revoke(SITE);
    expect(sessions.get(first.id, SITE)).toBeNull();
    expect(sessions.get(second.id, 'https://elsewhere.com')).not.toBeNull();
    now += WEBSITE_SESSION_TTL + 1;
    expect(sessions.get(second.id, 'https://elsewhere.com')).toBeNull();
    expect(JSON.stringify(storage.dump())).not.toMatch(/seed|passphrase|signature/);
  });
  it('does not restore revoked grants after the system clock moves backwards', () => {
    let now=1_800_000_000_000;
    const sessions=new WebsiteSessions(memoryStorage(),()=>now);
    const first=sessions.create(SITE,account,'Main');
    now-=2*WEBSITE_SESSION_TTL;
    expect(sessions.get(first.id,SITE)).toBeNull();
    sessions.revoke(SITE);
    now+=2*WEBSITE_SESSION_TTL;
    expect(sessions.get(first.id,SITE)).toBeNull();
  });
  it('bounds retained grants and fails closed for damaged storage', () => {
    const storage = memoryStorage();
    const sessions = new WebsiteSessions(storage);
    const first = sessions.create(SITE, account, 'Main');
    for (let i = 0; i < 40; i++) sessions.create(SITE, account, 'Main');
    expect(sessions.get(first.id, SITE)).toBeNull();
    expect(JSON.parse(Object.values(storage.dump())[0]!)).toHaveLength(32);
    sessions.revoke();
    expect(storage.dump()).toEqual({});
  });
});

describe('short-lived popup SDK', () => {
  type Popup = { closed: boolean; postMessage: ReturnType<typeof vi.fn>; close: ReturnType<typeof vi.fn> };
  let popup: Popup, open: ReturnType<typeof vi.fn>;
  let listeners: Map<string, Set<(e: MessageEvent) => void>>;
  let channel: string;
  function receive(data: Record<string, unknown>, opts: { origin?: string; source?: unknown; channel?: string } = {}) {
    for (const fn of listeners.get('message') ?? [])
      fn({
        source: opts.source ?? popup,
        origin: opts.origin ?? WALLET,
        data: { protocol: 'mmx-wallet', version: 2, channel: opts.channel ?? channel, ...data },
      } as MessageEvent);
  }
  function hello() {
    channel = new URLSearchParams(new URL(open.mock.calls.at(-1)![0]).hash.slice(1)).get('channel')!;
    receive({ type: 'ready' });
    return popup.postMessage.mock.calls.at(-1)![0] as { id: string; action: string; params: unknown; session?: string };
  }
  async function connected() {
    const promise = connect();
    const h = hello();
    receive({ type: 'result', id: h.id, result: { account: ACCOUNT, session: ID, expires: Date.now() + 3_600_000 } });
    return promise;
  }
  beforeEach(() => {
    vi.useFakeTimers();
    listeners = new Map();
    open = vi.fn(() => {
      const next: Popup = { closed: false, postMessage: vi.fn(), close: vi.fn() };
      next.close.mockImplementation(() => {
        next.closed = true;
      });
      popup = next;
      return next;
    });
    vi.stubGlobal('window', {
      location: { href: `${SITE}/app`, origin: SITE },
      open,
      addEventListener: (name: string, fn: (e: MessageEvent) => void) => {
        const set = listeners.get(name) ?? new Set();
        set.add(fn);
        listeners.set(name, set);
      },
      removeEventListener: (name: string, fn: (e: MessageEvent) => void) => listeners.get(name)?.delete(fn),
    });
  });
  afterEach(() => {
    vi.clearAllTimers();
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });
  it('binds the handshake to exact window, origin and channel and closes after connecting', async () => {
    const p = connect();
    let done = false;
    void p.then(() => {
      done = true;
    });
    channel = new URLSearchParams(new URL(open.mock.calls[0]![0]).hash.slice(1)).get('channel')!;
    receive({ type: 'ready' }, { origin: 'https://evil.example' });
    receive({ type: 'ready' }, { source: {} });
    receive({ type: 'ready' }, { channel: ID });
    expect(popup.postMessage).not.toHaveBeenCalled();
    const h = hello();
    expect(h.action).toBe('connect');
    expect(popup.postMessage.mock.calls[0]![1]).toBe(WALLET);
    receive({ type: 'result', id: 'b'.repeat(32), result: { account: ACCOUNT } });
    await Promise.resolve();
    expect(done).toBe(false);
    receive({ type: 'result', id: h.id, result: { account: ACCOUNT, session: ID, expires: Date.now() + 3_600_000 } });
    const c = await p;
    expect(c.account).toEqual(ACCOUNT);
    expect(popup.closed).toBe(true);
    expect(vi.getTimerCount()).toBe(0);
    c.disconnect();
    expect(listeners.get('pagehide')?.size).toBe(0);
  });
  it('opens one fresh popup for each transaction and survives approval/rejection closes', async () => {
    const c = await connected();
    const connectionPopup = popup;
    const tx = { execute: [{ method: 'call', args: ['10000000000000000000'] }] };
    const first = c.sendTransaction(tx);
    const h = hello();
    const firstPopup = popup;
    expect(popup).not.toBe(connectionPopup);
    expect(h).toMatchObject({ action: 'sendTransaction', session: ID, params: { network: 'testnet', transaction: tx } });
    await expect(c.sendTransaction(tx)).rejects.toMatchObject({ code: 'busy' });
    expect(open).toHaveBeenCalledTimes(2);
    receive({ type: 'result', id: h.id, result: { txid: 'F'.repeat(64), signature: 'never-return' } });
    expect(await first).toEqual({ txid: 'F'.repeat(64) });
    expect(firstPopup.closed).toBe(true);
    const second = c.sendTransaction(tx);
    const next = hello();
    expect(popup).not.toBe(firstPopup);
    receive({ type: 'result', id: next.id, error: { code: 'user_rejected' } });
    await expect(second).rejects.toMatchObject({ code: 'user_rejected' });
    expect(popup.closed).toBe(true);
    expect(vi.getTimerCount()).toBe(0);
    c.disconnect();
  });
  it('rejects lossy message encoding before opening an approval popup', async () => {
    const c = await connected();
    for (const message of ['\ud800', '\udfff', 'hello\ud800world']) {
      await expect(c.signMessage(message)).rejects.toMatchObject({ code: 'invalid_request' });
    }
    expect(open).toHaveBeenCalledTimes(1);
    c.disconnect();
  });
  it('does not open another window for malformed payloads, overlapping connects or disconnect', async () => {
    const pending = connect();
    hello();
    await expect(connect()).rejects.toMatchObject({ code: 'busy' });
    expect(open).toHaveBeenCalledTimes(1);
    receive({
      type: 'result',
      id: popup.postMessage.mock.calls[0]![0].id,
      result: { account: ACCOUNT, session: ID, expires: Date.now() + 3_600_000 },
    });
    const c = await pending;
    const loop: Record<string, unknown> = {};
    loop.self = loop;
    for (const tx of [loop, { args: [NaN] }, { value: 2 ** 53 }, { payload: 'x'.repeat(65537) }])
      await expect(c.sendTransaction(tx)).rejects.toMatchObject({ code: 'invalid_request' });
    expect(open).toHaveBeenCalledTimes(1);
    c.disconnect();
    expect(open).toHaveBeenCalledTimes(1);
    await expect(c.sendTransaction({})).rejects.toMatchObject({ code: 'disconnected' });
  });
  it('closes on a timeout without retrying; a close while submitting reports uncertainty', async () => {
    const c = await connected();
    const pending = c.sendTransaction({});
    hello();
    const failure = expect(pending).rejects.toMatchObject({ code: 'timeout' });
    await vi.advanceTimersByTimeAsync(120_000);
    await failure;
    expect(popup.closed).toBe(true);
    expect(open).toHaveBeenCalledTimes(2);
    const sending = c.sendTransaction({});
    const h = hello();
    receive({ type: 'submitting', id: h.id });
    popup.closed = true;
    const uncertain = expect(sending).rejects.toMatchObject({ code: 'tx_uncertain' });
    await vi.advanceTimersByTimeAsync(500);
    await uncertain;
    c.disconnect();
  });
  it('closes denied connection windows and validates URL options', async () => {
    await expect(connect({ walletUrl: 'bad url' })).rejects.toMatchObject({ code: 'invalid_wallet_url' });
    await expect(connect({ walletUrl: 'http://example.com/' })).rejects.toMatchObject({ code: 'invalid_wallet_url' });
    const pending = connect();
    const h = hello();
    receive({ type: 'result', id: h.id, error: { code: 'website_connections_disabled' } });
    await expect(pending).rejects.toMatchObject({ code: 'website_connections_disabled' });
    expect(popup.closed).toBe(true);
    expect(vi.getTimerCount()).toBe(0);
  });
});
