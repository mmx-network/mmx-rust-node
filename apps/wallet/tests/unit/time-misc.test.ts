import { afterEach, describe, expect, it } from 'vitest';
import { addressUrl, isExplorerUrl, isTxid, transactionUrl } from '../../src/lib/explorer';
import { errorMessage, isTransportError, messageForCode } from '../../src/lib/errors';
import { setLocale } from '../../src/lib/locale';
import { Lru } from '../../src/lib/lru';
import { calendarDayDiff, dayKey, formatDayHeading, formatRelative } from '../../src/lib/time';
import { BackendError, ERROR_CODES, isBackendError, toBackendError } from '../../src/backend/types';

afterEach(() => setLocale(null));

const ADDRESS = 'mmx1ecntftefvza4k9kev5q66la87akr62sljsh28jtfxwyvasgjz8gswl046q';
const TXID = '5DDF532146DB761488320E563F9A04FB39DA77FF1E29419DB7FD494D9B3E6C69';

describe('time', () => {
  const now = new Date(2026, 9, 24, 15, 30).getTime();

  it('formats relative times', () => {
    expect(formatRelative(now - 10_000, now)).toBe('Just now');
    expect(formatRelative(now - 5 * 60_000, now)).toBe('5 min ago');
    expect(formatRelative(now - 3 * 3_600_000, now)).toBe('3 h ago');
    expect(formatRelative(new Date(2026, 9, 23, 23, 0).getTime(), now)).toBe('Yesterday');
    expect(formatRelative(new Date(2026, 9, 1).getTime(), now, 'en-US')).toBe('Oct 1');
    expect(formatRelative(new Date(2025, 2, 4).getTime(), now, 'en-US')).toBe('Mar 4, 2025');
  });

  it('groups by calendar day', () => {
    expect(formatDayHeading(now - 60_000, now)).toBe('Today');
    expect(formatDayHeading(new Date(2026, 9, 23, 1).getTime(), now)).toBe('Yesterday');
    expect(formatDayHeading(new Date(2026, 9, 20).getTime(), now, 'en-US')).toBe('Tuesday');
    expect(formatDayHeading(new Date(2026, 8, 20).getTime(), now, 'en-US')).toBe('Sep 20');
    expect(calendarDayDiff(new Date(2026, 9, 23, 23, 59).getTime(), new Date(2026, 9, 24, 0, 1).getTime())).toBe(1);
    expect(dayKey(new Date(2026, 9, 24, 1).getTime())).toBe(dayKey(new Date(2026, 9, 24, 23).getTime()));
  });
});

describe('explorer', () => {
  it('builds and validates explorer links', () => {
    expect(transactionUrl(TXID.toLowerCase())).toBe(`https://explore.mmx.network/#/explore/transaction/${TXID}`);
    expect(addressUrl(ADDRESS.toUpperCase())).toBe(`https://explore.mmx.network/#/explore/address/${ADDRESS}`);
    expect(transactionUrl('nope')).toBeNull();
    expect(addressUrl('nope')).toBeNull();
    expect(isTxid(TXID)).toBe(true);
    expect(isExplorerUrl(transactionUrl(TXID)!)).toBe(true);
    expect(isExplorerUrl(addressUrl(ADDRESS)!)).toBe(true);
    expect(isExplorerUrl('https://explore.mmx.network/#/explore/block/1')).toBe(false);
    expect(isExplorerUrl(`https://explore.mmx.network/#/explore/transaction/${TXID}/x`)).toBe(false);
    expect(isExplorerUrl(`https://evil.example/#/explore/transaction/${TXID}`)).toBe(false);
    expect(isExplorerUrl(`https://explore.mmx.network.evil.example/#/explore/transaction/${TXID}`)).toBe(false);
    expect(isExplorerUrl('javascript:alert(1)')).toBe(false);
  });
});

describe('errors', () => {
  it('has a message for every code and never shows raw messages', () => {
    for (const code of ERROR_CODES) expect(messageForCode(code)).toMatch(/^[A-Z].*\.$/);
    expect(errorMessage(new BackendError('invalid_passphrase', 'secret detail'))).toBe('Wrong passphrase. Try again.');
    expect(errorMessage(new Error('words: abandon abandon'))).toBe('Something went wrong. Try again.');
    expect(errorMessage({ code: 'locked', message: 'x' })).toBe('Unlock the wallet to continue.');
    expect(errorMessage('rpc_timeout')).toMatch(/not responding/);
  });

  it('normalizes thrown values', () => {
    expect(toBackendError({ code: 'io_error', message: 'disk' }).code).toBe('io_error');
    expect(toBackendError({ message: 'invalid_mnemonic' }).code).toBe('invalid_mnemonic');
    expect(toBackendError(new Error('invalid_mnemonic')).code).toBe('invalid_mnemonic');
    expect(toBackendError({ code: 'bogus' }).code).toBe('internal');
    expect(toBackendError(undefined).code).toBe('internal');
    expect(toBackendError(new Error('leak: my secret words')).message).toBe('internal');
    expect(isBackendError(new BackendError('locked'), 'locked')).toBe(true);
    expect(isBackendError(new BackendError('locked'), 'internal')).toBe(false);
    expect(isTransportError(new BackendError('rpc_timeout'))).toBe(true);
    expect(isTransportError(new BackendError('tx_rejected'))).toBe(false);
  });
});

describe('Lru', () => {
  it('evicts the least recently used entry', () => {
    const lru = new Lru<string, number>(2);
    lru.set('a', 1);
    lru.set('b', 2);
    expect(lru.get('a')).toBe(1);
    lru.set('c', 3);
    expect(lru.has('b')).toBe(false);
    expect(lru.has('a')).toBe(true);
    expect(lru.peek('c')).toBe(3);
    lru.set('d', 4);
    expect(lru.has('a')).toBe(false);
    expect(lru.size).toBe(2);
    expect(() => new Lru(0)).toThrow();
  });
});
