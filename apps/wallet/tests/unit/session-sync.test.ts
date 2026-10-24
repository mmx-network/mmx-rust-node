/**
 * Refresh scheduling and session bookkeeping (src/state/session.ts) against
 * the mock backend: balance refetches after failed syncs, retry backoff,
 * "Received" announcements, address changes, wallet switches, local sends
 * across a lock, and token verification.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { encodeAddress, NATIVE_CONTRACT } from '../../src/lib/address';

const START = 1_792_835_101_709;
const FOREIGN = encodeAddress(new Uint8Array(32).fill(0x5a));

type State = typeof import('../../src/state');
type Session = typeof import('../../src/state/session');
type Mock = typeof import('../../src/backend/mock');
type MockBackend = ReturnType<Mock['createMockBackend']>;
type Types = typeof import('../../src/backend/types');

beforeEach(() => {
  vi.useFakeTimers({ now: START, toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval', 'Date'] });
});

afterEach(() => {
  vi.clearAllTimers();
  vi.useRealTimers();
});

interface Ctx {
  S: State;
  session: Session;
  M: Mock;
  backend: MockBackend;
  types: Types;
}

async function boot(prepare?: (backend: MockBackend) => void): Promise<Ctx> {
  vi.resetModules();
  const S: State = await import('../../src/state');
  const session: Session = await import('../../src/state/session');
  const M: Mock = await import('../../src/backend/mock');
  const types: Types = await import('../../src/backend/types');
  const backend = M.createMockBackend({ platform: 'desktop' });
  prepare?.(backend);
  await S.init({ backend });
  await settle(1200);
  return { S, session, M, backend, types };
}

async function settle(ms = 200): Promise<void> {
  await vi.advanceTimersByTimeAsync(ms);
}

/** Wraps the backend's chain requests; `hook` may answer or throw instead. */
function intercept(backend: MockBackend, hook: (path: string, real: (p: string) => Promise<unknown>) => Promise<unknown> | undefined) {
  const real = backend.rpcGet.bind(backend);
  return vi.spyOn(backend, 'rpcGet').mockImplementation(async (path: string) => {
    const answer = hook(path, real);
    return answer === undefined ? real(path) : answer;
  });
}

function txid(tag: string, i = 0): string {
  return (tag.repeat(4) + i.toString(16).padStart(8, '0')).padEnd(64, 'A').toUpperCase().slice(0, 64);
}

function receive(backend: MockBackend, address: string, amount: bigint, id: string, heightOffset = 0): void {
  backend.chain.record({
    height: backend.chain.height() - heightOffset,
    time: Date.now() - heightOffset * 10_000,
    txid: id,
    type: 'RECEIVE',
    address,
    contract: NATIVE_CONTRACT,
    amount,
  });
}

function chainTotal(backend: MockBackend, addresses: readonly string[]): bigint {
  return addresses.reduce((sum, a) => sum + backend.chain.balance(a, NATIVE_CONTRACT), 0n);
}

function receivedToasts(S: State): string[] {
  return S.toasts.value.filter((t) => t.message.startsWith('Received')).map((t) => t.message);
}

describe('balances after a failed sync', () => {
  it('refetches the balance of an address whose new rows arrived in a sync that failed', async () => {
    const { S, backend, types } = await boot();
    const addrs = S.addresses.value;
    const target = addrs[1]!;
    const flaky = addrs[5]!;
    let failFlaky = true;
    intercept(backend, (path) => {
      if (failFlaky && path.startsWith(`/address/history?id=${flaky}&`)) {
        failFlaky = false;
        return Promise.reject(new types.BackendError('rpc_timeout'));
      }
      return undefined;
    });
    const before = S.nativeAsset.value!.total;
    receive(backend, target, 7_000_000n, txid('C'));
    await settle(30_000);
    expect(failFlaky).toBe(false);
    await settle(30_000);
    expect(S.chainError.value).toBeNull();
    expect(S.activity.value.some((i) => i.txid === txid('C'))).toBe(true);
    // Well before the 5-minute full refresh, the total matches the chain.
    expect(S.nativeAsset.value!.total).toBe(chainTotal(backend, addrs));
    expect(S.nativeAsset.value!.total - before).toBeGreaterThanOrEqual(7_000_000n);
  });

  it('keeps a full balance snapshot when only history failed', async () => {
    const { S, backend, types } = await boot();
    const addrs = S.addresses.value;
    const broken = addrs[2]!;
    const probe = addrs[7]!;
    let probeFetches = 0;
    intercept(backend, (path) => {
      if (path.startsWith(`/address/history?id=${broken}&`) && path.includes('&since=')) {
        return Promise.reject(new types.BackendError('rpc_http_error', 'HTTP 500'));
      }
      if (path.startsWith(`/address?id=${probe}&`)) probeFetches++;
      return undefined;
    });
    // Retries at about 20, 40, 80, 160 and 320 s; the last one is due for all balances.
    await settle(330_000);
    expect(probeFetches).toBe(1);
    // A retry right after it does not fetch every balance again.
    S.refreshNow();
    await settle(1000);
    expect(probeFetches).toBe(1);
  });
});

describe('retry backoff', () => {
  it('spaces failing syncs at least 20 s apart and increasingly further', async () => {
    const { S, backend, types } = await boot();
    const broken = S.addresses.value[2]!;
    const attempts: number[] = [];
    intercept(backend, (path) => {
      if (!path.startsWith(`/address/history?id=${broken}&`) || !path.includes('&since=')) return undefined;
      attempts.push(Date.now());
      return Promise.reject(new types.BackendError('rpc_http_error', 'HTTP 500'));
    });
    await settle(150_000);
    expect(S.chainError.value).toBe('rpc_http_error');
    expect(attempts.length).toBeGreaterThanOrEqual(3);
    expect(attempts.length).toBeLessThanOrEqual(5);
    const gaps = attempts.slice(1).map((t, i) => t - attempts[i]!);
    for (const gap of gaps) expect(gap).toBeGreaterThanOrEqual(20_000);
    for (let i = 1; i < gaps.length; i++) expect(gaps[i]!).toBeGreaterThan(gaps[i - 1]!);
    // Refreshing by hand still retries at once.
    const n = attempts.length;
    S.refreshNow();
    await settle(100);
    expect(attempts.length).toBe(n + 1);
  });

  it('a refresh by hand during a failing retry runs again as soon as it ends', async () => {
    const { S, backend, types } = await boot();
    const broken = S.addresses.value[2]!;
    const attempts: number[] = [];
    intercept(backend, (path) => {
      if (!path.startsWith(`/address/history?id=${broken}&`) || !path.includes('&since=')) return undefined;
      attempts.push(Date.now());
      return new Promise((_, reject) => setTimeout(() => reject(new types.BackendError('rpc_http_error', 'HTTP 500')), 5000));
    });
    // Several failures: the next automatic retry is minutes away.
    await settle(200_000);
    const n = attempts.length;
    while (attempts.length === n) await settle(1000);
    // A retry is running; "Try again" now must not wait for the next delay.
    S.refreshNow();
    await settle(6000);
    expect(attempts.length).toBe(n + 2);
    expect(attempts[n + 1]! - attempts[n]!).toBeLessThan(6000);
  });

  it('starts over at the minimum interval after a success', async () => {
    const { S, backend, types } = await boot();
    const broken = S.addresses.value[2]!;
    let failing = true;
    const attempts: number[] = [];
    intercept(backend, (path) => {
      if (!path.startsWith(`/address/history?id=${broken}&`) || !path.includes('&since=')) return undefined;
      attempts.push(Date.now());
      return failing ? Promise.reject(new types.BackendError('rpc_timeout')) : undefined;
    });
    await settle(100_000);
    failing = false;
    await settle(120_000);
    expect(S.chainError.value).toBeNull();
    const last = attempts.slice(-3);
    expect(last[2]! - last[1]!).toBeLessThanOrEqual(30_000);
  });
});

describe('received announcements', () => {
  it('does not announce older transfers revealed while a sync waits for older history', async () => {
    const { S, backend } = await boot();
    expect(S.balanceSeries.value?.complete).toBe(true);
    let release!: () => void;
    const gate = new Promise<void>((resolve) => (release = resolve));
    let held = 0;
    intercept(backend, (path, real) => {
      if (!path.includes('&until=')) return undefined;
      held++;
      return gate.then(() => real(path));
    });
    const activityBefore = S.activity.value.length;
    S.setSeriesRange('ALL');
    await settle(300);
    expect(held).toBeGreaterThan(0);
    S.refreshNow();
    await settle(300);
    release();
    await settle(5_000);
    expect(S.activity.value.filter((i) => i.kind === 'received').length).toBeGreaterThan(0);
    expect(S.activity.value.length).toBeGreaterThan(activityBefore);
    expect(receivedToasts(S)).toEqual([]);

    // A transfer that really arrives is still announced, once.
    receive(backend, S.addresses.value[1]!, 7_000_000n, txid('D'));
    await settle(30_000);
    expect(receivedToasts(S)).toEqual(['Received 7 MMX']);
    await settle(60_000);
    expect(receivedToasts(S)).toEqual(['Received 7 MMX']);
  });

  it('does not announce the history of an address added to the wallet', async () => {
    const { S, backend } = await boot();
    const id = S.activeWallet.value!.id;
    const extra = (await backend.setAddressCount(id, 12))[11]!;
    await backend.setAddressCount(id, 10);
    receive(backend, extra, 3_000_000n, txid('E'), 2);
    const done = S.setAddressCount(12);
    await settle(500);
    await done;
    await settle(60_000);
    expect(S.activity.value.some((i) => i.txid === txid('E'))).toBe(true);
    expect(receivedToasts(S)).toEqual([]);
  });

  it('does not announce rows again that a lagging server hid for a while', async () => {
    const { S, backend } = await boot();
    const target = S.addresses.value[1]!;
    receive(backend, target, 2_000_000n, txid('F'));
    await settle(30_000);
    expect(receivedToasts(S)).toEqual(['Received 2 MMX']);
    S.clearToasts();
    // The next answers come from a server that has not seen that block yet.
    let lagging = true;
    intercept(backend, (path) => (lagging && path.startsWith(`/address/history?id=${target}&`) ? Promise.resolve([]) : undefined));
    await settle(30_000);
    expect(S.activity.value.some((i) => i.txid === txid('F'))).toBe(false);
    lagging = false;
    await settle(60_000);
    expect(S.activity.value.some((i) => i.txid === txid('F'))).toBe(true);
    expect(receivedToasts(S)).toEqual([]);
  });
});

describe('address changes', () => {
  async function withBusyNewAddress(): Promise<Ctx & { extra: string }> {
    const ctx = await boot();
    const id = ctx.S.activeWallet.value!.id;
    const extra = (await ctx.backend.setAddressCount(id, 20))[15]!;
    await ctx.backend.setAddressCount(id, 10);
    for (let i = 0; i < 40; i++) receive(ctx.backend, extra, 1_000_000n, txid('B', i), i + 1);
    return { ...ctx, extra };
  }

  it('keeps loaded activity and the chart complete when an added address has many recent rows', async () => {
    const { S } = await withBusyNewAddress();
    expect(S.balanceSeries.value!.complete).toBe(true);
    const before = S.activity.value.length;
    const done = S.setAddressCount(20);
    await settle(500);
    await done;
    expect(S.activity.value.length).toBeGreaterThanOrEqual(before + 40);
    await settle(120_000);
    expect(S.activity.value.length).toBeGreaterThanOrEqual(before + 40);
    expect(S.balanceSeries.value!.complete).toBe(true);
  });

  it('covers the chart range again when an added address could only load its first page', async () => {
    const { S, backend, types, extra } = await withBusyNewAddress();
    let failOlder = true;
    intercept(backend, (path) => {
      if (failOlder && path.startsWith(`/address/history?id=${extra}&`) && path.includes('&until=')) {
        failOlder = false;
        return Promise.reject(new types.BackendError('rpc_timeout'));
      }
      return undefined;
    });
    const before = S.activity.value.length;
    const done = S.setAddressCount(20);
    await settle(500);
    await done;
    expect(failOlder).toBe(false);
    await settle(2000);
    expect(S.balanceSeries.value!.complete).toBe(true);
    expect(S.activity.value.length).toBeGreaterThanOrEqual(before + 40);
  });
});

describe('switching wallets', () => {
  it('does not leave "load more" or the chart loading after a switch mid-request', async () => {
    const { S, backend } = await boot();
    const main = new Set(S.addresses.value);
    let release!: () => void;
    const gate = new Promise<void>((resolve) => (release = resolve));
    intercept(backend, (path, real) => {
      const id = /id=([^&]+)/.exec(path)?.[1];
      if (!path.includes('&until=') || !id || !main.has(id)) return undefined;
      return gate.then(() => real(path));
    });
    S.setSeriesRange('ALL');
    for (let i = 0; i < 20 && !S.activityLoadingMore.value; i++) {
      void S.loadMoreActivity();
      await settle(10);
    }
    expect(S.activityLoadingMore.value).toBe(true);
    expect(S.seriesLoading.value).toBe(true);
    const trading = S.wallets.value.find((w) => w.name === 'Trading')!;
    await S.selectWallet(trading.id);
    await settle(1000);
    expect(S.activityLoadingMore.value).toBe(false);
    release();
    await settle(2000);
    expect(S.activityLoadingMore.value).toBe(false);
    expect(S.seriesLoading.value).toBe(false);
    await S.loadMoreActivity();
    expect(S.visibleActivity.value.length).toBeGreaterThan(S.ACTIVITY_PAGE);
  });
});

describe('local sends', () => {
  function pending(walletId: string, over: Record<string, unknown> = {}) {
    return {
      txid: txid('7'),
      walletId,
      to: FOREIGN,
      contract: NATIVE_CONTRACT,
      symbol: 'MMX',
      decimals: 6,
      amount: 1_000_000n,
      fee: 50_000n,
      expires: 1e9,
      createdAt: Date.now(),
      status: 'pending' as const,
      ...over,
    };
  }

  it('keeps sends still on their way when wallets lock; removal drops everything', async () => {
    const { session } = await boot();
    session.trackPending(pending('w1'));
    session.trackPending(pending('w1', { txid: txid('8'), status: 'expired', settledAt: Date.now() }));
    session.trackPending(pending('w2', { txid: txid('9') }));
    session.forgetWalletData(new Set(['w1']));
    expect(session.pendingTxs.value.map((p) => p.txid).sort()).toEqual([txid('7'), txid('9')].sort());
    session.forgetWalletData('all');
    expect(session.pendingTxs.value).toHaveLength(2);
    session.forgetWalletData(new Set(['w1']), { keepPending: false });
    expect(session.pendingTxs.value.map((p) => p.txid)).toEqual([txid('9')]);
  });

  it('shows a just-sent transfer again after lock and unlock', async () => {
    const { S, M, session } = await boot();
    const savings = S.wallets.value.find((w) => w.name === 'Savings')!;
    await S.selectWallet(savings.id);
    await S.unlockWallet(savings.id, { passphrase: M.MOCK_PASSPHRASE });
    await settle(1000);
    session.trackPending(pending(savings.id, { uncertain: true }));
    expect(S.walletPending.value).toHaveLength(1);
    await S.lock();
    await settle(10);
    expect(S.walletPending.value).toHaveLength(0);
    await S.unlockWallet(savings.id, { passphrase: M.MOCK_PASSPHRASE });
    await settle(1000);
    expect(S.walletPending.value).toHaveLength(1);
    expect(S.walletPending.value[0]).toMatchObject({ txid: txid('7'), status: 'pending', uncertain: true });
    expect(S.activity.value[0]).toMatchObject({ local: true, status: 'pending', kind: 'sent' });
    expect(session.pendingOutgoingTotals.value.get(NATIVE_CONTRACT)).toBe(1_050_000n);
  });

  it('shows and deducts a send only on the network it was made for', async () => {
    const { S, session } = await boot();
    const walletId = S.sessionWalletId.value!;
    const net = S.params.value!.network;
    session.trackPending(pending(walletId, { network: 'testnet-other' }));
    session.trackPending(pending(walletId, { txid: txid('8'), network: net }));
    expect(S.walletPending.value.map((p) => p.txid)).toEqual([txid('8')]);
    expect(session.pendingReservedTotals.value.get(NATIVE_CONTRACT)).toBe(1_050_000n);
    // Kept for when that network's server is used again.
    expect(session.pendingTxs.value).toHaveLength(2);
  });

  it('looks up sends before reading history and balances', async () => {
    const { S, backend, session } = await boot();
    const walletId = S.sessionWalletId.value!;
    session.trackPending(pending(walletId));
    const order: string[] = [];
    intercept(backend, (path) => {
      if (path.startsWith('/transaction?')) order.push('tx');
      else if (path.startsWith('/address')) order.push('chain');
      return undefined;
    });
    S.refreshNow();
    await settle(2000);
    expect(order[0]).toBe('tx');
    expect(order.lastIndexOf('tx')).toBeLessThan(order.indexOf('chain'));
  });

  it('counts only the fee of a move to an own address as outgoing, but holds back the amount', async () => {
    const { S, session } = await boot();
    const walletId = S.sessionWalletId.value!;
    session.trackPending(pending(walletId, { to: S.addresses.value[3]!, amount: 900_000_000n, fee: 70_000n }));
    expect(S.activity.value[0]).toMatchObject({ local: true, kind: 'moved' });
    expect(session.pendingOutgoingTotals.value.get(NATIVE_CONTRACT)).toBe(70_000n);
    expect(session.pendingReservedTotals.value.get(NATIVE_CONTRACT)).toBe(900_070_000n);
  });

  it('marks a local send of an unverified token as unverified', async () => {
    const { S, session } = await boot();
    const fake = S.otherAssets.value.find((a) => a.symbol === 'MMX')!;
    expect(fake.native).toBe(false);
    session.trackPending(pending(S.sessionWalletId.value!, { contract: fake.contract, decimals: fake.decimals, status: 'expired', settledAt: Date.now() }));
    const local = S.activity.value.find((i) => i.local)!;
    expect(local.primary).toMatchObject({ contract: fake.contract, native: false, verified: false });
    expect(session.repeatRecipientFor(local)).toBe(FOREIGN);
  });
});

describe('repeating a send', () => {
  it('repeats a plain transfer but not a contract call', async () => {
    const { S, session, backend } = await boot();
    const sent = S.activity.value.find((i) => i.kind === 'sent' && i.primary?.native)!;
    expect(session.repeatRecipientFor(sent)).toBeUndefined();
    await settle(50);
    const to = session.repeatRecipientFor(sent);
    expect(to).toMatch(/^mmx1/);
    expect(S.ownAddressSet.value.has(to!)).toBe(false);
    expect(session.repeatRecipientFor(S.activity.value.find((i) => i.kind === 'reward')!)).toBeNull();

    // The same transaction as a contract call (e.g. an NFT purchase) is not repeatable.
    intercept(backend, (path, real) => {
      if (!path.startsWith(`/transaction?id=${sent.txid}`)) return undefined;
      return real(path).then((tx) => ({ ...(tx as object), operations: [{ __type: 'mmx.operation.Execute', method: 'accept' }] }));
    });
    await S.loadTx(sent.txid, true);
    expect(session.repeatRecipientFor(sent)).toBeNull();
  });
});

describe('tokens', () => {
  it('does not ask the server for whitelist flags and verifies only native MMX', async () => {
    const paths: string[] = [];
    const { S } = await boot((backend) => {
      const real = backend.rpcGet.bind(backend);
      vi.spyOn(backend, 'rpcGet').mockImplementation(async (path: string) => {
        paths.push(path);
        return real(path);
      });
    });
    // Trading holds USDM, which the mock server flags as validated.
    await S.selectWallet(S.wallets.value.find((w) => w.name === 'Trading')!.id);
    await settle(30_000);
    expect(paths.some((p) => p.startsWith('/balance'))).toBe(false);
    expect(paths.some((p) => p.startsWith('/address?id='))).toBe(true);
    expect(S.verifiedAssets.value.map((a) => a.symbol)).toEqual(['MMX']);
    expect(S.otherAssets.value.map((a) => a.symbol).sort()).toEqual(['MMX', 'USDM']);
  });

  it('a token with an empty symbol does not block balances or activity', async () => {
    let first = '';
    const odd = encodeAddress(new Uint8Array(32).fill(7));
    const { S } = await boot((backend) => {
      const real = backend.rpcGet.bind(backend);
      vi.spyOn(backend, 'rpcGet').mockImplementation(async (path: string) => {
        const answer = await real(path);
        if (!first && path.startsWith('/address?id=')) first = /id=([^&]+)/.exec(path)![1]!;
        if (first && path.startsWith(`/address?id=${first}&`)) {
          const o = answer as { balances: unknown[] };
          return { ...o, balances: [...o.balances, { amount: '5', contract: odd, decimals: 0, symbol: '\u{200b}' }] };
        }
        return answer;
      });
    });
    expect(S.chainError.value).toBeNull();
    expect(S.balancesLoaded.value).toBe(true);
    expect(S.otherAssets.value.find((a) => a.contract === odd)).toMatchObject({ total: 5n, verified: false, symbol: `${odd.slice(0, 8)}\u{2026}${odd.slice(-4)}` });
  });
});
