import { describe, expect, it } from 'vitest';
import { createMockBackend, MOCK_PASSPHRASE, MOCK_WALLETS, type MockBackend } from '../../src/backend/mock';
import { BackendError, type WalletInfo } from '../../src/backend/types';
import { parseAddressBalances, parseChainParams, parseHistory, parseNodeInfo, parseTransaction } from '../../src/chain/rpc';
import { NATIVE_CONTRACT } from '../../src/lib/address';

const START = 1_792_835_101_709;

function setup(platform: 'desktop' | 'web' = 'desktop') {
  let now = START;
  const backend = createMockBackend({ platform, now: () => now });
  return { backend, advance: (ms: number) => (now += ms), now: () => now };
}

async function code(p: Promise<unknown>): Promise<string> {
  try {
    await p;
    return 'ok';
  } catch (e) {
    return e instanceof BackendError ? e.code : 'thrown';
  }
}

async function params(b: MockBackend) {
  return parseChainParams(await b.rpcGet('/chain/info'));
}

describe('mock wallets (desktop)', () => {
  it('lists three wallets with the passphrase wallet locked', async () => {
    const { backend } = setup();
    const list = await backend.listWallets();
    expect(list.map((w) => [w.name, w.withPassphrase, w.locked, w.addressCount])).toEqual([
      ['Main', false, false, 10],
      ['Savings', true, true, 10],
      ['Trading', false, false, 12],
    ]);
    expect(list[0]!.id).toBe('wallet.dat');
    expect(list[1]!.id).toMatch(/^wallet_\d+\.dat$/);
    expect(list.every((w) => /^\d{1,10}$/.test(w.fingerprint))).toBe(true);
    expect((await backend.loadAppState()).activeWallet).toBe('wallet.dat');
  });

  it('detects wrong and missing passphrases', async () => {
    const { backend } = setup();
    const savings = (await backend.listWallets())[1]!;
    expect(await code(backend.getAddresses(savings.id))).toBe('locked');
    expect(await code(backend.unlockWallet(savings.id, {}))).toBe('passphrase_required');
    expect(await code(backend.unlockWallet(savings.id, { passphrase: 'wrong' }))).toBe('invalid_passphrase');
    const info = await backend.unlockWallet(savings.id, { passphrase: MOCK_PASSPHRASE });
    expect(info.locked).toBe(false);
    expect(await backend.getAddresses(savings.id)).toHaveLength(10);
    await backend.lockAll();
    expect((await backend.listWallets()).map((w) => w.locked)).toEqual([false, true, false]);
  });

  it('creates, imports, renames, resizes and removes wallets', async () => {
    const { backend } = setup();
    const before = (await backend.listWallets()).length;
    const mnemonic = await backend.newRecoveryPhrase();
    expect(mnemonic.split(' ')).toHaveLength(24);
    // New words alone store nothing.
    expect(await backend.listWallets()).toHaveLength(before);
    const wallet = await backend.createWallet({ name: 'New', mnemonic, addressCount: 10 });
    expect(wallet.locked).toBe(false);
    expect((await backend.loadAppState()).activeWallet).toBe(wallet.id);
    expect(await code(backend.createWallet({ name: 'Twice', mnemonic, addressCount: 10 }))).toBe('wallet_exists');
    expect(await code(backend.importWallet({ name: 'Again', mnemonic, addressCount: 10 }))).toBe('wallet_exists');
    expect(await code(backend.importWallet({ name: 'Bad', mnemonic: 'abandon '.repeat(23) + 'zzz', addressCount: 10 }))).toBe('invalid_mnemonic');
    expect(await code(backend.renameWallet(wallet.id, '  '))).toBe('invalid_argument');
    await backend.renameWallet(wallet.id, ' Renamed ');
    expect((await backend.listWallets()).find((w) => w.id === wallet.id)!.name).toBe('Renamed');
    expect(await backend.setAddressCount(wallet.id, 3)).toHaveLength(3);
    expect(await code(backend.setAddressCount(wallet.id, 101))).toBe('invalid_argument');
    expect(await backend.revealMnemonic(wallet.id)).toBe(mnemonic);
    await backend.removeWallet(wallet.id);
    expect(await code(backend.getAddresses(wallet.id))).toBe('wallet_not_found');
  });
});

describe('mock wallets (web)', () => {
  it('starts locked and opens with the recovery words', async () => {
    const { backend } = setup('web');
    const list = await backend.listWallets();
    expect(list.every((w) => w.locked && w.verified === true)).toBe(true);
    expect(list[0]!.id).toBe(list[0]!.fingerprint);
    const [main, savings] = list as [WalletInfo, WalletInfo];
    expect(await code(backend.unlockWallet(main.id, { mnemonic: MOCK_WALLETS[2].mnemonic }))).toBe('wallet_mismatch');
    expect(await code(backend.unlockWallet(main.id, { mnemonic: 'nope' }))).toBe('invalid_mnemonic');
    expect((await backend.unlockWallet(main.id, { mnemonic: MOCK_WALLETS[0].mnemonic.toUpperCase() })).locked).toBe(false);
    expect(await code(backend.unlockWallet(savings.id, { mnemonic: MOCK_WALLETS[1].mnemonic }))).toBe('passphrase_required');
    expect(await code(backend.unlockWallet(savings.id, { mnemonic: MOCK_WALLETS[1].mnemonic, passphrase: 'x' }))).toBe('invalid_passphrase');
    expect((await backend.unlockWallet(savings.id, { mnemonic: MOCK_WALLETS[1].mnemonic, passphrase: MOCK_PASSPHRASE })).locked).toBe(false);
    await backend.lockAll();
    expect((await backend.listWallets()).every((w) => w.locked)).toBe(true);
  });

  it('stores a new wallet open and confirmed, with the ID announced before', async () => {
    const { backend } = setup('web');
    const mnemonic = await backend.newRecoveryPhrase();
    const id = await backend.walletFingerprint({ mnemonic, passphrase: 'pw' });
    expect(id).not.toBe(await backend.walletFingerprint({ mnemonic }));
    expect(await backend.walletFingerprint({ mnemonic, passphrase: '' })).toBe(await backend.walletFingerprint({ mnemonic }));
    const wallet = await backend.createWallet({ name: 'Fresh', mnemonic, passphrase: 'pw', addressCount: 10 });
    expect(wallet).toMatchObject({ id, fingerprint: id, locked: false, verified: true, withPassphrase: true });
    expect(await backend.getAddresses(wallet.id)).toHaveLength(10);
  });

  it('detects the passphrase from an expected fingerprint on import', async () => {
    const { backend } = setup('web');
    const savings = (await backend.listWallets())[1]!;
    await backend.removeWallet(savings.id);
    const req = { name: 'Savings', mnemonic: MOCK_WALLETS[1].mnemonic, addressCount: 10, expectedFingerprint: savings.fingerprint };
    expect(await code(backend.importWallet(req))).toBe('passphrase_required');
    expect(await code(backend.importWallet({ ...req, passphrase: 'nope' }))).toBe('invalid_passphrase');
    const info = await backend.importWallet({ ...req, passphrase: MOCK_PASSPHRASE });
    expect(info.fingerprint).toBe(savings.fingerprint);
  });
});

describe('mock chain server', () => {
  it('serves valid node and chain info', async () => {
    const { backend, advance } = setup();
    const p = await params(backend);
    expect(p.network).toBe('mainnet');
    const info = parseNodeInfo(await backend.rpcGet('/node/info'));
    expect(info).toMatchObject({ synced: true, network: 'mainnet' });
    advance(25_000);
    expect(parseNodeInfo(await backend.rpcGet('/node/info')).height).toBe(info.height + 2);
  });

  it('enforces the request allowlist', async () => {
    const { backend } = setup();
    expect(await code(backend.rpcGet('/wallet/keys'))).toBe('invalid_argument');
    expect(await code(backend.rpcGet('/address?id=mmx1<x>'))).toBe('invalid_argument');
    expect(await code(backend.rpcGet('/address/history?id=nope&limit=5'))).toBe('rpc_http_error');
    expect(await code(backend.rpcGet('/address/history?id=nope&limit=5000'))).toBe('rpc_http_error');
  });

  it('has history consistent with balances for every address', async () => {
    const { backend } = setup();
    const p = await params(backend);
    let total = 0;
    for (const w of await backend.listWallets()) {
      if (w.locked) await backend.unlockWallet(w.id, { passphrase: MOCK_PASSPHRASE });
      const counts = new Map<string, number>();
      for (const a of await backend.getAddresses(w.id)) {
        const rows = parseHistory(await backend.rpcGet(`/address/history?id=${a}&limit=1000`), p, a);
        expect(rows.length).toBeLessThan(1000);
        total += rows.length;
        const sums = new Map<string, bigint>();
        for (const r of rows) {
          const d = r.type === 'SPEND' || r.type === 'TXFEE' ? -r.amount : r.amount;
          sums.set(r.contract, (sums.get(r.contract) ?? 0n) + d);
          counts.set(r.txid, (counts.get(r.txid) ?? 0) + 1);
        }
        const balances = parseAddressBalances(await backend.rpcGet(`/address?id=${a}&limit=1000`), p);
        for (const b of balances) expect(sums.get(b.contract)).toBe(b.amount);
        for (const [c, s] of sums) if (s !== 0n) expect(balances.find((b) => b.contract === c)?.amount).toBe(s);
      }
    }
    expect(total).toBeGreaterThan(700);
  });

  it('pages with since/until inclusive and completes the last block', async () => {
    const { backend } = setup();
    const p = await params(backend);
    const a = (await backend.getAddresses('wallet.dat'))[0]!;
    const all = parseHistory(await backend.rpcGet(`/address/history?id=${a}&limit=1000`), p, a);
    expect(all.length).toBeGreaterThan(150);
    const page = parseHistory(await backend.rpcGet(`/address/history?id=${a}&limit=10`), p, a);
    expect(page.length).toBeGreaterThanOrEqual(10);
    const low = Math.min(...page.map((r) => r.height));
    expect(page.length).toBe(all.filter((r) => r.height >= low).length);
    const mid = all[50]!.height;
    const ranged = parseHistory(await backend.rpcGet(`/address/history?id=${a}&limit=1000&since=${mid}&until=${mid}`), p, a);
    expect(ranged.length).toBeGreaterThan(0);
    expect(ranged.every((r) => r.height === mid)).toBe(true);
    for (let i = 1; i < all.length; i++) expect(all[i - 1]!.height).toBeGreaterThanOrEqual(all[i]!.height);
  });

  it('includes a token named MMX that is not native and another token', async () => {
    const { backend } = setup();
    const p = await params(backend);
    const symbols = new Map<string, string>();
    for (const w of await backend.listWallets()) {
      if (w.locked) continue;
      for (const a of await backend.getAddresses(w.id)) {
        for (const r of parseAddressBalances(await backend.rpcGet(`/address?id=${a}&limit=1000`), p)) symbols.set(r.contract, r.symbol);
      }
    }
    const tokens = [...symbols].filter(([c]) => c !== NATIVE_CONTRACT);
    expect(tokens.some(([, s]) => s === 'MMX')).toBe(true);
    expect(tokens.some(([, s]) => s === 'USDM')).toBe(true);
  });

  it('serves only the reads the wallet makes', async () => {
    const { backend } = setup();
    const [a] = await backend.getAddresses('wallet.dat');
    await expect(backend.rpcGet(`/balance?id=${a}`)).rejects.toMatchObject({ code: 'invalid_argument' });
  });
});

describe('mock sending', () => {
  it('prepares, broadcasts and confirms after two blocks', async () => {
    const { backend, advance } = setup();
    const p = await params(backend);
    const [from] = await backend.getAddresses('wallet.dat');
    const to = (await backend.getAddresses((await backend.listWallets())[2]!.id))[4]!;
    const prepared = await backend.prepareSend({ walletId: 'wallet.dat', to, contract: NATIVE_CONTRACT, amount: '1500000', memo: 'Hi', feeLevel: 2 });
    expect(prepared).toMatchObject({ to, amount: '1500000', symbol: 'MMX', decimals: 6, memo: 'Hi' });
    expect(BigInt(prepared.fee) % 2n).toBe(0n);
    expect(prepared.expires).toBe(prepared.height + 100);
    await backend.broadcast('wallet.dat', prepared.txid);
    expect(await code(backend.broadcast('wallet.dat', prepared.txid))).toBe('prepared_not_found');
    const pending = parseTransaction(await backend.rpcGet(`/transaction?id=${prepared.txid}`), p)!;
    expect(pending.height).toBeUndefined();
    advance(20_000);
    const done = parseTransaction(await backend.rpcGet(`/transaction?id=${prepared.txid}`), p)!;
    expect(done.height).toBe(prepared.height + 2);
    expect(done.outputs[0]).toMatchObject({ address: to, amount: 1_500_000n, memo: 'Hi' });
    const rows = parseHistory(await backend.rpcGet(`/address/history?id=${to}&limit=5`), p, to);
    expect(rows.some((r) => r.txid === prepared.txid && r.type === 'RECEIVE')).toBe(true);
    expect(from).toBeDefined();
  });

  it('validates send requests', async () => {
    const { backend } = setup();
    const to = (await backend.getAddresses((await backend.listWallets())[2]!.id))[0]!;
    const base = { walletId: 'wallet.dat', to, contract: NATIVE_CONTRACT, amount: '1', feeLevel: 1 as const };
    expect(await code(backend.prepareSend({ ...base, to: NATIVE_CONTRACT }))).toBe('invalid_address');
    expect(await code(backend.prepareSend({ ...base, to: 'mmx1nope' }))).toBe('invalid_address');
    expect(await code(backend.prepareSend({ ...base, amount: '0' }))).toBe('invalid_amount');
    expect(await code(backend.prepareSend({ ...base, amount: '1.5' }))).toBe('invalid_amount');
    expect(await code(backend.prepareSend({ ...base, amount: '9'.repeat(30) }))).toBe('insufficient_funds');
    expect(await code(backend.prepareSend({ ...base, memo: 'x'.repeat(65) }))).toBe('invalid_memo');
    expect(await code(backend.prepareSend({ ...base, feeLevel: 4 as never }))).toBe('invalid_argument');
    const savings = (await backend.listWallets())[1]!;
    expect(await code(backend.prepareSend({ ...base, walletId: savings.id }))).toBe('locked');
  });

  it('max sendable is accepted and leaves the fee', async () => {
    const { backend } = setup();
    const trading = (await backend.listWallets())[2]!;
    const to = (await backend.getAddresses('wallet.dat'))[5]!;
    for (const memo of [undefined, 'x', 'y'.repeat(33)]) {
      for (const feeLevel of [1, 10] as const) {
        const req = { walletId: trading.id, to, contract: NATIVE_CONTRACT, feeLevel, ...(memo ? { memo } : {}) };
        const max = await backend.maxSendable(req);
        expect(BigInt(max)).toBeGreaterThan(0n);
        const prepared = await backend.prepareSend({ ...req, amount: max });
        await backend.discardPrepared(prepared.txid);
        expect(await code(backend.prepareSend({ ...req, amount: (BigInt(max) + 1n).toString() }))).not.toBe('ok');
      }
    }
  });

  it('holds back funds of sends that are not included yet', async () => {
    const { backend, advance } = setup();
    const trading = (await backend.listWallets())[2]!;
    const to = (await backend.getAddresses('wallet.dat'))[5]!;
    const req = { walletId: trading.id, to, contract: NATIVE_CONTRACT, feeLevel: 1 as const };
    const max = BigInt(await backend.maxSendable(req));
    const half = max / 2n;
    const first = await backend.prepareSend({ ...req, amount: half.toString() });
    await backend.broadcast(trading.id, first.txid);
    // The first send's amount and fee are not offered again.
    const after = BigInt(await backend.maxSendable(req));
    expect(after).toBeLessThan(max - half);
    expect(await code(backend.prepareSend({ ...req, amount: (after + 1n).toString() }))).toBe('insufficient_funds');
    const second = await backend.prepareSend({ ...req, amount: after.toString() });
    await backend.broadcast(trading.id, second.txid);
    // Both are included (the mock chain throws on an overspent address) and nothing is held any more.
    advance(25_000);
    const p = await params(backend);
    for (const s of [first, second]) expect(parseTransaction(await backend.rpcGet(`/transaction?id=${s.txid}`), p)!.height).toBeDefined();
    expect(BigInt(await backend.maxSendable(req))).toBeLessThan(10n ** 6n);
  });

  it('holds back an uncertain send but not a refused one', async () => {
    const { backend } = setup();
    const trading = (await backend.listWallets())[2]!;
    const to = (await backend.getAddresses('wallet.dat'))[5]!;
    const req = { walletId: trading.id, to, contract: NATIVE_CONTRACT, feeLevel: 1 as const };
    const max = BigInt(await backend.maxSendable(req));
    const refused = await backend.prepareSend({ ...req, amount: '1000000' });
    backend.failNextBroadcast = new BackendError('tx_rejected');
    expect(await code(backend.broadcast(trading.id, refused.txid))).toBe('tx_rejected');
    expect(BigInt(await backend.maxSendable(req))).toBe(max);
    const unknown = await backend.prepareSend({ ...req, amount: '1000000' });
    backend.failNextBroadcast = new BackendError('rpc_timeout');
    expect(await code(backend.broadcast(trading.id, unknown.txid))).toBe('rpc_timeout');
    expect(BigInt(await backend.maxSendable(req))).toBeLessThanOrEqual(max - 1_000_000n - BigInt(unknown.fee));
  });

  it('expires prepared transactions after ten minutes', async () => {
    const { backend, advance } = setup();
    const to = (await backend.getAddresses((await backend.listWallets())[2]!.id))[0]!;
    const prepared = await backend.prepareSend({ walletId: 'wallet.dat', to, contract: NATIVE_CONTRACT, amount: '1', feeLevel: 1 });
    advance(10 * 60_000 + 1);
    expect(await code(backend.broadcast('wallet.dat', prepared.txid))).toBe('prepared_not_found');
    const other = await backend.prepareSend({ walletId: 'wallet.dat', to, contract: NATIVE_CONTRACT, amount: '1', feeLevel: 1 });
    expect(await code(backend.broadcast((await backend.listWallets())[2]!.id, other.txid))).toBe('prepared_not_found');
  });

  it('only opens explorer links', async () => {
    const { backend } = setup();
    expect(await code(backend.openExternal('https://evil.example'))).toBe('invalid_argument');
    await backend.openExternal(`https://explore.mmx.network/#/explore/transaction/${'A'.repeat(64)}`);
    expect(backend.lastOpened).toContain('explore.mmx.network');
  });
});
