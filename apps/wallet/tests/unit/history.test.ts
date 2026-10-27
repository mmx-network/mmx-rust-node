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
import type { ChainParams, HistoryPage, HistoryRow } from '../../src/chain/types';
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
  };
}

/** A response holding `rows` plus `nftHeights` NFT entries (counted, not kept). */
function page(rows: HistoryRow[], nftHeights: number[] = []): HistoryPage {
  const heights = rows.map((r) => r.height).concat(nftHeights);
  return {
    rows,
    count: heights.length,
    minHeight: heights.length ? Math.min(...heights) : Infinity,
    maxHeight: heights.length ? Math.max(...heights) : -1,
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
    const h = fromFirstPage(page([row(10), row(5)]), 25);
    expect(h).toMatchObject({ top: 10, bottom: 0, exhausted: true });
    expect(h.rows.map((r) => r.height)).toEqual([10, 5]);
    expect(olderPageQuery(h)).toBeNull();
  });

  it('an empty address is exhausted and covers everything', () => {
    const h = fromFirstPage(page([]), 25);
    expect(h).toEqual({ rows: [], top: -1, bottom: 0, exhausted: true });
  });

  it('a full page covers down to its lowest height', () => {
    const rows = [row(50), row(40), row(40, 1), row(30)];
    const h = fromFirstPage(page(rows), 4);
    expect(h).toMatchObject({ top: 50, bottom: 30, exhausted: false });
    expect(olderPageQuery(h, 1000)).toEqual({ until: 29, limit: 1000 });
  });

  it('top is the newest height the server returned, NFT rows included', () => {
    expect(fromFirstPage(page([row(70)]), 25).top).toBe(70);
    expect(fromFirstPage(page([row(70)], [75]), 25).top).toBe(75);
  });

  it('pages by what the node returned, counting NFT rows it leaves out of the cache', () => {
    // 25 entries with an NFT at the newest height: full, not the last page.
    const rows = Array.from({ length: 24 }, (_, i) => row(100 - i));
    const h = fromFirstPage(page(rows, [101]), 25);
    expect(h).toMatchObject({ top: 101, bottom: 77, exhausted: false });
    expect(h.rows).toHaveLength(24);
    // A full older page whose lowest entries are NFT rows continues below them.
    const older = applyOlderPage(h, page([row(76), row(75)], [74, 73]), 4);
    expect(older).toMatchObject({ bottom: 73, exhausted: false });
    expect(olderPageQuery(older)).toEqual({ until: 72, limit: 1000 });
    // A full page of NFT rows only still moves down.
    const nftsOnly = applyOlderPage(older, page([], [72, 71]), 2);
    expect(nftsOnly).toMatchObject({ bottom: 71, exhausted: false });
    expect(nftsOnly.rows).toHaveLength(26);
    // A short page ends the history even if it held only NFT rows.
    expect(applyOlderPage(nftsOnly, page([], [3]), 2)).toMatchObject({ bottom: 0, exhausted: true });
  });

  it('a full page without any confirmed height is rejected', () => {
    expect(() => fromFirstPage({ rows: [], count: 2, minHeight: Infinity, maxHeight: -1 }, 2)).toThrow();
  });

  it('older pages extend downward; a short page exhausts', () => {
    let h = fromFirstPage(page([row(50), row(40)]), 2);
    h = applyOlderPage(h, page([row(30), row(20)]), 2);
    expect(h).toMatchObject({ bottom: 20, exhausted: false });
    h = applyOlderPage(h, page([row(10)]), 2);
    expect(h).toMatchObject({ bottom: 0, exhausted: true });
    expect(h.rows.map((r) => r.height)).toEqual([50, 40, 30, 20, 10]);
  });

  it('ignores rows above the requested range in an older page', () => {
    const h = fromFirstPage(page([row(50), row(40)]), 2);
    const next = applyOlderPage(h, page([row(40), row(30), row(25)]), 3);
    expect(next.rows.map((r) => r.height)).toEqual([50, 40, 30, 25]);
  });

  it('a full page ending at height 0 is exhausted', () => {
    const h = fromFirstPage(page([row(5), row(1)]), 2);
    expect(applyOlderPage(h, page([row(0), row(0, 1)]), 2)).toMatchObject({ bottom: 0, exhausted: true });
  });

  it('refresh re-reads a few blocks and replaces them', () => {
    const h: AddressHistory = { rows: [row(98), row(95), row(80)], top: 100, bottom: 50, exhausted: false };
    expect(refreshSince(h)).toBe(95);
    const same = applyRefresh(h, 95, { rows: [row(98), row(95)], maxHeight: 100 });
    expect(same.changed).toBe(false);
    expect(same.history).toMatchObject({ top: 100, bottom: 50 });
    const reorg = applyRefresh(h, 95, { rows: [row(99), row(95)], maxHeight: 99 });
    expect(reorg.changed).toBe(true);
    expect(reorg.history.rows.map((r) => r.height)).toEqual([99, 95, 80]);
    const added = applyRefresh(h, 95, { rows: [row(103), row(98), row(95)], maxHeight: 103 });
    expect(added.changed).toBe(true);
    expect(added.history.rows.map((r) => r.height)).toEqual([103, 98, 95, 80]);
    expect(added.history.top).toBe(103);
  });

  it('a lagging response lowers top so the next refresh reads the gap again', () => {
    const h: AddressHistory = { rows: [row(99), row(97), row(90), row(80)], top: 99, bottom: 50, exhausted: false };
    // The server behind the load balancer is at 93: rows 97 and 99 are missing.
    const lagging = applyRefresh(h, refreshSince(h), { rows: [], maxHeight: -1 });
    expect(lagging.changed).toBe(true);
    expect(lagging.history.top).toBe(90);
    expect(refreshSince(lagging.history)).toBe(85);
    expect(refreshSince(lagging.history)).toBeLessThanOrEqual(97);
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
    const history = new Map<string, AddressHistory>([[A, fromFirstPage(page([row(5)]), 25)]]);
    expect(frontier(history, [A, B])).toBe(Infinity);
    expect(completeRows(history, [A, B])).toEqual([]);
    expect(addressesToExtend(history, [A, B])).toEqual([]);
  });

  it('everything is complete when all addresses are exhausted', () => {
    const history = new Map<string, AddressHistory>([
      [A, fromFirstPage(page([row(5)]), 25)],
      [B, fromFirstPage(page([]), 25)],
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
    let h = await loadFirstPage(rpc, A, PARAMS, 3);
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
      let h = await loadFirstPage(rpc, A, PARAMS, first);
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
    let h = await loadFirstPage(rpc, A, PARAMS, 10);
    // New activity: a block with 25 rows plus rows in other blocks.
    for (let i = 0; i < 25; i++) node.add(A, { height: 230 });
    node.add(A, { height: 231 });
    node.add(A, { height: 199 }); // inside the overlap window
    const since = refreshSince(h);
    const range = await fetchRange(rpc, A, since, PARAMS, 10);
    expect(keys(range.rows)).toEqual(storedKeys(node, A, since));
    expect(range.maxHeight).toBe(231);
    const r = await refreshAddress(rpc, A, h, PARAMS);
    expect(r.changed).toBe(true);
    h = r.history;
    expect(h.top).toBe(231);
    expect(keys(h.rows.filter((x) => x.height >= h.bottom))).toEqual(storedKeys(node, A, h.bottom));
    const again = await refreshAddress(rpc, A, h, PARAMS);
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
    for (const a of addrs) history.set(a, await loadFirstPage(rpc, a, PARAMS, 25));
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

describe('advancing refresh checkpoints', () => {
  const lastSince = (node: FakeNode) => Number(new URL(node.calls.at(-1)!, 'https://test.invalid').searchParams.get('since'));

  it.each([[999, 0], [1000, 1], [1001, 2], [50_000, 49_001]])('advances an empty address checked at head %i to since=%i', async (head, since) => {
    const node = new FakeNode();
    node.height = head;
    const rpc = new RpcClient(node.transport);
    const h = await loadFirstPage(rpc, A, PARAMS);
    const result = await refreshAddress(rpc, A, h, PARAMS, head);
    expect(result.changed).toBe(false);
    expect(result.history.top).toBe(-1);
    expect(refreshSince(result.history)).toBe(since);
  });

  it('moves since forward without discarding old transactions', async () => {
    const node = new FakeNode();
    node.add(A, { height: 10 });
    node.height = 10_000;
    const rpc = new RpcClient(node.transport);
    let h = await loadFirstPage(rpc, A, PARAMS);
    h = (await refreshAddress(rpc, A, h, PARAMS, node.height)).history;
    node.height = 10_100;
    const result = await refreshAddress(rpc, A, h, PARAMS, node.height);
    expect(lastSince(node)).toBe(9_001);
    expect(result.changed).toBe(false);
    expect(result.history).toMatchObject({ top: 10, checkedHead: 10_100, bottom: 0, exhausted: true });
    expect(keys(result.history.rows)).toEqual(storedKeys(node, A));
    expect(refreshSince(result.history)).toBe(9_101);
  });

  it('pages the entire unchecked gap after a long pause before advancing', async () => {
    const node = new FakeNode();
    node.height = 10_000;
    const rpc = new RpcClient(node.transport);
    let h = await loadFirstPage(rpc, A, PARAMS);
    h = (await refreshAddress(rpc, A, h, PARAMS, node.height)).history;
    for (let i = 0; i < 1005; i++) node.add(A, { height: 15_000 + i });
    node.height = 200_000;
    const before = node.calls.length;
    const result = await refreshAddress(rpc, A, h, PARAMS, node.height);
    const queries = node.calls.slice(before).map((path) => new URL(path, 'https://test.invalid'));
    expect(queries).toHaveLength(2);
    expect(queries.every((query) => query.searchParams.get('since') === '9001')).toBe(true);
    expect(keys(result.history.rows)).toEqual(storedKeys(node, A));
    expect(refreshSince(result.history)).toBe(199_001);
  });

  it('does not advance the checkpoint when a later page fails', async () => {
    const node = new FakeNode();
    node.height = 10_000;
    let fail = false;
    const rpc = new RpcClient(async (path) => {
      if (fail && path.includes('&until=')) throw new Error('page unavailable');
      return node.transport(path);
    });
    let h = await loadFirstPage(rpc, A, PARAMS);
    h = (await refreshAddress(rpc, A, h, PARAMS, node.height)).history;
    for (let i = 0; i < 1005; i++) node.add(A, { height: 15_000 + i });
    node.height = 200_000;
    fail = true;
    await expect(refreshAddress(rpc, A, h, PARAMS, node.height)).rejects.toThrow();
    expect(h.checkedHead).toBe(10_000);
    expect(h.rows).toEqual([]);
    expect(refreshSince(h)).toBe(9_001);
    fail = false;
    const retry = await refreshAddress(rpc, A, h, PARAMS, node.height);
    expect(keys(retry.history.rows)).toEqual(storedKeys(node, A));
    expect(retry.history.checkedHead).toBe(200_000);
  });

  it('preserves the checkpoint when paging older history', async () => {
    const node = new FakeNode();
    for (let i = 0; i < 30; i++) node.add(A, { height: 10 + i });
    node.height = 10_000;
    const rpc = new RpcClient(node.transport);
    let h = await loadFirstPage(rpc, A, PARAMS);
    h = (await refreshAddress(rpc, A, h, PARAMS, node.height)).history;
    h = await loadOlderPage(rpc, A, h, PARAMS);
    expect(h.checkedHead).toBe(10_000);
    expect(refreshSince(h)).toBe(9_001);
    expect(keys(h.rows)).toEqual(storedKeys(node, A));
  });

  it('recovers incoming funds omitted by a lagging history server inside the overlap', async () => {
    const node = new FakeNode();
    node.height = 10_000;
    const rpc = new RpcClient(node.transport);
    let h = await loadFirstPage(rpc, A, PARAMS);
    h = (await refreshAddress(rpc, A, h, PARAMS, node.height)).history;
    node.add(A, { height: 10_020 });
    node.height = 10_030;
    node.historyHeight = 9_950;
    h = (await refreshAddress(rpc, A, h, PARAMS, node.height)).history;
    expect(h.rows).toEqual([]);
    expect(refreshSince(h)).toBe(9_031);
    node.historyHeight = null;
    const result = await refreshAddress(rpc, A, h, PARAMS, node.height);
    expect(result.changed).toBe(true);
    expect(keys(result.history.rows)).toEqual(storedKeys(node, A));
  });

  it('lowers the checkpoint range when the node head rolls back', async () => {
    const node = new FakeNode();
    node.height = 50_000;
    const rpc = new RpcClient(node.transport);
    let h = await loadFirstPage(rpc, A, PARAMS);
    h = (await refreshAddress(rpc, A, h, PARAMS, node.height)).history;
    node.height = 49_000;
    h = (await refreshAddress(rpc, A, h, PARAMS, node.height)).history;
    expect(lastSince(node)).toBe(48_001);
    expect(h.checkedHead).toBe(49_000);
  });

  it('rebuilds the loaded range when a rollback moves below its bottom', async () => {
    const node = new FakeNode();
    for (let i = 0; i < 30; i++) node.add(A, { height: 49_970 + i });
    node.height = 50_000;
    const rpc = new RpcClient(node.transport);
    let h = await loadFirstPage(rpc, A, PARAMS);
    h = (await refreshAddress(rpc, A, h, PARAMS, node.height)).history;
    expect(h.bottom).toBeGreaterThan(49_000);
    node.height = 49_000;
    node.historyHeight = 49_000;
    node.add(A, { height: 48_999 });
    const result = await refreshAddress(rpc, A, h, PARAMS, node.height);
    expect(result.changed).toBe(true);
    expect(result.history.rows.map((entry) => entry.height)).toEqual([48_999]);
    expect(result.history).toMatchObject({ top: 48_999, bottom: 0, exhausted: true });
    expect(result.history.checkedHead).toBeUndefined();
  });
});

describe('NFT rows and lagging servers', () => {
  const currencyKeys = (node: FakeNode, address: string, minHeight = 0) =>
    node
      .all(address)
      .filter((e) => !e.nft && e.height >= minHeight)
      .map((e) => `${e.height}|${e.txid}|${e.amount}`)
      .sort();

  it('an NFT row at the top of the first page does not hide older history', async () => {
    const node = new FakeNode();
    for (let i = 1; i <= 99; i++) node.add(A, { height: i });
    node.add(A, { height: 100, nft: true, contract: testAddress(77) });
    const rpc = new RpcClient(node.transport);
    let h = await loadFirstPage(rpc, A, PARAMS, 25);
    expect(h).toMatchObject({ exhausted: false, bottom: 76, top: 100 });
    expect(h.rows).toHaveLength(24);
    for (let guard = 0; !h.exhausted && guard < 10; guard++) h = await loadOlderPage(rpc, A, h, PARAMS, 25);
    expect(h.exhausted).toBe(true);
    expect(keys(h.rows)).toEqual(currencyKeys(node, A));
  });

  it('pages through full pages containing NFT rows, including pages of NFT rows only', async () => {
    const node = new FakeNode();
    for (let i = 1; i <= 60; i++) node.add(A, { height: i, ...(i % 7 === 0 || (i >= 20 && i < 35) ? { nft: true, contract: testAddress(77) } : {}) });
    node.height = 100;
    const rpc = new RpcClient(node.transport);
    let h = await loadFirstPage(rpc, A, PARAMS, 10);
    for (let guard = 0; !h.exhausted && guard < 20; guard++) h = await loadOlderPage(rpc, A, h, PARAMS, 10);
    expect(h.exhausted).toBe(true);
    expect(keys(h.rows)).toEqual(currencyKeys(node, A));
    // A refresh over a range full of NFT rows pages down too.
    const range = await fetchRange(rpc, A, 15, PARAMS, 5);
    expect(keys(range.rows)).toEqual(currencyKeys(node, A, 15));
    expect(range.maxHeight).toBe(60);
  });

  it('an address with only NFT rows refreshes a small window, not its whole history', async () => {
    const node = new FakeNode();
    for (let i = 1; i <= 40; i++) node.add(A, { height: i * 10, nft: true, contract: testAddress(77) });
    const rpc = new RpcClient(node.transport);
    const h = await loadFirstPage(rpc, A, PARAMS, 25);
    expect(h.top).toBe(400);
    expect(refreshSince(h)).toBe(395);
  });

  it('recovers rows a lagging history server left out', async () => {
    const node = new FakeNode();
    for (let i = 0; i < 30; i++) node.add(A, { height: 900 + i * 3 });
    node.height = 1000;
    const rpc = new RpcClient(node.transport);
    let h = await loadFirstPage(rpc, A, PARAMS, 100);
    const all = currencyKeys(node, A);
    expect(keys(h.rows)).toEqual(all);
    // New rows arrive; the history server answering the refresh is 15 blocks behind.
    node.add(A, { height: 1001 });
    node.add(A, { height: 1004 });
    node.historyHeight = 975;
    const lagging = await refreshAddress(rpc, A, h, PARAMS);
    h = lagging.history;
    expect(h.top).toBeLessThan(983);
    // Later refreshes reach an up-to-date server.
    node.historyHeight = null;
    const caughtUp = await refreshAddress(rpc, A, h, PARAMS);
    expect(caughtUp.changed).toBe(true);
    expect(keys(caughtUp.history.rows)).toEqual(currencyKeys(node, A));
    expect(caughtUp.history.top).toBe(1004);
  });
});
