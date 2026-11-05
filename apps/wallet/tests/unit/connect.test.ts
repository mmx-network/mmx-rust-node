import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { sanitizeSettings } from '../../src/lib/appstate';
import { websiteAccess, websiteOrigin } from '../../src/lib/websites';
import { boundedJson, envelope, transactionRequest } from '../../src/connect/protocol';
import { connect } from '../../src/connect/sdk';

const SITE = 'https://example.com';
const WALLET = 'https://wallet.mmx.network';
const ID = 'a'.repeat(32);
const ACCOUNT = { walletId: '123', address: 'test-address', addresses: ['test-address'], network: 'testnet' };

describe('website permissions and message bounds', () => {
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
    const message = { protocol: 'mmx-wallet', version: 1, channel: ID, type: 'request' };
    expect(envelope(message, ID)).toBe(true);
    expect(envelope({ ...message, version: 2 }, ID)).toBe(false);
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

describe('standalone popup SDK', () => {
  let popup: { closed: boolean; postMessage: ReturnType<typeof vi.fn>; focus: ReturnType<typeof vi.fn>; close: ReturnType<typeof vi.fn> };
  let open: ReturnType<typeof vi.fn>;
  let listeners: Map<string, (e: MessageEvent) => void>;
  let channel: string;
  function receive(data: Record<string, unknown>, opts: { origin?: string; source?: unknown; channel?: string } = {}) {
    listeners.get('message')?.({
      source: opts.source ?? popup,
      origin: opts.origin ?? WALLET,
      data: { protocol: 'mmx-wallet', version: 1, channel: opts.channel ?? channel, ...data },
    } as MessageEvent);
  }
  function hello() {
    receive({ type: 'ready' });
    const message = popup.postMessage.mock.calls.at(-1)![0] as { id: string };
    return message.id;
  }
  async function connected() {
    const p = connect();
    channel = new URL(open.mock.calls[0]![0]).hash.match(/channel=([^&]+)/)![1]!;
    const id = hello();
    receive({ type: 'connected', id, account: ACCOUNT });
    return p;
  }
  beforeEach(() => {
    vi.useFakeTimers();
    listeners = new Map();
    popup = { closed: false, postMessage: vi.fn(), focus: vi.fn(), close: vi.fn() };
    open = vi.fn(() => popup);
    vi.stubGlobal('window', {
      location: { href: `${SITE}/app`, origin: SITE },
      open,
      addEventListener: (name: string, fn: (e: MessageEvent) => void) => listeners.set(name, fn),
      removeEventListener: (name: string) => listeners.delete(name),
    });
  });
  afterEach(() => {
    vi.clearAllTimers();
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });
  it('binds the handshake to the popup, exact origin and unpredictable channel', async () => {
    const p = connect();
    let done = false;
    void p.then(() => {
      done = true;
    });
    const url = new URL(open.mock.calls[0]![0]);
    channel = new URLSearchParams(url.hash.slice(1)).get('channel')!;
    expect(url.pathname).toBe('/connect.html');
    expect(url.hash).not.toContain('transaction');
    receive({ type: 'ready' }, { origin: 'https://evil.example' });
    receive({ type: 'ready' }, { source: {} });
    receive({ type: 'ready' }, { channel: ID });
    expect(popup.postMessage).not.toHaveBeenCalled();
    const id = hello();
    expect(popup.postMessage.mock.calls[0]![1]).toBe(WALLET);
    receive({ type: 'connected', id: 'b'.repeat(32), account: ACCOUNT });
    await Promise.resolve();
    expect(done).toBe(false);
    receive({ type: 'connected', id, account: ACCOUNT });
    const c = await p;
    expect(c.account).toEqual(ACCOUNT);
    c.disconnect();
    expect(listeners.size).toBe(0);
    expect(vi.getTimerCount()).toBe(0);
  });
  it('returns only a txid, serializes requests and never retries a timed out transaction', async () => {
    const c = await connected();
    const tx = { execute: [{ method: 'call', args: ['10000000000000000000'] }] };
    const p = c.sendTransaction(tx);
    const request = popup.postMessage.mock.calls.at(-1)![0] as { id: string; params: unknown };
    expect(request.params).toEqual({ network: 'testnet', transaction: tx });
    await expect(c.sendTransaction(tx)).rejects.toMatchObject({ code: 'busy' });
    receive({ type: 'result', id: request.id, result: { txid: 'F'.repeat(64), signature: 'never-return' } });
    expect(await p).toEqual({ txid: 'F'.repeat(64) });
    const failed = c.sendTransaction(tx);
    const failure = expect(failed).rejects.toMatchObject({ code: 'timeout' });
    await vi.advanceTimersByTimeAsync(120000);
    await failure;
    const requests = popup.postMessage.mock.calls.filter(([m]) => m.type === 'request');
    expect(requests).toHaveLength(2);
    expect(popup.postMessage.mock.calls.at(-1)![0].type).toBe('cancel');
    c.disconnect();
  });
  it('rejects malformed payloads locally and propagates permission revocation and window closure', async () => {
    const c = await connected();
    const loop: Record<string, unknown> = {};
    loop.self = loop;
    for (const tx of [loop, { args: [NaN] }, { value: 2 ** 53 }, { payload: 'x'.repeat(65537) }])
      await expect(c.sendTransaction(tx)).rejects.toMatchObject({ code: 'invalid_request' });
    const p = c.sendTransaction({});
    const id = popup.postMessage.mock.calls.at(-1)![0].id;
    receive({ type: 'result', id, error: { code: 'website_blocked', message: 'Blocked' } });
    await expect(p).rejects.toMatchObject({ code: 'website_blocked' });
    const pending = c.sendTransaction({});
    const failed = expect(pending).rejects.toMatchObject({ code: 'disconnected' });
    popup.closed = true;
    await vi.advanceTimersByTimeAsync(1000);
    await failed;
    expect(listeners.size).toBe(0);
  });
  it('reports a disabled connection without waiting for timeout and rejects invalid URLs', async () => {
    await expect(connect({ walletUrl: 'bad url' })).rejects.toMatchObject({ code: 'invalid_wallet_url' });
    await expect(connect({ walletUrl: 'http://example.com/' })).rejects.toMatchObject({ code: 'invalid_wallet_url' });
    const p = connect();
    channel = new URLSearchParams(new URL(open.mock.calls[0]![0]).hash.slice(1)).get('channel')!;
    const id = hello();
    receive({ type: 'result', id, error: { code: 'website_connections_disabled' } });
    await expect(p).rejects.toMatchObject({ code: 'website_connections_disabled' });
    expect(vi.getTimerCount()).toBe(0);
  });
});
