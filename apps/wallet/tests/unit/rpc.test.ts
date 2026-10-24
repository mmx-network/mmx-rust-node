import { describe, expect, it, vi } from 'vitest';
import {
  atomic,
  Backoff,
  checkNode,
  parseAddressBalances,
  parseChainParams,
  parseHistory,
  parseHistoryPage,
  parseNodeInfo,
  parseTransaction,
  RpcClient,
} from '../../src/chain/rpc';
import type { ChainParams } from '../../src/chain/types';
import { BackendError } from '../../src/backend/types';
import { NATIVE_CONTRACT } from '../../src/lib/address';
import { CHAIN_INFO, testAddress, testTxid } from './helpers/fake-node';

const PARAMS: ChainParams = parseChainParams(CHAIN_INFO);
const A = testAddress(1);
const TOKEN = testAddress(99);
const TXID = testTxid('x');

const code = async (p: Promise<unknown> | (() => unknown)): Promise<string> => {
  try {
    await (typeof p === 'function' ? p() : p);
  } catch (e) {
    return (e as BackendError).code;
  }
  return 'ok';
};

function historyRow(over: Record<string, unknown> = {}) {
  return {
    address: A,
    amount: '1500000',
    contract: NATIVE_CONTRACT,
    decimals: 6,
    height: 100,
    is_pending: false,
    is_validated: false,
    memo: null,
    symbol: 'MMX',
    time: 1700000000.5,
    time_stamp: 1700000000500,
    txid: TXID.toLowerCase(),
    type: 'RECEIVE',
    ...over,
  };
}

describe('field validation', () => {
  it('accepts exact amounts only', () => {
    expect(atomic('0')).toBe(0n);
    expect(atomic('340282366920938463463374607431768211455')).toBe((1n << 128n) - 1n);
    expect(atomic(12345)).toBe(12345n);
    for (const bad of ['340282366920938463463374607431768211456', '-1', '1.5', '1e3', '', ' 1', 1.5, -1, 2 ** 60, null, true]) {
      expect(() => atomic(bad)).toThrow(BackendError);
    }
  });

  it('parses chain parameters', () => {
    expect(PARAMS).toEqual({
      network: 'mainnet',
      decimals: 6,
      blockIntervalMs: 10000,
      minFeeRatio: [1, 2, 3, 5, 10],
      minTxfee: 20000,
      minTxfeeIo: 10000,
      minTxfeeSign: 10000,
      minTxfeeMemo: 5000,
    });
    expect(() => parseChainParams({ ...CHAIN_INFO, network: '' })).toThrow();
    expect(() => parseChainParams({ ...CHAIN_INFO, decimals: 19 })).toThrow();
    expect(() => parseChainParams({ ...CHAIN_INFO, min_fee_ratio: [0] })).toThrow();
    expect(() => parseChainParams(null)).toThrow();
    expect(parseChainParams({ ...CHAIN_INFO, block_interval_ms: 'x' }).blockIntervalMs).toBe(10000);
  });

  it('parses node info and checks sync and network', async () => {
    const info = parseNodeInfo({ height: 5, is_synced: true, name: 'mainnet', time_stamp: 1000 });
    expect(info).toEqual({ height: 5, synced: true, network: 'mainnet', timeMs: 1000 });
    expect(() => checkNode(info, PARAMS)).not.toThrow();
    expect(await code(() => checkNode({ ...info, synced: false }, PARAMS))).toBe('rpc_not_synced');
    expect(await code(() => checkNode({ ...info, network: 'testnet11' }, PARAMS))).toBe('rpc_network_mismatch');
    expect(() => parseNodeInfo({ height: -1, is_synced: true, name: 'x' })).toThrow();
    expect(() => parseNodeInfo({ height: 1, is_synced: 'yes', name: 'x' })).toThrow();
  });

  it('parses balances, skipping NFTs and fixing the native symbol', () => {
    const rows = parseAddressBalances(
      {
        balances: [
          { amount: '5', contract: NATIVE_CONTRACT, decimals: 6, symbol: 'XXX' },
          { amount: '7', contract: TOKEN, decimals: 18, symbol: 'TO\u{202e}K' },
          { amount: '1', contract: testAddress(5), is_nft: true },
        ],
        contract: null,
      },
      PARAMS,
    );
    expect(rows).toEqual([
      { contract: NATIVE_CONTRACT, amount: 5n, decimals: 6, symbol: 'MMX', native: true },
      { contract: TOKEN, amount: 7n, decimals: 18, symbol: 'TOK', native: false },
    ]);
  });

  it('rejects inconsistent balances', () => {
    const bad = (balances: unknown[]) => () => parseAddressBalances({ balances }, PARAMS);
    expect(bad([{ amount: '5', contract: NATIVE_CONTRACT, decimals: 8, symbol: 'MMX' }])).toThrow();
    expect(bad([{ amount: '5', contract: TOKEN, decimals: 19, symbol: 'T' }])).toThrow();
    expect(bad([{ amount: '5', contract: TOKEN, decimals: 6 }])).toThrow();
    expect(bad([{ amount: '5', contract: TOKEN, decimals: 6, symbol: 5 }])).toThrow();
    expect(bad([{ amount: '5', contract: 'mmx1bad', decimals: 6, symbol: 'T' }])).toThrow();
    expect(bad([{ amount: 5.5, contract: TOKEN, decimals: 6, symbol: 'T' }])).toThrow();
    expect(
      bad([
        { amount: '5', contract: TOKEN, decimals: 6, symbol: 'T' },
        { amount: '6', contract: TOKEN, decimals: 6, symbol: 'T' },
      ]),
    ).toThrow();
    expect(() => parseAddressBalances({}, PARAMS)).toThrow();
  });

  it('shows a token whose symbol is empty or invisible by its contract', () => {
    // Valid on chain: the node only limits the length and a few whitespace characters.
    const placeholder = 'mmx1' + TOKEN.slice(4, 8) + '\u{2026}' + TOKEN.slice(-4);
    for (const symbol of ['', '\u{1}', '\u{b}', '\u{a0}', '\u{200b}\u{200b}', '\u{202e}']) {
      const [row] = parseAddressBalances({ balances: [{ amount: '1', contract: TOKEN, decimals: 6, symbol }] }, PARAMS);
      expect(row).toMatchObject({ contract: TOKEN, amount: 1n, native: false, symbol: placeholder });
      const [entry] = parseHistory([historyRow({ contract: TOKEN, symbol })], PARAMS, A);
      expect(entry).toMatchObject({ contract: TOKEN, symbol: placeholder });
      const tx = parseTransaction(
        { id: TXID, expires: 1, did_fail: false, fee: { amount: '1' }, outputs: [{ address: A, contract: TOKEN, amount: '1', decimals: 6, symbol }] },
        PARAMS,
      )!;
      expect(tx.outputs[0]!.symbol).toBe(placeholder);
    }
    expect(placeholder).toMatch(/^mmx1.{4}\u{2026}.{4}$/u);
  });

  it('parses history rows', () => {
    const rows = parseHistory(
      [
        historyRow({ memo: 'hi\u{202e}there' }),
        historyRow({ is_pending: true, height: 4294967295 }),
        historyRow({ is_nft: true, symbol: undefined, decimals: undefined }),
        historyRow({ type: 'REWARD', memo: '', time_stamp: undefined }),
      ],
      PARAMS,
      A,
    );
    expect(rows).toHaveLength(2);
    expect(rows[0]).toEqual({
      txid: TXID,
      height: 100,
      timeMs: 1700000000500,
      type: 'RECEIVE',
      address: A,
      contract: NATIVE_CONTRACT,
      amount: 1500000n,
      decimals: 6,
      symbol: 'MMX',
      native: true,
      memo: 'hithere',
    });
    expect(rows[1]!.memo).toBeUndefined();
    expect(rows[1]!.timeMs).toBe(1700000000500);
  });

  it('rejects malformed history', () => {
    const bad = (over: Record<string, unknown>) => () => parseHistory([historyRow(over)], PARAMS, A);
    expect(bad({ type: 'BURN' })).toThrow();
    expect(bad({ txid: 'xyz' })).toThrow();
    expect(bad({ address: testAddress(2) })).toThrow();
    expect(bad({ amount: '-1' })).toThrow();
    expect(bad({ time_stamp: undefined, time: undefined })).toThrow();
    expect(bad({ decimals: 9 })).toThrow();
    expect(bad({ memo: 5 })).toThrow();
    expect(() => parseHistory({}, PARAMS)).toThrow();
  });

  it('counts NFT rows in a history page without keeping them', () => {
    const page = parseHistoryPage(
      [
        historyRow({ is_nft: true, height: 120, symbol: undefined, decimals: undefined }),
        historyRow({ height: 110 }),
        historyRow({ is_nft: true, height: 90, symbol: undefined, decimals: undefined }),
        historyRow({ is_pending: true, height: null }),
      ],
      PARAMS,
      A,
    );
    expect(page.rows.map((r) => r.height)).toEqual([110]);
    expect(page).toMatchObject({ count: 4, minHeight: 90, maxHeight: 120 });
    expect(parseHistoryPage([], PARAMS, A)).toEqual({ rows: [], count: 0, minHeight: Infinity, maxHeight: -1 });
    // NFT rows are still checked: they must belong to the address and lie in range.
    expect(() => parseHistoryPage([historyRow({ is_nft: true, address: testAddress(2) })], PARAMS, A)).toThrow();
  });

  it('rejects history outside the requested range', () => {
    const rows = [historyRow({ height: 50 }), historyRow({ height: 40, is_nft: true })];
    expect(parseHistoryPage(rows, PARAMS, A, { since: 40, until: 50 }).count).toBe(2);
    expect(() => parseHistoryPage(rows, PARAMS, A, { until: 49 })).toThrow(BackendError);
    expect(() => parseHistoryPage(rows, PARAMS, A, { since: 41 })).toThrow(BackendError);
  });

  it('parses transactions', () => {
    const tx = parseTransaction(
      {
        id: TXID,
        expires: 200,
        did_fail: false,
        height: 150,
        time_stamp: 5000,
        fee: { amount: '60000', value: 0.06 },
        sender: A,
        inputs: [{ address: A, contract: NATIVE_CONTRACT, amount: '10', decimals: 6, symbol: 'MMX', memo: 'm' }],
        outputs: [{ address: testAddress(2), contract: TOKEN, amount: '10', decimals: 6, symbol: 'T\u{202e}' }],
      },
      PARAMS,
    )!;
    expect(tx).toMatchObject({ id: TXID, height: 150, timeMs: 5000, expires: 200, didFail: false, fee: 60000n, sender: A });
    expect(tx.inputs[0]).toEqual({ address: A, contract: NATIVE_CONTRACT, amount: 10n, memo: 'm', decimals: 6, symbol: 'MMX' });
    expect(tx.outputs[0]!.symbol).toBe('T');
    expect(tx.contractCall).toBe(false);
    expect(parseTransaction(null, PARAMS)).toBeNull();
    const base = { id: TXID, expires: 1, did_fail: false, fee: { amount: '1' } };
    expect(parseTransaction({ ...base, operations: [], deployed: null }, PARAMS)!.contractCall).toBe(false);
    expect(parseTransaction({ ...base, operations: [{ __type: 'mmx.operation.Execute', method: 'trade' }] }, PARAMS)!.contractCall).toBe(true);
    expect(parseTransaction({ ...base, deployed: { __type: 'mmx.contract.Executable' } }, PARAMS)!.contractCall).toBe(true);
    expect(() => parseTransaction({ ...base, operations: 'x' }, PARAMS)).toThrow();
    const pending = parseTransaction({ id: TXID, expires: 1, did_fail: false, height: null, fee: { amount: '1' } }, PARAMS)!;
    expect(pending.height).toBeUndefined();
    expect(() => parseTransaction({ id: TXID, expires: 1, did_fail: false, fee: { amount: 1.5 } }, PARAMS)).toThrow();
  });
});

describe('RpcClient', () => {
  it('limits concurrency to 4 and hands slots over in order', async () => {
    let active = 0;
    let peak = 0;
    const order: string[] = [];
    const rpc = new RpcClient(async (path) => {
      active++;
      peak = Math.max(peak, active);
      await new Promise((r) => setTimeout(r, 5));
      active--;
      order.push(path);
      return path;
    });
    const results = await Promise.all(Array.from({ length: 20 }, (_, i) => rpc.get(`/p${i}`)));
    expect(results).toEqual(Array.from({ length: 20 }, (_, i) => `/p${i}`));
    expect(peak).toBe(4);
  });

  it('de-duplicates identical in-flight requests', async () => {
    const transport = vi.fn(async () => {
      await new Promise((r) => setTimeout(r, 5));
      return { ok: 1 };
    });
    const rpc = new RpcClient(transport);
    const [a, b] = await Promise.all([rpc.get('/x'), rpc.get('/x')]);
    expect(a).toBe(b);
    expect(transport).toHaveBeenCalledTimes(1);
    await rpc.get('/x');
    expect(transport).toHaveBeenCalledTimes(2);
  });

  it('times out and frees the slot', async () => {
    vi.useFakeTimers();
    try {
      const rpc = new RpcClient(() => new Promise(() => undefined), { concurrency: 1, timeoutMs: 1000 });
      const first = code(rpc.get('/hang'));
      const second = code(rpc.get('/hang2'));
      await vi.advanceTimersByTimeAsync(1000);
      expect(await first).toBe('rpc_timeout');
      await vi.advanceTimersByTimeAsync(1000);
      expect(await second).toBe('rpc_timeout');
    } finally {
      vi.useRealTimers();
    }
  });

  it('maps unknown transport failures to rpc_unreachable and keeps backend codes', async () => {
    expect(await code(new RpcClient(async () => Promise.reject(new TypeError('fetch failed'))).get('/x'))).toBe('rpc_unreachable');
    expect(await code(new RpcClient(() => { throw new Error('sync'); }).get('/x'))).toBe('rpc_unreachable');
    expect(await code(new RpcClient(async () => Promise.reject(new BackendError('rpc_http_error'))).get('/x'))).toBe('rpc_http_error');
    expect(await code(new RpcClient(async () => Promise.reject(new BackendError('invalid_argument'))).get('/x'))).toBe('invalid_argument');
  });

  it('caches chain parameters and retries after a failure', async () => {
    let calls = 0;
    const rpc = new RpcClient(async () => {
      calls++;
      if (calls === 1) throw new BackendError('rpc_unreachable');
      return CHAIN_INFO;
    });
    expect(await code(rpc.params())).toBe('rpc_unreachable');
    expect((await rpc.params()).network).toBe('mainnet');
    await rpc.params();
    expect(calls).toBe(2);
  });

  it('checks height, history and transactions', async () => {
    const rpc = new RpcClient(async (path) => {
      if (path === '/node/info') return { height: 9, is_synced: false, name: 'mainnet' };
      if (path.startsWith('/transaction')) return path.includes(TXID) ? null : { id: testTxid('other'), expires: 1, did_fail: false, fee: { amount: '1' } };
      if (path.startsWith('/address/history')) return [historyRow()];
      throw new Error(path);
    });
    expect(await code(rpc.height(PARAMS))).toBe('rpc_not_synced');
    expect(await rpc.transaction(TXID.toLowerCase(), PARAMS)).toBeNull();
    expect(await code(rpc.transaction(testTxid('y'), PARAMS))).toBe('rpc_response_invalid');
    expect(await code(rpc.transaction('nope', PARAMS))).toBe('invalid_argument');
    expect(await code(rpc.history(A, { limit: 5, since: 1, until: 9 }, PARAMS))).toBe('rpc_response_invalid');
    expect((await rpc.history(A, { limit: 5, since: 1, until: 100 }, PARAMS)).rows).toHaveLength(1);
    expect(await code(rpc.history(testAddress(3), { limit: 5 }, PARAMS))).toBe('rpc_response_invalid');
    expect(await rpc.isUsed(A)).toBe(true);
  });

  it('rejects a full history page without a confirmed height', async () => {
    const pending = historyRow({ is_pending: true, height: null });
    const rpc = new RpcClient(async () => [pending, pending]);
    expect(await code(rpc.history(A, { limit: 2 }, PARAMS))).toBe('rpc_response_invalid');
    expect((await rpc.history(A, { limit: 3 }, PARAMS)).count).toBe(2);
  });
});

describe('Backoff', () => {
  it('doubles up to the maximum and resets', () => {
    const b = new Backoff(10, 50);
    expect([b.fail(), b.fail(), b.fail(), b.fail()]).toEqual([10, 20, 40, 50]);
    expect(b.failing).toBe(true);
    b.reset();
    expect(b.failing).toBe(false);
    expect(b.fail()).toBe(10);
  });
});
