import { describe, expect, it } from 'vitest';
import { NATIVE_CONTRACT } from '@wallet/lib/address';
import { blockHeight, href, parseRoute, searchTarget } from '../../src/lib/router';
import { pageSlice } from '../../src/lib/paging';
import { atoms, amount, timestamp, transferAmount } from '../../src/lib/format';
import { rpcUrl } from '../../src/lib/config';

const HASH = 'abcd'.repeat(16);
const FARMER = `02${HASH}`;

describe('legacy explorer links and strict search', () => {
  it.each(['', '#/', '#/explore', '#/explore/'])('keeps the existing root %s', (value) => {
    expect(parseRoute(value)).toEqual({ kind: 'blocks' });
  });
  it('opens existing wallet address and transaction links', () => {
    expect(parseRoute(`#/explore/address/${NATIVE_CONTRACT}`)).toMatchObject({ kind: 'address', id: NATIVE_CONTRACT });
    expect(parseRoute(`#/explore/transaction/${HASH}`)).toEqual({ kind: 'transaction', id: HASH.toUpperCase() });
    expect(parseRoute(`#/explore/block/hash/${HASH}`)).toEqual({ kind: 'block', hash: HASH.toUpperCase() });
    expect(parseRoute(`#/explore/farmer/${FARMER}`)).toMatchObject({ kind: 'farmer', id: FARMER.toUpperCase() });
  });
  it('distinguishes heights from long numeric hashes and checks address checksums', () => {
    expect(searchTarget(' 0 ')).toEqual({ kind: 'block', height: 0 });
    expect(searchTarget('1'.repeat(64))).toEqual({ kind: 'hash', id: '1'.repeat(64) });
    expect(searchTarget(FARMER)).toEqual({ kind: 'farmer', id: FARMER.toUpperCase() });
    expect(searchTarget(NATIVE_CONTRACT.toUpperCase())).toEqual({ kind: 'address', id: NATIVE_CONTRACT });
    expect(searchTarget(`${NATIVE_CONTRACT.slice(0, -1)}q`)).toBeNull();
  });
  it.each(['12abc', '-1', '1e5', '3.5', '4294967296', '999999999999999999999', 'hello', '12/3', '0x12'])(
    'rejects malformed height %s before any request',
    (value) => {
      expect(blockHeight(value)).toBeNull();
      expect(searchTarget(value)).toBeNull();
    },
  );
  it('preserves bookmarkable anchored block pages', () => {
    const route = { kind: 'blocks' as const, page: 3, at: 65 };
    expect(parseRoute(href(route))).toEqual(route);
    expect(parseRoute(href({ kind: 'address', id: NATIVE_CONTRACT, page: 4 }))).toMatchObject({
      kind: 'address',
      page: 4,
    });
  });
  it.each([
    '#/explore/blocks?page=0',
    '#/explore/blocks?page=2',
    '#/explore/blocks?page=5&at=3',
    '#/explore/blocks?page=2junk&at=40',
    '#/explore/address/%ZZ',
    '#/other',
    '#/explore/transaction/nope',
  ])('rejects invalid routes %s', (hash) => {
    expect(parseRoute(hash)).toEqual({ kind: 'not-found' });
  });
});

describe('paging and chain values', () => {
  it('pages every record once, including a partial final page', () => {
    const rows = Array.from({ length: 43 }, (_, index) => index);
    const pages = [1, 2, 3].map((page) => pageSlice(rows, page));
    expect(pages.flatMap((page) => page.rows)).toEqual(rows);
    expect(pages.map((page) => [page.start, page.end])).toEqual([
      [1, 20],
      [21, 40],
      [41, 43],
    ]);
    expect(pageSlice(rows, 999).page).toBe(3);
    expect(pageSlice([], 99)).toMatchObject({ page: 1, pages: 1, start: 0, end: 0, rows: [] });
  });
  it('does not round exact uint128 amounts through floating point', () => {
    expect(atoms('340282366920938463463374607431768211455')).toBe('340,282,366,920,938,463,463,374,607,431,768.211455');
    expect(atoms('1')).toBe('0.000001');
    expect(atoms('-1000001')).toBe('-1.000001');
    expect(amount({ amount: '123456789123456789', value: 0 })).toBe('123,456,789,123.456789');
    expect(transferAmount({ amount: '1000000000000000001', decimals: 18 })).toBe('1.000000000000000001');
    expect(atoms(Number.MAX_SAFE_INTEGER + 1)).toBeNull();
    expect(atoms('1', -1)).toBeNull();
    expect(atoms('1', 256)).toBeNull();
  });
  it('keeps zero timestamps and falls back to seconds when needed', () => {
    expect(timestamp({ time_stamp: 0, time: 12 })).toBe(0);
    expect(timestamp({ time: 123 })).toBe(123000);
    expect(timestamp({})).toBeNull();
  });
  it('accepts HTTPS proxies and loopback tests, without credentials', () => {
    expect(rpcUrl(undefined)).toBe('https://rpc.mmx.network');
    expect(rpcUrl('https://example.org/api/')).toBe('https://example.org/api');
    expect(rpcUrl('http://127.0.0.1:5130')).toBe('http://127.0.0.1:5130');
    for (const value of [
      'http://example.org',
      'https://user:secret@example.org',
      'https://example.org/?key=secret',
      'https://example.org/#x',
    ])
      expect(() => rpcUrl(value)).toThrow();
  });
});
