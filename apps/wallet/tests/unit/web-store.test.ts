import { describe, expect, it } from 'vitest';
import { isBackendError } from '../../src/backend/types';
import { memoryStorage } from '../../src/backend/web/memory-storage';
import { DAMAGED_KEY, isFingerprint, MetadataStore, parseWalletRecord, STORAGE_KEY } from '../../src/backend/web/store';

const CONTACT = 'mmx1yg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3q9fkn2w';

function seeded(value: unknown) {
  const storage = memoryStorage({ [STORAGE_KEY]: typeof value === 'string' ? value : JSON.stringify(value) });
  return { storage, store: new MetadataStore(storage) };
}

function stored(storage: ReturnType<typeof memoryStorage>): Record<string, unknown> {
  return JSON.parse(storage.dump()[STORAGE_KEY]!) as Record<string, unknown>;
}

describe('wallet entries', () => {
  it('accepts canonical u32 fingerprints only', () => {
    for (const ok of ['0', '7', '3291348052', '4294967295']) expect(isFingerprint(ok), ok).toBe(true);
    for (const bad of ['', '01', '4294967296', '-1', '1.0', ' 1', 'abc', 7]) expect(isFingerprint(bad), String(bad)).toBe(false);
  });

  it('falls back to safe values for damaged fields', () => {
    expect(parseWalletRecord({ name: 'Main', withPassphrase: false, addressCount: 12, verified: true, added: 5 })).toEqual({
      name: 'Main',
      withPassphrase: false,
      addressCount: 12,
      verified: true,
      added: 5,
    });
    expect(parseWalletRecord({ name: 5, withPassphrase: 'no', addressCount: 101, verified: 'yes', added: -1 })).toEqual({
      name: 'Wallet',
      withPassphrase: true,
      addressCount: 10,
      verified: false,
      added: 0,
    });
    expect(parseWalletRecord({ name: 'a\u{202e}b\n'.repeat(30) })?.name.length).toBeLessThanOrEqual(64);
    expect(parseWalletRecord(null)).toBeNull();
    expect(parseWalletRecord([])).toBeNull();
    expect(parseWalletRecord('x')).toBeNull();
  });
});

describe('MetadataStore', () => {
  it('starts empty and lists wallets in the order they were added', () => {
    const storage = memoryStorage();
    const store = new MetadataStore(storage);
    expect(store.list()).toEqual([]);
    store.putWallet('900', { name: 'B', withPassphrase: false, addressCount: 10, verified: true, added: 2 });
    store.putWallet('5', { name: 'A', withPassphrase: true, addressCount: 3, verified: false, added: 1 });
    store.putWallet('3000000000', { name: 'C', withPassphrase: false, addressCount: 10, verified: true, added: 3 });
    expect(store.list().map((w) => w.record.name)).toEqual(['A', 'B', 'C']);
    expect(store.get('5')?.addressCount).toBe(3);
    expect(store.has('6')).toBe(false);
  });

  it('skips damaged entries but keeps them, and other wallets, when writing', () => {
    const { storage, store } = seeded({
      wallets: { '1': { name: 'Good', withPassphrase: false, addressCount: 10, verified: true, added: 1 }, '2': 'garbage', bad: { name: 'x' } },
      contacts: [],
      future: { keep: true },
    });
    expect(store.list().map((w) => w.id)).toEqual(['1']);
    store.patchWallet('1', { name: 'Renamed' });
    const root = stored(storage);
    expect(root.wallets).toEqual({
      '1': { name: 'Renamed', withPassphrase: false, addressCount: 10, verified: true, added: 1 },
      '2': 'garbage',
      bad: { name: 'x' },
    });
    expect(root.future).toEqual({ keep: true });
  });

  it('keeps unknown fields of an entry', () => {
    const { storage, store } = seeded({ wallets: { '1': { name: 'A', withPassphrase: false, addressCount: 10, verified: false, added: 1, extra: 'x' } } });
    store.patchWallet('1', { verified: true });
    expect((stored(storage).wallets as Record<string, Record<string, unknown>>)['1']).toMatchObject({ verified: true, extra: 'x' });
  });

  it('survives unreadable data and moves it aside before overwriting', () => {
    for (const raw of ['{not json', '[1,2]', '"text"', 'null']) {
      const { storage, store } = seeded(raw);
      expect(store.list()).toEqual([]);
      expect(store.appState().contacts).toEqual([]);
      store.putWallet('1', { name: 'A', withPassphrase: false, addressCount: 10, verified: false, added: 1 });
      expect(storage.dump()[DAMAGED_KEY]).toBe(raw);
      expect(store.list().map((w) => w.id)).toEqual(['1']);
    }
  });

  it('reports wallet_not_found for unknown or damaged entries', () => {
    const { store } = seeded({ wallets: { '2': 'garbage' } });
    for (const id of ['1', '2', 'x']) {
      try {
        store.patchWallet(id, { name: 'X' });
        expect.unreachable();
      } catch (e) {
        expect(isBackendError(e, 'wallet_not_found')).toBe(true);
      }
    }
  });

  it('reports io_error when storage refuses a write', () => {
    const storage = memoryStorage();
    storage.setItem = () => {
      throw new DOMException('full', 'QuotaExceededError');
    };
    const store = new MetadataStore(storage);
    expect(() => store.putWallet('1', { name: 'A', withPassphrase: false, addressCount: 10, verified: false, added: 1 })).toThrow(
      expect.objectContaining({ code: 'io_error' }),
    );
  });

  it('treats unreadable storage as empty', () => {
    const store = new MetadataStore({
      getItem: () => {
        throw new DOMException('denied', 'SecurityError');
      },
      setItem: () => undefined,
      removeItem: () => undefined,
    });
    expect(store.list()).toEqual([]);
  });

  it('validates app state on read and write and never stores a server URL', () => {
    const { storage, store } = seeded({
      wallets: { '1': { name: 'A', withPassphrase: false, addressCount: 10, verified: true, added: 1 } },
      activeWallet: '99',
      labels: { '1': { '0': 'Main', '500': 'x', '2': 'y\u{202e}' } },
      contacts: [{ id: 'c-1', name: 'Alice', address: CONTACT }, { name: 'Bad', address: 'mmx1nope' }],
      settings: { theme: 'dark', autoLockMinutes: 0, feeLevel: 3, rpcUrl: 'https://evil.example' },
    });
    const state = store.appState();
    expect(state.activeWallet).toBeUndefined();
    expect(state.labels).toEqual({ '1': { 0: 'Main', 2: 'y' } });
    expect(state.contacts).toEqual([{ id: 'c-1', name: 'Alice', address: CONTACT }]);
    expect(state.settings).toEqual({ theme: 'dark', autoLockMinutes: 15, feeLevel: 3 });

    store.saveAppState({ ...state, activeWallet: '1', settings: { ...state.settings, rpcUrl: 'https://evil.example' } });
    const root = stored(storage);
    expect(root.settings).toEqual({ theme: 'dark', autoLockMinutes: 15, feeLevel: 3 });
    expect(root.activeWallet).toBe('1');
    expect(Object.keys(root.wallets as object)).toEqual(['1']);
  });

  it('does not overwrite wallets added by another tab when saving app state', () => {
    const storage = memoryStorage();
    const tabA = new MetadataStore(storage);
    const tabB = new MetadataStore(storage);
    const state = tabA.appState();
    tabB.putWallet('7', { name: 'Other tab', withPassphrase: false, addressCount: 10, verified: true, added: 1 });
    tabA.saveAppState({ ...state, contacts: [{ id: 'c-1', name: 'Alice', address: CONTACT }] });
    expect(tabA.list().map((w) => w.record.name)).toEqual(['Other tab']);
    expect(tabB.appState().contacts).toHaveLength(1);
  });

  it('removes a wallet with its labels and active selection', () => {
    const { storage, store } = seeded({
      wallets: {
        '1': { name: 'A', withPassphrase: false, addressCount: 10, verified: true, added: 1 },
        '2': { name: 'B', withPassphrase: false, addressCount: 10, verified: true, added: 2 },
      },
      activeWallet: '1',
      labels: { '1': { '0': 'x' }, '2': { '0': 'y' } },
    });
    store.removeWallet('1');
    const root = stored(storage);
    expect(Object.keys(root.wallets as object)).toEqual(['2']);
    expect(root.activeWallet).toBeUndefined();
    expect(root.labels).toEqual({ '2': { '0': 'y' } });
  });
});
