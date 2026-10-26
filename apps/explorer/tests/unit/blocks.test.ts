import { describe, expect, it } from 'vitest';
import { ApiClient } from '../../src/lib/api';
import { blockPage, transactionsPerSecond } from '../../src/lib/blocks';
import type { Block } from '../../src/lib/types';

const block = (height: number): Block => ({ height, hash: height.toString(16).padStart(64, '0') });

describe('TPS from the displayed headers', () => {
  const headers = (): Block[] =>
    Array.from({ length: 20 }, (_, index) => ({
      ...block(20 - index),
      tx_count: 23,
      time_stamp: 1_000_000 - index * 10_000,
    }));

  it('reports steady throughput without treating 20 timestamps as 20 intervals', () => {
    expect(transactionsPerSecond(headers())).toBeCloseTo(2.3);
  });

  it('includes all transaction counts and uses measured, irregular intervals', () => {
    expect(
      transactionsPerSecond([
        { ...block(3), tx_count: 20, time_stamp: 5000 },
        { ...block(2), tx_count: 0, time: 3.5 },
        { ...block(1), tx_count: 40, time_stamp: 1000 },
      ]),
    ).toBeCloseTo(10);
  });

  it('distinguishes no traffic from an unavailable measurement', () => {
    expect(transactionsPerSecond(headers().map((row) => ({ ...row, tx_count: 0 })))).toBe(0);
    expect(transactionsPerSecond([])).toBeNull();
    expect(transactionsPerSecond(headers().slice(0, 1))).toBeNull();
    for (const invalid of [
      { time_stamp: undefined },
      { time_stamp: 1_000_000 },
      { tx_count: undefined },
      { tx_count: -1 },
      { tx_count: NaN },
      { height: 0 },
    ]) {
      const rows = headers();
      rows[1] = { ...rows[1]!, ...invalid };
      expect(transactionsPerSecond(rows)).toBeNull();
    }
  });
});

function chain(tip: number) {
  const requests: string[] = [];
  const client = new ApiClient(undefined, async (input) => {
    const url = new URL(String(input));
    requests.push(url.pathname + url.search);
    if (url.pathname === '/header') return new Response(JSON.stringify(block(0)));
    const offset = Number(url.searchParams.get('offset') ?? tip);
    // Deliberately match WebAPI::render_headers: offset never includes zero.
    const rows = Array.from({ length: Math.min(20, offset) }, (_, index) => block(offset - index));
    return new Response(JSON.stringify(rows));
  });
  return { client, requests };
}

describe('bounded block history with the C++ headers contract', () => {
  it('visits every height through genesis without duplicates or a full-chain scan', async () => {
    const { client, requests } = chain(65);
    const heights: number[] = [];
    for (const offset of [undefined, 45, 25, 5])
      heights.push(...(await blockPage(client, offset, new AbortController().signal)).map((row) => row.height));
    expect(heights).toEqual(Array.from({ length: 66 }, (_, index) => 65 - index));
    expect(requests).toEqual([
      '/headers?limit=20',
      '/headers?limit=20&offset=45',
      '/headers?limit=20&offset=25',
      '/headers?limit=20&offset=5',
      '/header?height=0',
    ]);
  });
  it('fetches genesis alone when exactly one page remains', async () => {
    const { client, requests } = chain(20);
    expect((await blockPage(client, undefined, new AbortController().signal)).map((row) => row.height)).toHaveLength(
      20,
    );
    expect(await blockPage(client, 0, new AbortController().signal)).toEqual([block(0)]);
    expect(requests).toEqual(['/headers?limit=20', '/header?height=0']);
  });
  it('handles a genesis-only chain', async () => {
    const { client } = chain(0);
    expect(await blockPage(client, undefined, new AbortController().signal)).toEqual([block(0)]);
  });
});
