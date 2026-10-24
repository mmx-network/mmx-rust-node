import { describe, expect, it } from 'vitest';
import {
  addressesToExtend,
  applyOlderPage,
  applyRefresh,
  canExtend,
  completeRows,
  fetchRange,
  frontier,
  fromFirstPage,
  isComplete,
  loadFirstPage,
  loadOlderPage,
  olderPageQuery,
  refreshAddress,
  refreshSince,
  sameRows,
  totalRows,
  type AddressHistory,
} from '../../src/chain/history';
import { RpcClient } from '../../src/chain/rpc';
import type { ChainParams, HistoryRow } from '../../src/chain/types';
import { NATIVE_CONTRACT } from '../../src/lib/address';
import { FakeNode, testAddress, testTxid } from './helpers/fake-node';

const PARAMS: ChainParams = {
  network: 'mainnet',
  decimals: 6,
  blockIntervalMs: 10_000,
  minFeeRatio: [1, 2, 3, 5, 10],
  minTxfee: 20000,
  minTxfeeIo: 10000,
  minTxfeeSign: 10000,
  minTxfeeMemo: 5000,
};

const A = testAddress(1);
const B = testAddress(2);

function row(height: number, n = 0, address = A): HistoryRow {
  return {
    txid: testTxid(`${address}|${height}|${n}`),
    height,
    timeMs: 1_700_000_000_000 + height * 10_000,
    type: 'RECEIVE',
    address,
    contract: NATIVE_CONTRACT,
    amount: BigInt(n + 1),
    symbol: 'MMX',
    decimals: 6,
    native: true,
    validated: false,
  };
}

function keys(rows: readonly { txid: string; height: number; amount: bigint }[]): string[] {
  return rows.map((r) => `${r.height}|${r.txid}|${r.amount}`).sort();
}

function storedKeys(node: FakeNode, address: string, minHeight = 0): string[] {
  return node
    .all(address)
    .filter((e) => e.height >= minHeight)
    .map((e) => `${e.height}|${e.txid}|${e.amount}`)
    .sort();
}

/** Deterministic pseudo-random generator for property tests. */
function prng(seed: number): () => number {
  let s = seed >>> 0;
  return () => {
    s = (Math.imul(s ^ (s >>> 15), 0x2c1b3c6d) + 0x9e3779b9) >>> 0;
    s ^= s >>> 13;
    return (s >>> 0) / 4294967296;
  };
}

describe('pure page logic', () => {
  it('first page shorter than the limit means the address is exhausted', () => {
    const h = fromFirstPage([row(10), row(5)], 25, 100);
    expect(h).toMatchObject({ top: 100, bottom: 0, exhausted: true });
    expect(h.rows.map((r) => r.height)).toEqual([10, 5]);
    expect(olderPageQuery(h)).toBeNull();
  });

  it('an empty address is exhausted and covers everything', () => {
    const h = fromFirstPage([], 25, 100);
    expect(h).toEqual({ rows: [], top: 100, bottom: 0, exhausted: true });
  });

  it('a full page covers down to its lowest height', () => {
    const rows = [row(50), row(40), row(40, 1), row(30)];
    const h = fromFirstPage(rows, 4, 60);
    expect(h).toMatchObject({ top: 60, bottom: 30, exhausted: false });
    expect(olderPageQuery(h, 1000)).toEqual({ until: 29, limit: 1000 });
  });

  it('top never goes below the newest row', () => {
    expect(fromFirstPage([row(70)], 25, 60).top).toBe(70);
  });

  it('older pages extend downward; a short page exhausts', () => {
    let h = fromFirstPage([row(50), row(40)], 2, 60);
    h = applyOlderPage(h, [row(30), row(20)], 2);
    expect(h).toMatchObject({ bottom: 20, exhausted: false });
    h = applyOlderPage(h, [row(10)], 2);
    expect(h).toMatchObject({ bottom: 0, exhausted: true });
    expect(h.rows.map((r) => r.height)).toEqual([50, 40, 30, 20, 10]);
  });

  it('ignores rows above the requested range in an older page', () => {
    const h = fromFirstPage([row(50), row(40)], 2, 60);
    const next = applyOlderPage(h, [row(40), row(30), row(25)], 3);
    expect(next.rows.map((r) => r.height)).toEqual([50, 40, 30, 25]);
  });

  it('a full page ending at height 0 is exhausted', () => {
    const h = fromFirstPage([row(5), row(1)], 2, 10);
    expect(applyOlderPage(h, [row(0), row(0, 1)], 2)).toMatchObject({ bottom: 0, exhausted: true });
  });

  it('refresh re-reads a few blocks and replaces them', () => {
    const h: AddressHistory = { rows: [row(98), row(95), row(80)], top: 100, bottom: 50, exhausted: false };
    expect(refreshSince(h)).toBe(95);
    const same = applyRefresh(h, 95, [row(98), row(95)], 104);
    expect(same.changed).toBe(false);
    expect(same.history).toMatchObject({ top: 104, bottom: 50 });
    const reorg = applyRefresh(h, 95, [row(99), row(95)], 104);
    expect(reorg.changed).toBe(true);
    expect(reorg.history.rows.map((r) => r.height)).toEqual([99, 95, 80]);
    const added = applyRefresh(h, 95, [row(103), row(98), row(95)], 104);
    expect(added.changed).toBe(true);
    expect(added.history.rows.map((r) => r.height)).toEqual([103, 98, 95, 80]);
  });

  it('refresh never starts below the covered range', () => {
    expect(refreshSince({ rows: [], top: 100, bottom: 98, exhausted: false })).toBe(98);
    expect(refreshSince({ rows: [], top: 3, bottom: 0, exhausted: true })).toBe(0);
  });

  it('compares row multisets', () => {
    expect(sameRows([row(1), row(1)], [row(1), row(1)])).toBe(true);
    expect(sameRows([row(1), row(1)], [row(1)])).toBe(false);
    expect(sameRows([row(1), row(2)], [row(2), row(1)])).toBe(true);
    expect(sameRows([row(1, 0)], [row(1, 1)])).toBe(false);
  });
});

describe('wallet frontier', () => {
  it('only rows at or above the highest open bottom are complete', () => {
    const history = new Map<string, AddressHistory>([
      [A, { rows: [row(90), row(70), row(60)], top: 100, bottom: 60, exhausted: false }],
      [B, { rows: [row(95, 0, B), row(50, 0, B)], top: 100, bottom: 0, exhausted: true }],
    ]);
    expect(frontier(history, [A, B])).toBe(60);
    expect(completeRows(history, [A, B]).map((r) => r.height)).toEqual([95, 90, 70, 60]);
    expect(addressesToExtend(history, [A, B])).toEqual([A]);
    expect(isComplete(history, [A, B])).toBe(false);
    expect(totalRows(history, [A, B])).toBe(5);
    expect(canExtend(history, [A, B])).toBe(true);
    expect(canExtend(history, [A, B], 5)).toBe(false);
  });

  it('nothing is complete while an address is not loaded', () => {
    const history = new Map<string, AddressHistory>([[A, fromFirstPage([row(5)], 25, 10)]]);
    expect(frontier(history, [A, B])).toBe(Infinity);
    expect(completeRows(history, [A, B])).toEqual([]);
    expect(addressesToExtend(history, [A, B])).toEqual([]);
  });

  it('everything is complete when all addresses are exhausted', () => {
    const history = new Map<string, AddressHistory>([
      [A, fromFirstPage([row(5)], 25, 10)],
      [B, fromFirstPage([], 25, 10)],
    ]);
    expect(frontier(history, [A, B])).toBe(0);
    expect(isComplete(history, [A, B])).toBe(true);
    expect(canExtend(history, [A, B])).toBe(false);
  });

  it('extends all addresses tied at the frontier', () => {
    const history = new Map<string, AddressHistory>([
      [A, { rows: [row(60)], top: 100, bottom: 60, exhausted: false }],
      [B, { rows: [row(60, 0, B)], top: 100, bottom: 60, exhausted: false }],
    ]);
    expect(addressesToExtend(history, [A, B])).toEqual([A, B]);
  });
});

describe('paging against a node', () => {
  it('pages ending mid-block include the whole block exactly once', async () => {
    const node = new FakeNode();
    for (let i = 0; i < 7; i++) node.add(A, { height: 500 });
    node.add(A, { height: 600 });
    node.add(A, { height: 400 });
    node.add(A, { height: 300 });
    node.height = 700;
    const rpc = new RpcClient(node.transport);
    let h = await loadFirstPage(rpc, A, PARAMS, 700, 3);
    expect(h.rows).toHaveLength(8); // 600 + all seven rows of block 500
    expect(h.bottom).toBe(500);
    h = await loadOlderPage(rpc, A, h, PARAMS, 3);
    expect(h.exhausted).toBe(true);
    expect(keys(h.rows)).toEqual(storedKeys(node, A));
  });

  it('reconstructs every address exactly for random data and limits', async () => {
    const rand = prng(42);
    for (let trial = 0; trial < 40; trial++) {
      const node = new FakeNode();
      const n = Math.floor(rand() * 400);
      for (let i = 0; i < n; i++) node.add(A, { height: 1 + Math.floor(rand() * (rand() < 0.3 ? 20 : 2000)) });
      node.height = 3000;
      const rpc = new RpcClient(node.transport);
      const first = 1 + Math.floor(rand() * 30);
      const page = 1 + Math.floor(rand() * 150);
      let h = await loadFirstPage(rpc, A, PARAMS, node.height, first);
      for (let guard = 0; !h.exhausted && guard < 1000; guard++) h = await loadOlderPage(rpc, A, h, PARAMS, page);
      expect(h.exhausted).toBe(true);
      expect(keys(h.rows)).toEqual(storedKeys(node, A));
    }
  });

  it('refresh picks up new rows, including busy blocks spanning several pages', async () => {
    const node = new FakeNode();
    for (let i = 0; i < 30; i++) node.add(A, { height: 100 + i });
    node.height = 200;
    const rpc = new RpcClient(node.transport);
    let h = await loadFirstPage(rpc, A, PARAMS, 200, 10);
    // New activity: a block with 25 rows plus rows in other blocks.
    for (let i = 0; i < 25; i++) node.add(A, { height: 230 });
    node.add(A, { height: 231 });
    node.add(A, { height: 199 }); // inside the overlap window
    const since = refreshSince(h);
    const rows = await fetchRange(rpc, A, since, PARAMS, 10);
    expect(keys(rows)).toEqual(storedKeys(node, A, since));
    const r = await refreshAddress(rpc, A, h, PARAMS, 231);
    expect(r.changed).toBe(true);
    h = r.history;
    expect(h.top).toBe(231);
    expect(keys(h.rows.filter((x) => x.height >= h.bottom))).toEqual(storedKeys(node, A, h.bottom));
    const again = await refreshAddress(rpc, A, h, PARAMS, 231);
    expect(again.changed).toBe(false);
  });

  it('stops paging a range that is too large', async () => {
    const node = new FakeNode();
    for (let i = 0; i < 50; i++) node.add(A, { height: 10 + i });
    const rpc = new RpcClient(node.transport);
    await expect(fetchRange(rpc, A, 0, PARAMS, 5, 3)).rejects.toBeInstanceOf(RangeError);
  });

  it('merges several addresses into a complete wallet view', async () => {
    const node = new FakeNode();
    const rand = prng(7);
    const addrs = [A, B, testAddress(3)];
    for (const a of addrs) {
      const n = Math.floor(rand() * 200);
      for (let i = 0; i < n; i++) node.add(a, { height: 1 + Math.floor(rand() * 1000) });
    }
    node.height = 1200;
    const rpc = new RpcClient(node.transport);
    const history = new Map<string, AddressHistory>();
    for (const a of addrs) history.set(a, await loadFirstPage(rpc, a, PARAMS, 1200, 25));
    for (let round = 0; round < 100 && !isComplete(history, addrs); round++) {
      const f = frontier(history, addrs);
      const all = addrs.flatMap((a) => node.all(a).filter((e) => e.height >= f));
      expect(keys(completeRows(history, addrs))).toEqual(all.map((e) => `${e.height}|${e.txid}|${e.amount}`).sort());
      for (const a of addressesToExtend(history, addrs)) history.set(a, await loadOlderPage(rpc, a, history.get(a)!, PARAMS, 40));
    }
    expect(isComplete(history, addrs)).toBe(true);
    expect(completeRows(history, addrs)).toHaveLength(addrs.reduce((s, a) => s + node.all(a).length, 0));
  });
});
