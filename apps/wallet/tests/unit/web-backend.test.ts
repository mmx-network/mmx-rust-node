// Web backend with the real WASM wallet core (tests/.wasm-test/pkg from
// `bash scripts/build-wasm.sh --test`, else src/backend/web/pkg) and the mock
// chain server (tests/e2e/mock-rpc.ts) behind a fake fetch.
import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { isBackendError, type ErrorCode, type SendRequest } from '../../src/backend/types';
import { EXPIRE_DELTA, PREPARED_TTL_MS, WebBackend } from '../../src/backend/web/backend';
import type { FetchLike } from '../../src/backend/web/http';
import { memoryStorage } from '../../src/backend/web/memory-storage';
import { DAMAGED_KEY, LOCK_SIGNAL_KEY, STORAGE_KEY } from '../../src/backend/web/store';
import type { WasmApi } from '../../src/backend/web/wasm';
import { MockRpc, NATIVE, testAddress } from '../e2e/mock-rpc';

interface TestWasm extends WasmApi {
  initSync(input: { module: BufferSource }): unknown;
}

const testPkg = fileURLToPath(new URL('../.wasm-test/pkg/', import.meta.url));
const shippedPkg = fileURLToPath(new URL('../../src/backend/web/pkg/', import.meta.url));

let wasm: TestWasm;

beforeAll(async () => {
  const dir = existsSync(`${testPkg}mmx_wallet_wasm.js`) ? testPkg : shippedPkg;
  if (!existsSync(`${dir}mmx_wallet_wasm.js`)) throw new Error('WASM package missing: run `npm run build:wasm` first');
  wasm = (await import(/* @vite-ignore */ `${dir}mmx_wallet_wasm.js`)) as TestWasm;
  wasm.initSync({ module: readFileSync(`${dir}mmx_wallet_wasm_bg.wasm`) });
});

const RECIPIENT = testAddress('recipient');
const TOKEN = testAddress('token');
const MMX = 1_000_000n;
// Mainnet: min_txfee 20000, io 10000 per input/output, memo 5000 per 32 bytes, sign 10000.
const fee = (inputs: number, owners: number, memoBytes = 0, level = 1) =>
  BigInt(20000 + 10000 * (inputs + 1) + (memoBytes ? 5000 * Math.max(1, Math.ceil(memoBytes / 32)) : 0) + 10000 * owners) *
  BigInt(level);

function mockFetch(rpc: MockRpc, intercept?: (path: string, method: string) => Response | undefined): FetchLike & { calls: string[] } {
  const calls: string[] = [];
  const fn = async (input: string, init: RequestInit) => {
    const url = new URL(input);
    const method = init.method ?? 'GET';
    const path = url.pathname + url.search;
    calls.push(`${method} ${path}`);
    const custom = intercept?.(path, method);
    if (custom) return custom;
    const reply = rpc.handle(method, path, typeof init.body === 'string' ? init.body : '');
    if (reply.drop) throw new TypeError('Failed to fetch');
    return new Response(reply.body === '' ? null : reply.body, {
      status: reply.status,
      headers: reply.contentType ? { 'Content-Type': reply.contentType } : {},
    });
  };
  return Object.assign(fn, { calls });
}

class Events {
  private listeners = new Set<(e: StorageEvent) => void>();
  addEventListener(_type: 'storage', l: (e: StorageEvent) => void) {
    this.listeners.add(l);
  }
  removeEventListener(_type: 'storage', l: (e: StorageEvent) => void) {
    this.listeners.delete(l);
  }
  emit(key: string | null, newValue: string | null = 'x') {
    for (const l of [...this.listeners]) l({ key, newValue } as StorageEvent);
  }
}

let rpc: MockRpc;
let storage: ReturnType<typeof memoryStorage>;
let clock: number;
let fetchFn: ReturnType<typeof mockFetch>;
let events: Events;

function backend(options: { fetch?: FetchLike; storage?: ReturnType<typeof memoryStorage> } = {}): WebBackend {
  return new WebBackend({
    loadWasm: async () => wasm,
    fetch: options.fetch ?? fetchFn,
    storage: options.storage ?? storage,
    rpcUrl: 'https://rpc.test',
    events,
    now: () => clock,
  });
}

async function code(p: Promise<unknown>): Promise<ErrorCode | 'ok'> {
  try {
    await p;
    return 'ok';
  } catch (e) {
    if (isBackendError(e)) return e.code;
    throw e;
  }
}

beforeEach(() => {
  rpc = new MockRpc({ height: 1_000_000 });
  storage = memoryStorage();
  clock = 1_800_000_000_000;
  fetchFn = mockFetch(rpc);
  events = new Events();
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

/** A wallet opened from fresh words (no passphrase), funded on its first address. */
async function openWallet(b: WebBackend, funds: bigint[] = [], passphrase?: string) {
  const words = wasm.generate_mnemonic();
  const info = await b.importWallet({ name: 'Main', mnemonic: words, addressCount: 10, ...(passphrase ? { passphrase } : {}) });
  const addresses = await b.getAddresses(info.id);
  funds.forEach((amount, i) => {
    if (amount > 0n) rpc.fund({ address: addresses[i]!, amount: amount.toString() });
  });
  return { words, info, addresses };
}

function send(walletId: string, amount: bigint, extra: Partial<SendRequest> = {}): SendRequest {
  return { walletId, to: RECIPIENT, contract: NATIVE, amount: amount.toString(), feeLevel: 1, ...extra };
}

/** Every string stored, with JSON values opened up (keys and booleans are not data). */
function storedStrings(dump: Record<string, string>): string[] {
  const out: string[] = [];
  const walk = (v: unknown): void => {
    if (typeof v === 'string') {
      try {
        const parsed: unknown = JSON.parse(v);
        if (typeof parsed === 'object' && parsed !== null) return walk(parsed);
      } catch {
        // Plain text.
      }
      out.push(v);
    } else if (Array.isArray(v)) v.forEach(walk);
    else if (typeof v === 'object' && v !== null) Object.values(v).forEach(walk);
  };
  walk(dump);
  return out;
}

/** No stored value holds any of the words (alone, in a list or inside nested JSON). */
function expectNoWordsStored(dump: Record<string, string>, mnemonic: string, allowed: string[] = []): void {
  const raw = JSON.stringify(dump);
  const words = mnemonic.split(' ');
  for (let i = 0; i + 1 < words.length; i++) expect(raw).not.toContain(`${words[i]} ${words[i + 1]}`);
  for (const s of storedStrings(dump)) {
    if (allowed.includes(s)) continue;
    for (const word of new Set(words)) expect(s).not.toMatch(new RegExp(`\\b${word}\\b`, 'u'));
  }
}

/** Creates a wallet from new words the way the create flow does, after the user confirmed them. */
async function createNew(b: WebBackend, name = 'Main', passphrase?: string) {
  const mnemonic = await b.newRecoveryPhrase();
  const wallet = await b.createWallet({ name, mnemonic, addressCount: 10, ...(passphrase !== undefined ? { passphrase } : {}) });
  return { wallet, mnemonic };
}

describe('creating and opening wallets', () => {
  it('gives new recovery words without storing anything', async () => {
    const b = backend();
    const first = await b.newRecoveryPhrase();
    const second = await b.newRecoveryPhrase();
    expect(first.split(' ')).toHaveLength(24);
    expect(wasm.normalize_mnemonic(first)).toBe(first);
    expect(second).not.toBe(first);
    expect(storage.dump()).toEqual({});
    expect(await b.listWallets()).toEqual([]);
  });

  it('stores a new wallet only when it is created, opened and active, without any secret', async () => {
    const b = backend();
    const mnemonic = await b.newRecoveryPhrase();
    const wallet = await b.createWallet({ name: ' Main ', mnemonic, addressCount: 10 });
    expect(wallet).toEqual({
      id: wasm.fingerprint(mnemonic),
      name: 'Main',
      fingerprint: wasm.fingerprint(mnemonic),
      withPassphrase: false,
      locked: false,
      addressCount: 10,
      verified: true,
    });
    expectNoWordsStored(storage.dump(), mnemonic, ['Main']);
    expect(await b.listWallets()).toEqual([wallet]);
    expect((await b.loadAppState()).activeWallet).toBe(wallet.id);
    expect(await b.getAddresses(wallet.id)).toHaveLength(10);
    expect(await b.revealMnemonic(wallet.id)).toBe(mnemonic);
    expect(await code(b.createWallet({ name: 'Again', mnemonic, addressCount: 10 }))).toBe('wallet_exists');
  });

  it('validates a new wallet before storing it', async () => {
    const b = backend();
    const mnemonic = await b.newRecoveryPhrase();
    expect(await code(b.createWallet({ name: ' ', mnemonic, addressCount: 10 }))).toBe('invalid_argument');
    expect(await code(b.createWallet({ name: 'A', mnemonic, addressCount: 0 }))).toBe('invalid_argument');
    expect(await code(b.createWallet({ name: 'A', mnemonic: mnemonic.split(' ').slice(1).join(' '), addressCount: 10 }))).toBe('invalid_mnemonic');
    expect(storage.dump()).toEqual({});
  });

  it('gives the wallet ID of new words for the saved username', async () => {
    const b = backend();
    const mnemonic = await b.newRecoveryPhrase();
    expect(await b.walletFingerprint({ mnemonic })).toBe(wasm.fingerprint(mnemonic));
    expect(await b.walletFingerprint({ mnemonic, passphrase: '' })).toBe(wasm.fingerprint(mnemonic));
    expect(await b.walletFingerprint({ mnemonic, passphrase: 'pw' })).toBe(wasm.fingerprint(mnemonic, 'pw'));
    expect(await code(b.walletFingerprint({ mnemonic: 'not words' }))).toBe('invalid_mnemonic');
    const created = await b.createWallet({ name: 'A', mnemonic, passphrase: 'pw', addressCount: 10 });
    expect(created.id).toBe(await b.walletFingerprint({ mnemonic, passphrase: 'pw' }));
    expect(storage.dump()[STORAGE_KEY]).not.toContain('pw');
  });

  it('opens a created wallet again with its words', async () => {
    const b = backend();
    const { wallet, mnemonic } = await createNew(b);
    await b.lockAll();
    expect((await b.listWallets())[0]).toMatchObject({ locked: true, verified: true });
    const opened = await b.unlockWallet(wallet.id, { mnemonic });
    expect(opened).toMatchObject({ locked: false, verified: true, withPassphrase: false });
    const addresses = await b.getAddresses(wallet.id);
    expect(addresses).toHaveLength(10);
    expect(new Set(addresses).size).toBe(10);
    expect(await b.revealMnemonic(wallet.id)).toBe(mnemonic);
  });

  it('still finishes the setup of a wallet stored unconfirmed by an earlier version', async () => {
    const words = wasm.generate_mnemonic();
    const id = wasm.fingerprint(words);
    storage.setItem(STORAGE_KEY, JSON.stringify({ wallets: { [id]: { name: 'Old', withPassphrase: false, addressCount: 10, verified: false, added: 1 } } }));
    const b = backend();
    expect((await b.listWallets())[0]).toMatchObject({ id, verified: false, locked: true });
    expect(await b.unlockWallet(id, { mnemonic: words })).toMatchObject({ verified: true, locked: false });
    expect((await backend().listWallets())[0]).toMatchObject({ verified: true });
  });

  it('tells a phrase of another wallet apart from a missing passphrase', async () => {
    const b = backend();
    const plain = await createNew(b, 'Plain');
    await b.lockAll();
    const other = wasm.generate_mnemonic();
    expect(await code(b.unlockWallet(plain.wallet.id, { mnemonic: other }))).toBe('wallet_mismatch');
    expect(await code(b.unlockWallet(plain.wallet.id, { mnemonic: other, passphrase: 'x' }))).toBe('wallet_mismatch');
    expect(await code(b.unlockWallet(plain.wallet.id, { mnemonic: 'not words' }))).toBe('invalid_mnemonic');
    // Correct words with a needless passphrase still open the wallet.
    expect(await code(b.unlockWallet(plain.wallet.id, { mnemonic: plain.mnemonic, passphrase: 'ignored' }))).toBe('ok');
  });

  it('detects locked and wrong passphrases by wallet ID', async () => {
    const b = backend();
    const { wallet, mnemonic } = await createNew(b, 'Vault', 'correct horse');
    expect(wallet.withPassphrase).toBe(true);
    expect(wallet.id).toBe(wasm.fingerprint(mnemonic, 'correct horse'));
    await b.lockAll();
    expect(await code(b.unlockWallet(wallet.id, { mnemonic }))).toBe('passphrase_required');
    expect(await code(b.unlockWallet(wallet.id, { mnemonic, passphrase: '' }))).toBe('passphrase_required');
    expect(await code(b.unlockWallet(wallet.id, { mnemonic, passphrase: 'wrong' }))).toBe('invalid_passphrase');
    const opened = await b.unlockWallet(wallet.id, { mnemonic: mnemonic.toUpperCase().replace(/ /gu, '   '), passphrase: 'correct horse' });
    expect(opened).toMatchObject({ locked: false, verified: true, withPassphrase: true });
    // Passphrase re-check on an open wallet.
    expect(await code(b.unlockWallet(wallet.id, { passphrase: 'correct horse' }))).toBe('ok');
    expect(await code(b.unlockWallet(wallet.id, { passphrase: 'wrong' }))).toBe('invalid_passphrase');
  });

  it('treats an empty passphrase as none', async () => {
    const b = backend();
    const { wallet } = await createNew(b, 'A', '');
    expect(wallet.withPassphrase).toBe(false);
  });

  it('imports with passphrase detection from a saved username on a new device', async () => {
    const words = wasm.generate_mnemonic();
    const expected = wasm.fingerprint(words, 'pass phrase');
    const b = backend();
    const req = { name: 'Saved', mnemonic: words, addressCount: 100, expectedFingerprint: expected };
    expect(await code(b.importWallet(req))).toBe('passphrase_required');
    expect(await code(b.importWallet({ ...req, passphrase: 'nope' }))).toBe('invalid_passphrase');
    expect(await b.listWallets()).toEqual([]);
    const info = await b.importWallet({ ...req, passphrase: 'pass phrase' });
    expect(info).toMatchObject({ id: expected, withPassphrase: true, locked: false, verified: true, addressCount: 100 });
    expect(await b.getAddresses(info.id)).toHaveLength(100);
    expect(await code(b.importWallet({ ...req, passphrase: 'pass phrase' }))).toBe('wallet_exists');
  });

  it('opens a wallet saved with an empty passphrase silently', async () => {
    const words = wasm.generate_mnemonic();
    const b = backend();
    const info = await b.importWallet({ name: 'Old', mnemonic: words, addressCount: 10, expectedFingerprint: wasm.fingerprint(words, '') });
    expect(info).toMatchObject({ withPassphrase: false, locked: false });
  });

  it('validates import input', async () => {
    const b = backend();
    const words = wasm.generate_mnemonic();
    expect(await code(b.importWallet({ name: '', mnemonic: words, addressCount: 10 }))).toBe('invalid_argument');
    expect(await code(b.importWallet({ name: 'x'.repeat(65), mnemonic: words, addressCount: 10 }))).toBe('invalid_argument');
    expect(await code(b.importWallet({ name: 'a\u{202e}b', mnemonic: words, addressCount: 10 }))).toBe('invalid_argument');
    expect(await code(b.importWallet({ name: 'A', mnemonic: words, addressCount: 0 }))).toBe('invalid_argument');
    expect(await code(b.importWallet({ name: 'A', mnemonic: words, addressCount: 101 }))).toBe('invalid_argument');
    expect(await code(b.importWallet({ name: 'A', mnemonic: words, addressCount: 10, expectedFingerprint: '007' }))).toBe('invalid_argument');
    expect(await code(b.importWallet({ name: 'A', mnemonic: words.split(' ').slice(0, 23).join(' '), addressCount: 10 }))).toBe('invalid_mnemonic');
    expect(await b.listWallets()).toEqual([]);
  });

  it('locks, frees keys and forgets prepared transfers', async () => {
    const b = backend();
    const { info } = await openWallet(b, [10n * MMX]);
    const prepared = await b.prepareSend(send(info.id, MMX));
    await b.lockAll();
    expect((await b.listWallets())[0]!.locked).toBe(true);
    expect(await code(b.getAddresses(info.id))).toBe('locked');
    expect(await code(b.broadcast(info.id, prepared.txid))).toBe('prepared_not_found');
    expect(await code(b.unlockWallet(info.id, {}))).toBe('locked');
    expect(rpc.broadcasts).toHaveLength(0);
  });

  it('lockWallet closes one wallet', async () => {
    const b = backend();
    const a = await openWallet(b);
    const second = await b.importWallet({ name: 'Second', mnemonic: wasm.generate_mnemonic(), addressCount: 10 });
    await b.lockWallet(a.info.id);
    const list = await b.listWallets();
    expect(list.find((w) => w.id === a.info.id)!.locked).toBe(true);
    expect(list.find((w) => w.id === second.id)!.locked).toBe(false);
    expect(await code(b.lockWallet('123'))).toBe('wallet_not_found');
  });

  it('changes the number of addresses, renames and removes', async () => {
    const b = backend();
    const { info, addresses } = await openWallet(b);
    const more = await b.setAddressCount(info.id, 25);
    expect(more).toHaveLength(25);
    expect(more.slice(0, 10)).toEqual(addresses);
    expect((await b.listWallets())[0]!.addressCount).toBe(25);
    expect(await code(b.setAddressCount(info.id, 0))).toBe('invalid_argument');
    expect(await code(b.setAddressCount(info.id, 1.5))).toBe('invalid_argument');
    await b.renameWallet(info.id, '  Savings ');
    expect((await b.listWallets())[0]!.name).toBe('Savings');
    expect(await code(b.renameWallet(info.id, ' '))).toBe('invalid_argument');
    await b.saveAppState({ ...(await b.loadAppState()), activeWallet: info.id, labels: { [info.id]: { 0: 'Main' } } });
    await b.removeWallet(info.id);
    expect(await b.listWallets()).toEqual([]);
    expect(await b.loadAppState()).toMatchObject({ labels: {} });
    expect((await b.loadAppState()).activeWallet).toBeUndefined();
    expect(await code(b.getAddresses(info.id))).toBe('wallet_not_found');
    expect(await code(b.removeWallet(info.id))).toBe('wallet_not_found');
  });

  it('remembers wallets across sessions, always locked', async () => {
    const first = backend();
    const { info } = await openWallet(first);
    await first.setActiveWallet(info.id);
    const second = backend();
    expect(await second.listWallets()).toEqual([{ ...info, locked: true }]);
    expect((await second.loadAppState()).activeWallet).toBe(info.id);
  });

  it('keeps working with damaged metadata', async () => {
    storage.setItem(STORAGE_KEY, '{"wallets": {"12": {"name": "Ok", "withPassphrase": false, "addressCount": 3, "verified": true, "added": 1}, "13": 5}');
    const b = backend();
    expect(await b.listWallets()).toEqual([]);
    const { info } = await openWallet(b);
    expect(storage.dump()[DAMAGED_KEY]).toContain('"Ok"');
    expect((await b.listWallets()).map((w) => w.id)).toEqual([info.id]);
  });

  it('follows changes made in another tab', async () => {
    const a = backend();
    const b = backend();
    const { info } = await openWallet(a);
    const listener = vi.fn();
    const stop = a.onExternalChange(listener);
    // Tab B changes the address count; tab A re-derives on next use.
    const records = JSON.parse(storage.getItem(STORAGE_KEY)!) as { wallets: Record<string, { addressCount: number }> };
    records.wallets[info.id]!.addressCount = 4;
    storage.setItem(STORAGE_KEY, JSON.stringify(records));
    events.emit(STORAGE_KEY);
    expect(listener).toHaveBeenCalledTimes(1);
    expect(await a.getAddresses(info.id)).toHaveLength(4);
    // Tab B removes the wallet: tab A closes it.
    await b.removeWallet(info.id);
    events.emit('unrelated-key');
    expect(listener).toHaveBeenCalledTimes(1);
    events.emit(STORAGE_KEY);
    expect(listener).toHaveBeenCalledTimes(2);
    expect(await code(a.getAddresses(info.id))).toBe('wallet_not_found');
    stop();
    events.emit(null);
    expect(listener).toHaveBeenCalledTimes(2);
  });

  it('does not finish an unlock that was overtaken by a lock', async () => {
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    const b = new WebBackend({
      loadWasm: async () => {
        await gate;
        return wasm;
      },
      fetch: fetchFn,
      storage,
      rpcUrl: 'https://rpc.test',
      now: () => clock,
    });
    storage.setItem(STORAGE_KEY, JSON.stringify({ wallets: {} }));
    const words = wasm.generate_mnemonic();
    const pending = code(b.importWallet({ name: 'A', mnemonic: words, addressCount: 10 }));
    await b.lockAll();
    release();
    expect(await pending).toBe('locked');
    expect(await b.listWallets()).toEqual([]);
  });
});

describe('wallet core failures', () => {
  it('locks everything and stops using the core after it crashes', async () => {
    const crashing: WasmApi = {
      generate_mnemonic: () => wasm.generate_mnemonic(),
      normalize_mnemonic: (w) => wasm.normalize_mnemonic(w),
      fingerprint: (w, p) => wasm.fingerprint(w, p),
      validate_address: (a) => wasm.validate_address(a),
      verify_transaction: (t, p) => wasm.verify_transaction(t, p),
      Vault: {
        open: (...args) => {
          const vault = wasm.Vault.open(...args);
          return {
            free: () => vault.free(),
            fingerprint: () => vault.fingerprint(),
            with_passphrase: () => vault.with_passphrase(),
            addresses: () => vault.addresses(),
            set_count: (n: number) => vault.set_count(n),
            mnemonic: () => vault.mnemonic(),
            sign_transfer: (...a: Parameters<typeof vault.sign_transfer>) => vault.sign_transfer(...a),
            max_amount: () => {
              throw new WebAssembly.RuntimeError('unreachable');
            },
          };
        },
      },
    };
    const b = new WebBackend({ loadWasm: async () => crashing, fetch: fetchFn, storage, rpcUrl: 'https://rpc.test', now: () => clock });
    const { info } = await openWallet(b, [10n * MMX]);
    expect(await code(b.maxSendable({ walletId: info.id, to: '', contract: NATIVE, feeLevel: 1 }))).toBe('internal');
    expect((await b.listWallets()).every((w) => w.locked)).toBe(true);
    expect(await code(b.importWallet({ name: 'B', mnemonic: wasm.generate_mnemonic(), addressCount: 10 }))).toBe('internal');
  });

  it('reports a core that cannot load and retries later', async () => {
    let attempts = 0;
    const b = new WebBackend({
      loadWasm: async () => {
        attempts++;
        if (attempts === 1) throw new Error('network');
        return wasm;
      },
      fetch: fetchFn,
      storage,
      rpcUrl: 'https://rpc.test',
      now: () => clock,
    });
    expect(await code(b.newRecoveryPhrase())).toBe('internal');
    expect(await code(b.newRecoveryPhrase())).toBe('ok');
    expect(attempts).toBe(2);
  });
});

describe('chain server access', () => {
  it('reads allowlisted paths only', async () => {
    const b = backend();
    expect(await b.rpcGet('/chain/info')).toMatchObject({ network: 'mainnet', decimals: 6 });
    expect(await code(b.rpcGet('/wallet/keys'))).toBe('invalid_argument');
    expect(await code(b.rpcGet('/transaction/broadcast'))).toBe('invalid_argument');
    expect(await code(b.rpcGet('/address?id=a b'))).toBe('invalid_argument');
    expect(fetchFn.calls).toEqual(['GET /chain/info']);
  });
});

describe('sending', () => {
  it('prepares locally and broadcasts exactly once', async () => {
    const b = backend();
    const { info, addresses } = await openWallet(b, [10n * MMX]);
    const memo = 'Invoice \u{4e16}\u{754c}';
    const prepared = await b.prepareSend(send(info.id, 3n * MMX, { memo, feeLevel: 2 }));
    expect(prepared).toEqual({
      txid: expect.stringMatching(/^[0-9A-F]{64}$/u),
      to: RECIPIENT,
      contract: NATIVE,
      symbol: 'MMX',
      decimals: 6,
      amount: '3000000',
      fee: fee(1, 1, new TextEncoder().encode(memo).length, 2).toString(),
      expires: 1_000_000 + EXPIRE_DELTA,
      height: 1_000_000,
      memo,
    });
    // Nothing signed reaches the chain server before the user confirms.
    expect(fetchFn.calls.filter((c) => c.startsWith('POST'))).toEqual([]);
    await b.broadcast(info.id, prepared.txid.toLowerCase());
    expect(rpc.broadcasts).toHaveLength(1);
    expect(rpc.broadcasts[0]).toMatchObject({ txid: prepared.txid, submissions: 1, sender: addresses[0] });
    expect(rpc.broadcasts[0]!.outputs).toEqual([{ address: RECIPIENT, contract: NATIVE, amount: '3000000', memo }]);
    // A second broadcast of the same transfer makes no request.
    const requests = fetchFn.calls.length;
    expect(await code(b.broadcast(info.id, prepared.txid))).toBe('prepared_not_found');
    expect(fetchFn.calls).toHaveLength(requests);
    rpc.advance(1);
    expect(rpc.broadcasts[0]!.status).toBe('confirmed');
    expect(rpc.balance(RECIPIENT)).toBe(3n * MMX);
    expect(rpc.balance(addresses[0]!)).toBe(7n * MMX - BigInt(prepared.fee));
  });

  it('broadcasts the signed bytes unchanged', async () => {
    const posted: string[] = [];
    const b = backend({
      fetch: mockFetch(rpc, (path, method) => {
        if (method === 'POST') posted.push(path);
        return undefined;
      }),
    });
    const { info } = await openWallet(b, [10n * MMX]);
    const prepared = await b.prepareSend(send(info.id, MMX));
    await b.broadcast(info.id, prepared.txid);
    const raw = rpc.broadcasts[0]!.raw;
    expect(raw).toMatch(/"nonce":\d+/u);
    expect(JSON.parse(raw).id.toUpperCase()).toBe(prepared.txid);
    expect(posted).toEqual(['/transaction/validate', '/transaction/broadcast']);
  });

  it('sends only the transfer the user confirms', async () => {
    const posted: Array<{ path: string; body: string }> = [];
    const watched = mockFetch(rpc);
    const b = backend({
      fetch: async (input, init) => {
        if (init.method === 'POST') posted.push({ path: new URL(input).pathname, body: String(init.body) });
        return watched(input, init);
      },
    });
    const { info } = await openWallet(b, [10n * MMX]);
    // Review, change the priority, go back and edit, review again, confirm.
    const standard = await b.prepareSend(send(info.id, MMX));
    const faster = await b.prepareSend(send(info.id, MMX, { feeLevel: 2 }));
    await b.discardPrepared(standard.txid);
    await b.discardPrepared(faster.txid);
    const final = await b.prepareSend(send(info.id, 2n * MMX));
    expect(new Set([standard.txid, faster.txid, final.txid]).size).toBe(3);
    expect(posted).toEqual([]);
    await b.broadcast(info.id, final.txid);
    // The confirmed transfer only: the node's dry run, then the broadcast.
    expect(posted.map((p) => p.path)).toEqual(['/transaction/validate', '/transaction/broadcast']);
    expect(posted[0]!.body).toBe(posted[1]!.body);
    expect(JSON.parse(posted[1]!.body).id.toUpperCase()).toBe(final.txid);
    expect(rpc.broadcasts.map((t) => t.txid)).toEqual([final.txid]);
  });

  it('binds a prepared transfer to its wallet and expires it', async () => {
    const b = backend();
    const { info } = await openWallet(b, [10n * MMX]);
    const other = await b.importWallet({ name: 'Other', mnemonic: wasm.generate_mnemonic(), addressCount: 10 });
    const p1 = await b.prepareSend(send(info.id, MMX));
    expect(await code(b.broadcast(other.id, p1.txid))).toBe('prepared_not_found');
    clock += PREPARED_TTL_MS + 1;
    expect(await code(b.broadcast(info.id, p1.txid))).toBe('prepared_not_found');
    const p2 = await b.prepareSend(send(info.id, MMX));
    await b.discardPrepared(p2.txid);
    expect(await code(b.broadcast(info.id, p2.txid))).toBe('prepared_not_found');
    expect(rpc.broadcasts).toHaveLength(0);
  });

  it('does not send a transfer that expired before it was confirmed', async () => {
    const b = backend();
    const { info } = await openWallet(b, [10n * MMX]);
    const prepared = await b.prepareSend(send(info.id, MMX));
    rpc.advance(EXPIRE_DELTA);
    expect(await code(b.broadcast(info.id, prepared.txid))).toBe('tx_expired');
    expect(rpc.broadcasts).toHaveLength(0);
    expect(fetchFn.calls.filter((c) => c.startsWith('POST'))).toEqual([]);
    // Nothing is held back for it.
    const max = BigInt(await b.maxSendable({ walletId: info.id, to: '', contract: NATIVE, feeLevel: 1 }));
    expect(max).toBe(10n * MMX - fee(1, 1));
  });

  it('reports a lost reply as a connection problem and never sends again', async () => {
    const b = backend();
    const { info } = await openWallet(b, [10n * MMX]);
    const prepared = await b.prepareSend(send(info.id, MMX));
    rpc.handle('POST', '/__mock/fail', JSON.stringify({ path: '/transaction/broadcast', mode: 'drop', after: true }));
    expect(await code(b.broadcast(info.id, prepared.txid))).toBe('rpc_unreachable');
    expect(rpc.broadcasts).toHaveLength(1);
    expect(await code(b.broadcast(info.id, prepared.txid))).toBe('prepared_not_found');
    expect(rpc.broadcasts[0]!.submissions).toBe(1);
  });

  it('does not send a transfer whose dry run fails, and holds nothing for it', async () => {
    const b = backend();
    const { info } = await openWallet(b, [10n * MMX]);
    const shown = fee(1, 1);
    const cases: Array<[Record<string, unknown>, ErrorCode]> = [
      [{ path: '/transaction/validate', mode: 'did_fail' }, 'tx_rejected'],
      [
        {
          path: '/transaction/validate',
          mode: 'status',
          status: 200,
          body: JSON.stringify({ did_fail: true, total_fee: Number(shown), error: { code: 1975784893, message: 'insufficient funds for mmx1' } }),
        },
        'insufficient_funds',
      ],
      // A fee above the one shown on review.
      [{ path: '/transaction/validate', mode: 'status', status: 200, body: JSON.stringify({ did_fail: false, total_fee: Number(shown) + 1 }) }, 'tx_not_sent'],
      // No answer: nothing was sent, so it is not reported as uncertain.
      [{ path: '/transaction/validate', mode: 'status', status: 503, body: 'busy' }, 'tx_not_sent'],
      [{ path: '/transaction/validate', mode: 'drop' }, 'tx_not_sent'],
      [{ path: '/node/info', mode: 'status', status: 503, body: 'busy' }, 'tx_not_sent'],
    ];
    for (const [rule, expected] of cases) {
      const prepared = await b.prepareSend(send(info.id, MMX));
      rpc.handle('POST', '/__mock/fail', JSON.stringify(rule));
      expect(await code(b.broadcast(info.id, prepared.txid)), JSON.stringify(rule)).toBe(expected);
      const max = BigInt(await b.maxSendable({ walletId: info.id, to: '', contract: NATIVE, feeLevel: 1 }));
      expect(max).toBe(10n * MMX - shown);
    }
    expect(rpc.broadcasts).toHaveLength(0);
    expect(fetchFn.calls.filter((c) => c.includes('/transaction/broadcast'))).toEqual([]);
  });

  it('maps refusals by the node to definite errors', async () => {
    const b = backend();
    const { info } = await openWallet(b, [10n * MMX]);
    const cases: Array<[number, string, ErrorCode]> = [
      [400, 'insufficient funds for transaction fee: 1 > 0', 'insufficient_fee'],
      [400, 'insufficient funds for mmx1abc', 'insufficient_funds'],
      [400, 'transaction expired at height 5', 'tx_expired'],
      [400, 'duplicate tx', 'tx_rejected'],
      [429, 'slow down', 'tx_rejected'],
    ];
    for (const [status, body, expected] of cases) {
      const prepared = await b.prepareSend(send(info.id, MMX));
      rpc.handle('POST', '/__mock/fail', JSON.stringify({ path: '/transaction/broadcast', mode: 'status', status, body }));
      expect(await code(b.broadcast(info.id, prepared.txid)), body).toBe(expected);
    }
    expect(rpc.broadcasts).toHaveLength(0);
    // A refused transfer holds no funds: the whole balance can be sent again.
    const max = BigInt(await b.maxSendable({ walletId: info.id, to: '', contract: NATIVE, feeLevel: 1 }));
    expect(max).toBe(10n * MMX - fee(1, 1));
  });

  it('shows the fee of the signed transfer and checks it locally', async () => {
    const b = backend();
    const { info } = await openWallet(b, [10n * MMX]);
    expect((await b.prepareSend(send(info.id, MMX))).fee).toBe(fee(1, 1).toString());
    expect((await b.prepareSend(send(info.id, MMX, { feeLevel: 5 }))).fee).toBe(fee(1, 1, 0, 5).toString());
    let verified = 0;
    const strict = new WebBackend({
      loadWasm: async () => ({
        ...wasm,
        Vault: wasm.Vault,
        verify_transaction: (tx: string, params: string) => {
          verified++;
          wasm.verify_transaction(tx, params);
          throw new Error('tx_rejected');
        },
      }),
      fetch: fetchFn,
      storage,
      rpcUrl: 'https://rpc.test',
      now: () => clock,
    });
    await strict.unlockWallet(info.id, { mnemonic: await b.revealMnemonic(info.id) });
    expect(await code(strict.prepareSend(send(info.id, MMX)))).toBe('internal');
    expect(verified).toBe(1);
    expect(fetchFn.calls.filter((c) => c.startsWith('POST'))).toEqual([]);
  });

  it('checks the node before signing', async () => {
    const b = backend();
    const { info } = await openWallet(b, [10n * MMX]);
    rpc.synced = false;
    expect(await code(b.prepareSend(send(info.id, MMX)))).toBe('rpc_not_synced');
    rpc.synced = true;
    const mismatch = backend({
      fetch: mockFetch(rpc, (path) => (path === '/node/info' ? new Response(JSON.stringify({ height: 5, is_synced: true, name: 'testnet' })) : undefined)),
    });
    await mismatch.unlockWallet(info.id, { mnemonic: await b.revealMnemonic(info.id) });
    expect(await code(mismatch.prepareSend(send(info.id, MMX)))).toBe('rpc_network_mismatch');
    expect(rpc.broadcasts).toHaveLength(0);
  });

  it('validates every field before signing', async () => {
    const b = backend();
    const { info, addresses } = await openWallet(b, [10n * MMX]);
    const cases: Array<[Partial<SendRequest>, ErrorCode]> = [
      [{ to: 'mmx1invalid' }, 'invalid_address'],
      [{ to: NATIVE }, 'invalid_address'],
      [{ to: RECIPIENT.slice(0, 10).toUpperCase() + RECIPIENT.slice(10) }, 'invalid_address'],
      [{ amount: '0' }, 'invalid_amount'],
      [{ amount: '-1' }, 'invalid_amount'],
      [{ amount: '1.5' }, 'invalid_amount'],
      [{ amount: '1e6' }, 'invalid_amount'],
      [{ memo: 'x'.repeat(65) }, 'invalid_memo'],
      [{ memo: '\u{4e16}'.repeat(22) }, 'invalid_memo'],
      [{ memo: 'a\u{202e}b' }, 'invalid_memo'],
      [{ memo: 'line\nbreak' }, 'invalid_memo'],
      [{ memo: '' }, 'invalid_memo'],
      [{ feeLevel: 4 as 1 }, 'invalid_argument'],
      [{ contract: 'MMX' }, 'invalid_argument'],
      [{ amount: (11n * MMX).toString() }, 'insufficient_funds'],
      [{ contract: TOKEN }, 'insufficient_funds'],
      [{ walletId: '42' }, 'wallet_not_found'],
    ];
    for (const [patch, expected] of cases) {
      expect(await code(b.prepareSend(send(info.id, MMX, patch))), JSON.stringify(patch)).toBe(expected);
    }
    // Uppercase addresses are accepted and normalized.
    expect((await b.prepareSend(send(info.id, MMX, { to: RECIPIENT.toUpperCase() }))).to).toBe(RECIPIENT);
    // Sending to an own address works.
    expect((await b.prepareSend(send(info.id, MMX, { to: addresses[3]! }))).to).toBe(addresses[3]);
  });

  it('computes a maximum that can be sent, and nothing more', async () => {
    const b = backend();
    const { info } = await openWallet(b, [5n * MMX, 2n * MMX, 15_000n]);
    const max = BigInt(await b.maxSendable({ walletId: info.id, to: '', contract: NATIVE, feeLevel: 1 }));
    expect(max).toBeGreaterThan(6n * MMX);
    expect(max).toBeLessThan(7n * MMX);
    const prepared = await b.prepareSend(send(info.id, max));
    expect(prepared.amount).toBe(max.toString());
    expect(await code(b.prepareSend(send(info.id, max + 1n)))).not.toBe('ok');
    const withMemo = BigInt(await b.maxSendable({ walletId: info.id, to: '', contract: NATIVE, feeLevel: 10, memo: 'x'.repeat(40) }));
    expect(withMemo).toBeLessThan(max);
    await b.prepareSend(send(info.id, withMemo, { feeLevel: 10, memo: 'x'.repeat(40) }));
  });

  it('reports an MMX amount that leaves nothing for the fee as missing funds', async () => {
    const b = backend();
    const { info } = await openWallet(b, [2n * MMX]);
    expect(await code(b.prepareSend(send(info.id, 2n * MMX)))).toBe('insufficient_funds');
    expect(await code(b.prepareSend(send(info.id, 3n * MMX)))).toBe('insufficient_funds');
    expect(rpc.broadcasts).toHaveLength(0);
  });

  it('reports zero when nothing can be sent', async () => {
    const b = backend();
    const { info } = await openWallet(b, [1000n]);
    expect(await b.maxSendable({ walletId: info.id, to: '', contract: NATIVE, feeLevel: 1 })).toBe('0');
  });

  it('sends tokens with the fee paid in MMX', async () => {
    rpc.registerToken(TOKEN, 'USDM', 4);
    const b = backend();
    const { info, addresses } = await openWallet(b);
    rpc.fund({ address: addresses[1]!, amount: '125000', contract: TOKEN });
    expect(await code(b.maxSendable({ walletId: info.id, to: '', contract: TOKEN, feeLevel: 1 }))).toBe('insufficient_fee');
    expect(await code(b.prepareSend(send(info.id, 1000n, { contract: TOKEN })))).toBe('insufficient_fee');
    rpc.fund({ address: addresses[0]!, amount: MMX.toString() });
    expect(await b.maxSendable({ walletId: info.id, to: '', contract: TOKEN, feeLevel: 1 })).toBe('125000');
    const prepared = await b.prepareSend(send(info.id, 125000n, { contract: TOKEN }));
    expect(prepared).toMatchObject({ contract: TOKEN, symbol: 'USDM', decimals: 4, amount: '125000', fee: fee(1, 2).toString() });
    await b.broadcast(info.id, prepared.txid);
    rpc.advance(1);
    expect(rpc.balance(RECIPIENT, TOKEN)).toBe(125000n);
  });

  it('requires an open wallet', async () => {
    const b = backend();
    const { info } = await openWallet(b, [10n * MMX]);
    await b.lockWallet(info.id);
    expect(await code(b.prepareSend(send(info.id, MMX)))).toBe('locked');
    expect(await code(b.maxSendable({ walletId: info.id, to: '', contract: NATIVE, feeLevel: 1 }))).toBe('locked');
  });

  it('drops a transfer prepared while the wallet was locked', async () => {
    let lockDuring: (() => Promise<void>) | null = null;
    const tracked = mockFetch(rpc, (path) => {
      if (path.startsWith('/address?') && lockDuring) {
        const run = lockDuring;
        lockDuring = null;
        void run();
      }
      return undefined;
    });
    const b = backend({ fetch: tracked });
    const { info } = await openWallet(b, [10n * MMX]);
    lockDuring = () => b.lockAll();
    expect(await code(b.prepareSend(send(info.id, MMX)))).toBe('locked');
    expect(tracked.calls.some((c) => c.startsWith('GET /address?'))).toBe(true);
    expect(tracked.calls.some((c) => c.startsWith('POST'))).toBe(false);
  });

  it('fetches balances at most four at a time', async () => {
    let active = 0;
    let peak = 0;
    const slow: FetchLike = async (input, init) => {
      active++;
      peak = Math.max(peak, active);
      await new Promise((r) => setTimeout(r, 2));
      try {
        return await fetchFn(input, init);
      } finally {
        active--;
      }
    };
    const b = backend({ fetch: slow });
    const { info } = await openWallet(b, [10n * MMX]);
    await b.setAddressCount(info.id, 40);
    peak = 0;
    await b.maxSendable({ walletId: info.id, to: '', contract: NATIVE, feeLevel: 1 });
    expect(peak).toBeLessThanOrEqual(4);
    expect(peak).toBeGreaterThan(1);
  });
});

describe('funds of broadcast transfers', () => {
  const maxOf = (b: WebBackend, walletId: string) =>
    b.maxSendable({ walletId, to: '', contract: NATIVE, feeLevel: 1 }).then((v) => BigInt(v));

  it('are not offered or spent again until the transfer is included', async () => {
    const b = backend();
    const { info, addresses } = await openWallet(b, [10n * MMX]);
    const first = await b.prepareSend(send(info.id, 6n * MMX));
    await b.broadcast(info.id, first.txid);
    // The chain server still reports 10 MMX on the address.
    expect(rpc.balance(addresses[0]!)).toBe(10n * MMX);
    const left = 10n * MMX - 6n * MMX - BigInt(first.fee);
    expect(await maxOf(b, info.id)).toBe(left - fee(1, 1));
    expect(await code(b.prepareSend(send(info.id, 6n * MMX)))).toBe('insufficient_funds');
    const second = await b.prepareSend(send(info.id, left - fee(1, 1)));
    await b.broadcast(info.id, second.txid);
    expect(rpc.broadcasts.map((t) => t.status)).toEqual(['pending', 'pending']);
    expect(await maxOf(b, info.id)).toBe(0n);
    rpc.advance(1);
    expect(rpc.broadcasts.map((t) => t.status)).toEqual(['confirmed', 'confirmed']);
    rpc.fund({ address: addresses[0]!, amount: (5n * MMX).toString() });
    // Included transfers no longer hold anything: the balance is what the chain server reports.
    expect(await maxOf(b, info.id)).toBe(5n * MMX - fee(1, 1));
  });

  it('make the next transfer use other addresses', async () => {
    const b = backend();
    const { info, addresses } = await openWallet(b, [10n * MMX, 8n * MMX]);
    const first = await b.prepareSend(send(info.id, 6n * MMX));
    await b.broadcast(info.id, first.txid);
    expect(rpc.broadcasts[0]!.inputs.map((i) => i.address)).toEqual([addresses[0]]);
    const second = await b.prepareSend(send(info.id, 6n * MMX));
    await b.broadcast(info.id, second.txid);
    // The node accepted it: it does not spend what the first transfer holds.
    expect(rpc.broadcasts).toHaveLength(2);
    expect(rpc.broadcasts[1]!.inputs.map((i) => i.address)).toEqual([addresses[1]]);
  });

  it('stay held while the outcome is unknown and are released on refusal', async () => {
    const b = backend();
    const { info } = await openWallet(b, [10n * MMX]);
    const full = 10n * MMX - fee(1, 1);
    const lost = await b.prepareSend(send(info.id, MMX));
    rpc.handle('POST', '/__mock/fail', JSON.stringify({ path: '/transaction/broadcast', mode: 'drop' }));
    expect(await code(b.broadcast(info.id, lost.txid))).toBe('rpc_unreachable');
    expect(rpc.broadcasts).toHaveLength(0);
    expect(await maxOf(b, info.id)).toBe(full - MMX - BigInt(lost.fee));
    const busy = await b.prepareSend(send(info.id, MMX));
    rpc.handle('POST', '/__mock/fail', JSON.stringify({ path: '/transaction/broadcast', mode: 'status', status: 503, body: 'lost sync' }));
    expect(await code(b.broadcast(info.id, busy.txid))).toBe('rpc_http_error');
    expect(await maxOf(b, info.id)).toBe(full - 2n * (MMX + BigInt(lost.fee)));
    const refused = await b.prepareSend(send(info.id, MMX));
    rpc.handle('POST', '/__mock/fail', JSON.stringify({ path: '/transaction/broadcast', mode: 'status', status: 400, body: 'invalid signature' }));
    expect(await code(b.broadcast(info.id, refused.txid))).toBe('tx_rejected');
    expect(await maxOf(b, info.id)).toBe(full - 2n * (MMX + BigInt(lost.fee)));
  });

  it('are released when the transfer expires', async () => {
    rpc.confirmBlocks = 1000;
    const b = backend();
    const { info } = await openWallet(b, [10n * MMX]);
    const full = 10n * MMX - fee(1, 1);
    const prepared = await b.prepareSend(send(info.id, MMX));
    await b.broadcast(info.id, prepared.txid);
    rpc.advance(EXPIRE_DELTA);
    // expires == height: it could still be included in the next block.
    expect(await maxOf(b, info.id)).toBe(full - MMX - BigInt(prepared.fee));
    rpc.advance(1);
    expect(rpc.broadcasts[0]!.status).toBe('expired');
    expect(await maxOf(b, info.id)).toBe(full);
  });

  it('check each waiting transfer once per balance read, before the balances', async () => {
    const b = backend();
    const { info } = await openWallet(b, [10n * MMX, 10n * MMX]);
    const first = await b.prepareSend(send(info.id, MMX));
    await b.broadcast(info.id, first.txid);
    const second = await b.prepareSend(send(info.id, MMX));
    await b.broadcast(info.id, second.txid);
    fetchFn.calls.length = 0;
    await maxOf(b, info.id);
    const lookups = fetchFn.calls.filter((c) => c.startsWith('GET /transaction?'));
    expect(lookups.sort()).toEqual([`GET /transaction?id=${first.txid}`, `GET /transaction?id=${second.txid}`].sort());
    const firstBalance = fetchFn.calls.findIndex((c) => c.startsWith('GET /address?'));
    const lastLookup = fetchFn.calls.reduce((last, c, i) => (c.startsWith('GET /transaction?') ? i : last), -1);
    expect(lastLookup).toBeLessThan(firstBalance);
    // Without waiting transfers nothing is looked up.
    rpc.advance(1);
    await maxOf(b, info.id);
    fetchFn.calls.length = 0;
    await maxOf(b, info.id);
    expect(fetchFn.calls.some((c) => c.startsWith('GET /transaction?'))).toBe(false);
  });

  it('stay held when the inclusion lookup fails', async () => {
    const b = backend();
    const { info } = await openWallet(b, [10n * MMX]);
    const prepared = await b.prepareSend(send(info.id, MMX));
    await b.broadcast(info.id, prepared.txid);
    rpc.advance(1);
    expect(rpc.broadcasts[0]!.status).toBe('confirmed');
    // The lookup fails: Max still works and keeps the funds held.
    rpc.handle('POST', '/__mock/fail', JSON.stringify({ path: '/transaction', mode: 'status', status: 503 }));
    const held = await maxOf(b, info.id);
    expect(held).toBe(10n * MMX - MMX - BigInt(prepared.fee) - MMX - BigInt(prepared.fee) - fee(1, 1));
    // The next lookup sees the block and releases them.
    expect(await maxOf(b, info.id)).toBe(10n * MMX - MMX - BigInt(prepared.fee) - fee(1, 1));
  });

  it('are kept while the wallet is locked and forgotten with the wallet', async () => {
    const b = backend();
    const { info, words } = await openWallet(b, [10n * MMX]);
    const prepared = await b.prepareSend(send(info.id, 6n * MMX));
    await b.broadcast(info.id, prepared.txid);
    await b.lockAll();
    await b.unlockWallet(info.id, { mnemonic: words });
    expect(await code(b.prepareSend(send(info.id, 6n * MMX)))).toBe('insufficient_funds');
    await b.removeWallet(info.id);
    await b.importWallet({ name: 'Main', mnemonic: words, addressCount: 10 });
    expect(await maxOf(b, info.id)).toBe(10n * MMX - fee(1, 1));
  });
});

describe('locking other tabs', () => {
  it('an explicit Lock locks every tab, idle and page-hide locks only this one', async () => {
    const a = backend();
    const b = backend();
    const { info, words } = await openWallet(a, [10n * MMX]);
    await b.unlockWallet(info.id, { mnemonic: words });
    const prepared = await b.prepareSend(send(info.id, MMX));
    const changes = vi.fn();
    // What the listener sees when it runs.
    const locks = vi.fn();
    b.onExternalChange(changes);
    b.onExternalLock(() => locks(b.listWallets()));

    // Idle lock in tab A: nothing is signalled.
    await a.lockAll();
    expect(storage.getItem(LOCK_SIGNAL_KEY)).toBeNull();
    expect((await b.listWallets())[0]!.locked).toBe(false);
    expect(await b.revealMnemonic(info.id)).toBe(words);

    await a.unlockWallet(info.id, { mnemonic: words });
    await a.lockAll({ everywhere: true });
    const signal = storage.getItem(LOCK_SIGNAL_KEY);
    expect(signal).not.toBeNull();
    // The browser delivers the change to the other tabs.
    events.emit(LOCK_SIGNAL_KEY, signal);
    expect(locks).toHaveBeenCalledTimes(1);
    expect((await (locks.mock.calls[0]![0] as Promise<Array<{ locked: boolean }>>))[0]!.locked).toBe(true);
    expect(changes).toHaveBeenCalledTimes(1);
    expect(await code(b.revealMnemonic(info.id))).toBe('locked');
    expect(await code(b.broadcast(info.id, prepared.txid))).toBe('prepared_not_found');
    expect(rpc.broadcasts).toHaveLength(0);

    // Every explicit Lock writes a new value, so the browser reports each one.
    await a.unlockWallet(info.id, { mnemonic: words });
    await a.lockAll({ everywhere: true });
    expect(storage.getItem(LOCK_SIGNAL_KEY)).not.toBe(signal);
    // Removing the key is not a lock.
    await b.unlockWallet(info.id, { mnemonic: words });
    events.emit(LOCK_SIGNAL_KEY, null);
    expect((await b.listWallets())[0]!.locked).toBe(false);
    expect(locks).toHaveBeenCalledTimes(1);
  });

  it('a lock from another tab overtakes an unlock in progress', async () => {
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    const b = new WebBackend({
      loadWasm: async () => {
        await gate;
        return wasm;
      },
      fetch: fetchFn,
      storage,
      rpcUrl: 'https://rpc.test',
      events,
      now: () => clock,
    });
    const pending = code(b.importWallet({ name: 'A', mnemonic: wasm.generate_mnemonic(), addressCount: 10 }));
    events.emit(LOCK_SIGNAL_KEY);
    release();
    expect(await pending).toBe('locked');
  });

  it('works without site storage', async () => {
    const blocked = {
      getItem: () => null,
      setItem: () => {
        throw new DOMException('blocked', 'SecurityError');
      },
      removeItem: () => undefined,
    };
    const b = new WebBackend({ loadWasm: async () => wasm, fetch: fetchFn, storage: blocked, rpcUrl: 'https://rpc.test', now: () => clock });
    expect(await code(b.lockAll({ everywhere: true }))).toBe('ok');
  });
});

describe('platform services', () => {
  it('only opens explorer links', async () => {
    const open = vi.fn();
    vi.stubGlobal('window', { open });
    const b = backend();
    const tx = `https://explore.mmx.network/#/explore/transaction/${'A'.repeat(64)}`;
    await b.openExternal(tx);
    expect(open).toHaveBeenCalledWith(tx, '_blank', 'noopener,noreferrer');
    for (const bad of ['https://evil.example', 'javascript:alert(1)', `${tx}/x`, 'https://explore.mmx.network.evil/#/explore/transaction/x']) {
      expect(await code(b.openExternal(bad)), bad).toBe('invalid_argument');
    }
    expect(open).toHaveBeenCalledTimes(1);
  });

  it('clears copied recovery words after a minute unless something else was copied', async () => {
    vi.useFakeTimers();
    const writes: string[] = [];
    vi.stubGlobal('navigator', { clipboard: { writeText: async (t: string) => void writes.push(t) } });
    const b = backend();
    await b.copyText('secret words', true);
    await vi.advanceTimersByTimeAsync(59_000);
    expect(writes).toEqual(['secret words']);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(writes).toEqual(['secret words', '']);
    await b.copyText('secret words', true);
    await b.copyText('mmx1address', false);
    await vi.advanceTimersByTimeAsync(61_000);
    expect(writes).toEqual(['secret words', '', 'secret words', 'mmx1address']);
  });

  it('still clears recovery words when a later copy fails', async () => {
    const writes: string[] = [];
    let fail = false;
    vi.stubGlobal('navigator', {
      clipboard: {
        writeText: async (t: string) => {
          if (fail && t !== '') throw new DOMException('denied', 'NotAllowedError');
          writes.push(t);
        },
      },
    });
    const b = backend();
    await b.copyText('secret words', true);
    fail = true;
    expect(await code(b.copyText('mmx1address'))).toBe('internal');
    await Promise.resolve();
    await new Promise((r) => setTimeout(r, 0));
    expect(writes).toEqual(['secret words', '']);
  });

  /** A window whose listeners can be fired by tests (several per type, like the real one). */
  function fakeWindow() {
    const listeners = new Map<string, Set<() => void>>();
    const win = {
      addEventListener: (type: string, l: () => void) => {
        const set = listeners.get(type) ?? new Set();
        set.add(l);
        listeners.set(type, set);
      },
      removeEventListener: (type: string, l: () => void) => void listeners.get(type)?.delete(l),
    };
    const fire = (type: string) => {
      for (const l of [...(listeners.get(type) ?? [])]) l();
    };
    const count = () => [...listeners.values()].reduce((n, set) => n + set.size, 0);
    return { win, fire, count };
  }

  it('clears recovery words on the next click or key press where browsers require a user gesture', async () => {
    vi.useFakeTimers();
    const writes: string[] = [];
    // Firefox and Safari: clipboard writes only during a user gesture, decided when the call is made.
    let gesture = false;
    vi.stubGlobal('navigator', {
      clipboard: {
        writeText: (t: string) => {
          if (!gesture) return Promise.reject(new DOMException('denied', 'NotAllowedError'));
          writes.push(t);
          return Promise.resolve();
        },
      },
    });
    const { win, fire, count } = fakeWindow();
    vi.stubGlobal('window', win);
    const userAction = (type: string) => {
      gesture = true;
      try {
        fire(type);
      } finally {
        gesture = false;
      }
    };
    const b = backend();
    gesture = true;
    await b.copyText('secret words', true);
    gesture = false;
    await vi.advanceTimersByTimeAsync(60_000);
    expect(writes).toEqual(['secret words']);
    // Focus alone is no user gesture.
    fire('focus');
    await vi.advanceTimersByTimeAsync(10 * 60_000);
    expect(writes).toEqual(['secret words']);
    userAction('pointerdown');
    await vi.advanceTimersByTimeAsync(0);
    expect(writes).toEqual(['secret words', '']);
    expect(count()).toBe(0);
    userAction('keydown');
    await vi.advanceTimersByTimeAsync(0);
    expect(writes).toEqual(['secret words', '']);
  });

  it('tries to clear recovery words when the wallets are locked and when the page is hidden', async () => {
    vi.useFakeTimers();
    const writes: string[] = [];
    let allowed = true;
    vi.stubGlobal('navigator', {
      clipboard: {
        writeText: (t: string) => {
          if (!allowed) return Promise.reject(new DOMException('Document is not focused.', 'NotAllowedError'));
          writes.push(t);
          return Promise.resolve();
        },
      },
    });
    const { win, fire, count } = fakeWindow();
    vi.stubGlobal('window', win);
    const b = backend();
    await b.copyText('secret words', true);
    await b.lockAll();
    await vi.advanceTimersByTimeAsync(0);
    expect(writes).toEqual(['secret words', '']);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(writes).toEqual(['secret words', '']);

    await b.copyText('more words', true);
    allowed = false;
    await b.lockAll({ everywhere: true });
    await vi.advanceTimersByTimeAsync(0);
    expect(count()).toBeGreaterThan(0);
    allowed = true;
    fire('pagehide');
    await vi.advanceTimersByTimeAsync(0);
    expect(writes).toEqual(['secret words', '', 'more words', '']);
    expect(count()).toBe(0);
  });

  it('keeps the clearing of words copied again while an earlier clearing was still running', async () => {
    vi.useFakeTimers();
    const writes: string[] = [];
    const clears: Array<() => void> = [];
    vi.stubGlobal('navigator', {
      clipboard: {
        writeText: (t: string) => {
          writes.push(t);
          return t === '' ? new Promise<void>((resolve) => clears.push(resolve)) : Promise.resolve();
        },
      },
    });
    const b = backend();
    await b.copyText('secret words', true);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(writes).toEqual(['secret words', '']);
    await b.copyText('secret words', true);
    clears[0]!();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(writes).toEqual(['secret words', '', 'secret words', '']);
  });

  it('retries clearing when the page regains focus', async () => {
    vi.useFakeTimers();
    const writes: string[] = [];
    let focused = true;
    const listeners = new Map<string, () => void>();
    vi.stubGlobal('window', {
      addEventListener: (type: string, l: () => void) => listeners.set(type, l),
      removeEventListener: (type: string) => listeners.delete(type),
    });
    vi.stubGlobal('navigator', {
      clipboard: {
        writeText: async (t: string) => {
          if (!focused) throw new DOMException('Document is not focused.', 'NotAllowedError');
          writes.push(t);
        },
      },
    });
    const b = backend();
    await b.copyText('secret words', true);
    focused = false;
    await vi.advanceTimersByTimeAsync(60_000);
    expect(writes).toEqual(['secret words']);
    focused = true;
    listeners.get('focus')?.();
    await vi.advanceTimersByTimeAsync(0);
    expect(writes).toEqual(['secret words', '']);
  });
});
