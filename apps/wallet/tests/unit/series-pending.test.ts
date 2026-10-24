import { describe, expect, it } from 'vitest';
import { nextStatus, pendingOutgoing, pendingReserved, prunePending, SETTLED_RETENTION_MS, type PendingTx } from '../../src/chain/pending';
import { buildSeries, estimateTime, rangeStart, RANGE_MS, rowDelta } from '../../src/chain/series';
import { discoveredCount, findLastUsedIndex } from '../../src/chain/discovery';
import { computeAssets, fetchBalances, sortAssets, type AddressBalance } from '../../src/chain/balances';
import { RpcClient } from '../../src/chain/rpc';
import { isVerifiedToken, tokenVerifier } from '../../src/chain/tokens';
import type { ChainParams, HistoryRow, TxInfo, TxType } from '../../src/chain/types';
import { NATIVE_CONTRACT } from '../../src/lib/address';
import { FakeNode, testAddress, testTxid } from './helpers/fake-node';

const DAY = 86_400_000;
const NOW = 1_800_000_000_000;
const A = testAddress(1);
const TOKEN = testAddress(90);

function row(type: TxType, amount: bigint, daysAgo: number, height: number, contract = NATIVE_CONTRACT): HistoryRow {
  return {
    txid: testTxid(`${type}${height}${amount}`),
    height,
    timeMs: NOW - daysAgo * DAY,
    type,
    address: A,
    contract,
    amount,
    symbol: 'MMX',
    decimals: 6,
    native: contract === NATIVE_CONTRACT,
  };
}

describe('balance series', () => {
  it('walks back from the current balance in steps', () => {
    const rows = [row('RECEIVE', 100n, 20, 10), row('SPEND', 30n, 10, 20), row('TXFEE', 1n, 10, 20), row('REWARD', 5n, 2, 30)];
    const s = buildSeries({ rows, contract: NATIVE_CONTRACT, current: 74n, now: NOW, range: '1M', historyComplete: true });
    expect(s.complete).toBe(true);
    expect(s.start).toBe(NOW - RANGE_MS['1M']);
    expect(s.points.map((p) => [Math.round((NOW - p.t) / DAY), p.v])).toEqual([
      [30, 0n],
      [20, 100n],
      [10, 69n],
      [2, 74n],
      [0, 74n],
    ]);
    expect(s.min).toBe(0n);
    expect(s.max).toBe(100n);
  });

  it('starts at the range start with the balance at that time', () => {
    const rows = [row('RECEIVE', 100n, 20, 10), row('RECEIVE', 50n, 3, 20)];
    const s = buildSeries({ rows, contract: NATIVE_CONTRACT, current: 150n, now: NOW, range: '1W', historyComplete: false, frontierTimeMs: NOW - 25 * DAY });
    expect(s.complete).toBe(true);
    expect(s.points.map((p) => p.v)).toEqual([100n, 150n, 150n]);
    expect(s.points[0]!.t).toBe(NOW - 7 * DAY);
  });

  it('is incomplete when older history is missing', () => {
    const rows = [row('RECEIVE', 50n, 3, 20)];
    const s = buildSeries({ rows, contract: NATIVE_CONTRACT, current: 150n, now: NOW, range: '1Y', historyComplete: false, frontierTimeMs: NOW - 5 * DAY });
    expect(s.complete).toBe(false);
    expect(s.start).toBe(NOW - 5 * DAY);
    expect(s.points.map((p) => p.v)).toEqual([100n, 150n, 150n]);
  });

  it('"All" spans the whole history', () => {
    const rows = [row('RECEIVE', 100n, 200, 10), row('SPEND', 40n, 100, 20)];
    const all = buildSeries({ rows, contract: NATIVE_CONTRACT, current: 60n, now: NOW, range: 'ALL', historyComplete: true });
    expect(all.start).toBe(NOW - 200 * DAY);
    expect(all.points.map((p) => p.v)).toEqual([0n, 100n, 60n, 60n]);
    const empty = buildSeries({ rows: [], contract: NATIVE_CONTRACT, current: 0n, now: NOW, range: 'ALL', historyComplete: true });
    expect(empty.points).toEqual([
      { t: NOW - 7 * DAY, v: 0n },
      { t: NOW, v: 0n },
    ]);
  });

  it('combines rows of one block and filters by currency', () => {
    const rows = [row('SPEND', 10n, 5, 20), row('RECEIVE', 4n, 5, 20), row('RECEIVE', 999n, 4, 21, TOKEN), row('TXFEE', 1n, 5, 20)];
    const s = buildSeries({ rows, contract: NATIVE_CONTRACT, current: 93n, now: NOW, range: '1M', historyComplete: true });
    expect(s.points.map((p) => p.v)).toEqual([100n, 93n, 93n]);
    const t = buildSeries({ rows, contract: TOKEN, current: 999n, now: NOW, range: '1M', historyComplete: true });
    expect(t.points.map((p) => p.v)).toEqual([0n, 999n, 999n]);
  });

  it('never goes below zero on inconsistent data', () => {
    const rows = [row('RECEIVE', 100n, 5, 20)];
    const s = buildSeries({ rows, contract: NATIVE_CONTRACT, current: 10n, now: NOW, range: '1M', historyComplete: true });
    expect(s.points.every((p) => p.v >= 0n)).toBe(true);
  });

  it('helpers', () => {
    expect(rowDelta(row('SPEND', 5n, 1, 1))).toBe(-5n);
    expect(rowDelta(row('VDF_REWARD', 5n, 1, 1))).toBe(5n);
    expect(rangeStart('ALL', NOW)).toBeNull();
    expect(estimateTime(90, 100, NOW, 10_000)).toBe(NOW - 100_000);
    expect(estimateTime(110, 100, NOW, 10_000)).toBe(NOW);
  });
});

describe('pending', () => {
  const base: PendingTx = {
    txid: testTxid('p'),
    walletId: 'w',
    to: testAddress(9),
    contract: NATIVE_CONTRACT,
    symbol: 'MMX',
    decimals: 6,
    amount: 10n,
    fee: 2n,
    expires: 100,
    createdAt: 0,
    status: 'pending',
  };
  const tx = (over: Partial<TxInfo>): TxInfo => ({ id: base.txid, expires: 100, didFail: false, fee: 2n, inputs: [], outputs: [], contractCall: false, ...over });

  it('moves to confirmed, failed or expired', () => {
    expect(nextStatus(base, null, 50)).toBe(base);
    expect(nextStatus(base, tx({}), 50)).toBe(base);
    expect(nextStatus(base, tx({ height: 60 }), 61, 5)).toMatchObject({ status: 'confirmed', includedHeight: 60, settledAt: 5 });
    expect(nextStatus(base, tx({ height: 60, didFail: true }), 61)).toMatchObject({ status: 'failed' });
    expect(nextStatus(base, null, 100)).toBe(base);
    expect(nextStatus(base, null, 101)).toMatchObject({ status: 'expired' });
    const settled = { ...base, status: 'failed' as const };
    expect(nextStatus(settled, tx({ height: 1 }), 2)).toBe(settled);
  });

  it('prunes entries that history contains and old settled ones', () => {
    const now = 10 * SETTLED_RETENTION_MS;
    const list: PendingTx[] = [
      { ...base, txid: testTxid('1') },
      { ...base, txid: testTxid('2'), status: 'confirmed', settledAt: now },
      { ...base, txid: testTxid('3'), status: 'failed', settledAt: now },
      { ...base, txid: testTxid('4'), status: 'expired', settledAt: now - SETTLED_RETENTION_MS },
      { ...base, txid: testTxid('5'), status: 'expired', settledAt: now },
    ];
    const kept = prunePending(list, new Set([testTxid('1'), testTxid('3'), testTxid('5')]), now).map((p) => p.txid);
    expect(kept).toEqual([testTxid('2'), testTxid('3')]);
  });

  it('totals pending outgoing including fees in MMX', () => {
    const totals = pendingOutgoing(
      [base, { ...base, contract: TOKEN, amount: 7n }, { ...base, status: 'confirmed' }],
      NATIVE_CONTRACT,
      new Set([A]),
    );
    expect(totals.get(NATIVE_CONTRACT)).toBe(14n);
    expect(totals.get(TOKEN)).toBe(7n);
  });

  it('counts only the fee of a move to an own address as outgoing', () => {
    const own = new Set([A, testAddress(9)]);
    const move = { ...base, to: A, amount: 1_000_000_000n, fee: 100n };
    const tokenMove = { ...base, txid: testTxid('t'), to: A, contract: TOKEN, amount: 50n, fee: 3n };
    const outgoing = pendingOutgoing([move, tokenMove], NATIVE_CONTRACT, new Set([A]));
    expect(outgoing.get(NATIVE_CONTRACT)).toBe(103n);
    expect(outgoing.has(TOKEN)).toBe(false);
    expect(pendingOutgoing([base], NATIVE_CONTRACT, own).get(NATIVE_CONTRACT)).toBe(2n);
    // Until it confirms, the moved amount is still held back from spending.
    const reserved = pendingReserved([move, tokenMove, { ...base, status: 'expired' }], NATIVE_CONTRACT);
    expect(reserved.get(NATIVE_CONTRACT)).toBe(1_000_000_103n);
    expect(reserved.get(TOKEN)).toBe(50n);
  });
});

describe('discovery', () => {
  it('finds the last used address with bounded concurrency', async () => {
    const node = new FakeNode();
    const addrs = Array.from({ length: 30 }, (_, i) => testAddress(100 + i));
    node.add(addrs[3]!, { height: 5 });
    node.add(addrs[17]!, { height: 6 });
    let active = 0;
    let peak = 0;
    const rpc = new RpcClient(async (p) => {
      active++;
      peak = Math.max(peak, active);
      await new Promise((r) => setTimeout(r, 1));
      active--;
      return node.transport(p);
    }, { concurrency: 10 });
    const progress: number[] = [];
    const last = await findLastUsedIndex(rpc, addrs, { concurrency: 4, onProgress: (c) => progress.push(c) });
    expect(last).toBe(17);
    expect(peak).toBeLessThanOrEqual(4);
    expect(progress).toHaveLength(30);
    expect(progress[29]).toBe(30);
    expect(await findLastUsedIndex(rpc, [addrs[0]!])).toBe(-1);
    expect(await findLastUsedIndex(rpc, [])).toBe(-1);
  });

  it('computes the address count', () => {
    expect(discoveredCount(-1)).toBe(10);
    expect(discoveredCount(3)).toBe(10);
    expect(discoveredCount(9)).toBe(10);
    expect(discoveredCount(10)).toBe(11);
    expect(discoveredCount(99)).toBe(100);
    expect(discoveredCount(150)).toBe(100);
  });
});

describe('balances', () => {
  const params: ChainParams = { network: 'mainnet', decimals: 6, blockIntervalMs: 10000, minFeeRatio: [1], minTxfee: 1, minTxfeeIo: 1, minTxfeeSign: 1, minTxfeeMemo: 1 };
  const B = testAddress(2);
  const FAKE = testAddress(91);
  const map = new Map<string, AddressBalance>([
    [A, { rows: [{ contract: NATIVE_CONTRACT, symbol: 'MMX', decimals: 6, native: true, amount: 5n }, { contract: TOKEN, symbol: 'USDM', decimals: 6, native: false, amount: 3n }] }],
    [B, { rows: [{ contract: NATIVE_CONTRACT, symbol: 'MMX', decimals: 6, native: true, amount: 7n }, { contract: FAKE, symbol: 'MMX', decimals: 6, native: false, amount: 1000n }, { contract: TOKEN, symbol: 'USDM', decimals: 6, native: false, amount: 1n }] }],
  ]);

  it('verifies only native MMX on mainnet; the server flag is not used', () => {
    expect(isVerifiedToken('mainnet', NATIVE_CONTRACT)).toBe(true);
    expect(isVerifiedToken('mainnet', TOKEN)).toBe(false);
    expect(isVerifiedToken(undefined, TOKEN)).toBe(false);
    expect(isVerifiedToken('constructor', TOKEN)).toBe(false);
    expect(tokenVerifier('testnet-x')(NATIVE_CONTRACT)).toBe(true);
    const assets = computeAssets(map, [A, B], params);
    expect(assets.map((a) => [a.symbol, a.native, a.verified])).toEqual([
      ['MMX', true, true],
      ['MMX', false, false],
      ['USDM', false, false],
    ]);
  });

  it('fetches one request per address and no whitelist flags', async () => {
    const paths: string[] = [];
    const rpc = new RpcClient(async (path) => {
      paths.push(path);
      return { balances: [{ amount: '5', contract: TOKEN, decimals: 6, symbol: 'USDM', is_validated: true }] };
    });
    const balances = await fetchBalances(rpc, [A, B], params);
    expect(paths.sort()).toEqual([`/address?id=${A}&limit=1000`, `/address?id=${B}&limit=1000`].sort());
    expect(computeAssets(balances, [A, B], params).find((a) => a.contract === TOKEN)).toMatchObject({ verified: false, total: 10n });
  });

  it('totals per contract with native first and unverified last', () => {
    const assets = computeAssets(map, [A, B], params, (c) => c === TOKEN);
    expect(assets.map((a) => [a.symbol, a.contract === NATIVE_CONTRACT, a.verified, a.total])).toEqual([
      ['MMX', true, true, 12n],
      ['USDM', false, true, 4n],
      ['MMX', false, false, 1000n],
    ]);
    expect(assets[0]!.byAddress.get(B)).toBe(7n);
  });

  it('always includes MMX and only counts listed addresses', () => {
    expect(computeAssets(new Map(), [A], params)).toMatchObject([{ native: true, total: 0n }]);
    expect(computeAssets(map, [A], params).map((a) => a.total)).toEqual([5n, 3n]);
    expect(sortAssets([{ native: false, verified: false, symbol: 'A', contract: 'x' }, { native: true, verified: true, symbol: 'Z', contract: 'y' }])[0]!.native).toBe(true);
  });
});
