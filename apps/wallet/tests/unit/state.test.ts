import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { NATIVE_CONTRACT } from '../../src/lib/address';

const START = 1_792_835_101_709;

type State = typeof import('../../src/state');
type Mock = typeof import('../../src/backend/mock');

beforeEach(() => {
  vi.useFakeTimers({ now: START, toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval', 'Date'] });
});

afterEach(() => {
  vi.clearAllTimers();
  vi.useRealTimers();
});

async function boot(platform: 'desktop' | 'web' = 'desktop', options: { empty?: boolean } = {}) {
  vi.resetModules();
  const S: State = await import('../../src/state');
  const M: Mock = await import('../../src/backend/mock');
  const types = await import('../../src/backend/types');
  const backend = M.createMockBackend({ platform, ...options });
  await S.init({ backend });
  await settle();
  return { S, M, backend, types };
}

async function settle(ms = 200): Promise<void> {
  await vi.advanceTimersByTimeAsync(ms);
}

async function codeOf(p: Promise<unknown>): Promise<string> {
  try {
    await p;
    return 'ok';
  } catch (e) {
    return (e as { code?: string }).code ?? 'thrown';
  }
}

describe('desktop start-up', () => {
  it('opens the active wallet and loads chain data', async () => {
    const { S } = await boot();
    expect(S.phase.value).toBe('ready');
    expect(S.platform.value).toBe('desktop');
    expect(S.activeWallet.value?.name).toBe('Main');
    expect(S.addresses.value).toHaveLength(10);
    expect(S.network.value).toMatchObject({ state: 'online', network: 'mainnet' });
    expect(S.chainHeight.value).toBeGreaterThan(5_000_000);
    expect(S.balancesLoaded.value).toBe(true);
    expect(S.nativeAsset.value!.total).toBeGreaterThan(0n);
    expect(S.verifiedAssets.value.map((a) => a.symbol)).toEqual(['MMX']);
    expect(S.otherAssets.value).toHaveLength(1);
    expect(S.otherAssets.value[0]).toMatchObject({ symbol: 'MMX', native: false, verified: false });
    expect(S.historyLoaded.value).toBe(true);
    expect(S.recentActivity.value).toHaveLength(5);
    expect(S.canLock.value).toBe(false);
    expect(S.contacts.value).toHaveLength(3);
    expect(S.activeLabels.value[0]).toBe('Farming');
    expect(S.addressRows.value[0]).toMatchObject({ index: 0, label: 'Farming' });
  });

  it('extends history to cover the chart range and loads more activity', async () => {
    const { S } = await boot();
    await settle(1000);
    const series = S.balanceSeries.value!;
    expect(series.complete).toBe(true);
    expect(series.points.length).toBeGreaterThan(5);
    expect(series.points[series.points.length - 1]!.v).toBe(S.nativeAsset.value!.total);
    S.setSeriesRange('ALL');
    await settle(1000);
    expect(S.balanceSeries.value!.complete).toBe(true);
    expect(S.balanceSeries.value!.points[0]!.v).toBe(0n);
    const before = S.visibleActivity.value.length;
    expect(before).toBe(S.ACTIVITY_PAGE);
    await S.loadMoreActivity();
    expect(S.visibleActivity.value.length).toBeGreaterThan(before);
    S.setActivityFilter({ kind: 'rewards' });
    expect(S.visibleActivity.value.every((i) => i.kind === 'reward')).toBe(true);
    S.setActivityFilter({ kind: 'all', contract: null });
  });

  it('resolves counterparties lazily', async () => {
    const { S } = await boot();
    await settle(1000);
    const received = S.activity.value.find((i) => i.kind === 'received')!;
    expect(S.counterpartyFor(received)).toBeUndefined();
    await settle(50);
    const cp = S.counterpartyFor(received);
    expect(cp).toBeTruthy();
    expect(cp!.address).toMatch(/^mmx1/);
    const reward = S.activity.value.find((i) => i.kind === 'reward')!;
    expect(S.counterpartyFor(reward)).toBeNull();
  });

  it('keeps routes valid for the phase', async () => {
    const { S } = await boot();
    S.navigate('unlock');
    await settle(10);
    expect(S.route.value.name).toBe('overview');
    S.navigate('create');
    await settle(10);
    expect(S.route.value.name).toBe('create');
  });
});

describe('passphrase wallets (desktop)', () => {
  it('switches to a locked wallet, rejects a wrong passphrase, unlocks and locks', async () => {
    const { S, M } = await boot();
    const savings = S.wallets.value.find((w) => w.name === 'Savings')!;
    await S.selectWallet(savings.id);
    await settle(10);
    expect(S.phase.value).toBe('unlock');
    expect(S.route.value.name).toBe('unlock');
    expect(S.activeWallet.value?.id).toBe(savings.id);
    expect(await codeOf(S.unlockWallet(savings.id, { passphrase: 'nope' }))).toBe('invalid_passphrase');
    expect(await codeOf(S.unlockWallet(savings.id, {}))).toBe('passphrase_required');
    await S.unlockWallet(savings.id, { passphrase: M.MOCK_PASSPHRASE });
    await settle();
    expect(S.phase.value).toBe('ready');
    expect(S.route.value.name).toBe('overview');
    expect(S.canLock.value).toBe(true);
    // Mainnet verifies native MMX only; the mock's is_validated flag is ignored.
    expect(S.verifiedAssets.value.map((a) => a.symbol)).toEqual(['MMX']);
    await S.lock();
    await settle(10);
    expect(S.phase.value).toBe('unlock');
    expect(S.addresses.value).toEqual([]);
    await S.selectWallet(S.wallets.value[0]!.id);
    await settle();
    expect(S.phase.value).toBe('ready');
  });

  it('resets the five-minute timeout on spending even without a DOM activity event', async () => {
    const { S, M, backend } = await boot();
    const savings = S.wallets.value.find((w) => w.name === 'Savings')!;
    await S.unlockWallet(savings.id, { passphrase: M.MOCK_PASSPHRASE });
    const trading = S.wallets.value.find((w) => w.name === 'Trading')!;
    const to = (await backend.getAddresses(trading.id))[0]!;
    await S.selectWallet(S.wallets.value.find((w) => w.name === 'Main')!.id);
    await settle();
    await S.startSend({ to, amount: '0.1' });
    S.markActivity(START);
    vi.setSystemTime(START + 4 * 60_000);
    expect(await S.reviewSend()).toBe(true);
    expect(S.idleExpired(START + 5 * 60_000)).toBe(false);
    expect(S.idleExpired(START + 9 * 60_000)).toBe(true);
    vi.setSystemTime(START + 8 * 60_000);
    await S.setSendFeeLevel(2);
    expect(S.idleExpired(START + 9 * 60_000)).toBe(false);
    expect(S.idleExpired(START + 13 * 60_000)).toBe(true);
  });

  it('auto-locks only unlocked passphrase wallets', async () => {
    const { S, M } = await boot();
    expect(S.autoLockDelayMinutes()).toBeNull();
    const savings = S.wallets.value.find((w) => w.name === 'Savings')!;
    await S.unlockWallet(savings.id, { passphrase: M.MOCK_PASSPHRASE });
    expect(S.autoLockDelayMinutes()).toBe(5);
    await S.setAutoLockMinutes(0);
    expect(S.autoLockDelayMinutes()).toBeNull();
    expect(await codeOf(S.setAutoLockMinutes(7))).toBe('invalid_argument');
  });
});

describe('sending', () => {
  async function ready() {
    const ctx = await boot();
    const trading = ctx.S.wallets.value.find((w) => w.name === 'Trading')!;
    const to = (await ctx.backend.getAddresses(trading.id))[3]!;
    return { ...ctx, to };
  }

  it('reviews, broadcasts once and tracks the send until confirmed', async () => {
    const { S, backend, to } = await ready();
    await S.startSend({ to });
    S.updateDraft({ amount: '1.5', memo: 'Lunch' });
    expect(S.draftCheck.value.ready).toBe(true);
    expect(await S.reviewSend()).toBe(true);
    const flow = S.sendFlow.value;
    expect(flow.step).toBe('review');
    expect(flow.prepared).toMatchObject({ to, amount: '1500000', memo: 'Lunch', symbol: 'MMX' });
    const spy = vi.spyOn(backend, 'broadcast');
    await Promise.all([S.confirmSend(), S.confirmSend(), S.confirmSend()]);
    expect(spy).toHaveBeenCalledTimes(1);
    expect(S.sendFlow.value.step).toBe('done');
    expect(S.walletPending.value).toHaveLength(1);
    expect(S.activity.value[0]).toMatchObject({ local: true, status: 'pending', kind: 'sent', to });
    expect(S.pendingOutgoingTotals.value.get(NATIVE_CONTRACT)).toBeGreaterThan(1_500_000n);
    await settle(45_000);
    expect(S.walletPending.value).toHaveLength(0);
    const item = S.activity.value.find((i) => i.txid === flow.prepared!.txid)!;
    expect(item).toMatchObject({ kind: 'sent', status: 'confirmed' });
    expect(item.local).toBeUndefined();
    S.finishSend();
    expect(S.sendDraft.value.to).toBe('');
  });

  it('changes the fee level by preparing a new transaction', async () => {
    const { S, backend, to } = await ready();
    await S.startSend({ to, amount: '2' });
    await S.reviewSend();
    const first = S.sendFlow.value.prepared!;
    await S.setSendFeeLevel(10);
    const second = S.sendFlow.value.prepared!;
    expect(second.txid).not.toBe(first.txid);
    expect(BigInt(second.fee)).toBe(BigInt(first.fee) * 10n);
    expect(backend.preparedCount()).toBe(1);
    await S.backToEdit();
    expect(S.sendFlow.value.step).toBe('edit');
    expect(backend.preparedCount()).toBe(0);
  });

  it('reports an unconfirmed broadcast without retrying', async () => {
    const { S, backend, to, types } = await ready();
    await S.startSend({ to, amount: '1' });
    await S.reviewSend();
    backend.failNextBroadcast = new types.BackendError('rpc_timeout');
    const spy = vi.spyOn(backend, 'broadcast');
    await S.confirmSend();
    expect(S.sendFlow.value.step).toBe('uncertain');
    expect(S.walletPending.value[0]).toMatchObject({ uncertain: true, status: 'pending' });
    await settle(30_000);
    expect(spy).toHaveBeenCalledTimes(1);
  });

  it('validates the draft', async () => {
    const { S, to } = await ready();
    await S.startSend();
    expect(S.draftCheck.value.errors.to).toBeDefined();
    S.updateDraft({ to: 'mmx1nope', amount: '1,5' });
    expect(S.draftCheck.value.errors.to).toBe('Enter a valid MMX address.');
    expect(S.draftCheck.value.errors.amount).toMatch(/decimals/);
    S.updateDraft({ to: NATIVE_CONTRACT });
    expect(S.draftCheck.value.errors.to).toBe('Enter a valid MMX address.');
    S.updateDraft({ to: to.toUpperCase(), amount: '0' });
    expect(S.draftCheck.value.recipient).toBe(to);
    expect(S.draftCheck.value.errors.amount).toBe('Enter an amount greater than zero.');
    S.updateDraft({ amount: '99999999999' });
    expect(S.draftCheck.value.errors.amount).toBe('This is more than your balance.');
    S.updateDraft({ amount: '1.0000001' });
    expect(S.draftCheck.value.errors.amount).toBe('MMX allows up to 6 decimal places.');
    S.updateDraft({ amount: '1', memo: 'x'.repeat(65) });
    expect(S.draftCheck.value.errors.memo).toBeDefined();
    S.updateDraft({ memo: '' });
    expect(S.draftCheck.value.ready).toBe(true);
    const contact = S.contacts.value[0]!;
    S.updateDraft({ to: contact.address });
    expect(S.draftCheck.value.contact?.name).toBe(contact.name);
    expect(await S.reviewSend()).toBe(true);
    await S.discardSend();
    expect(S.sendFlow.value.step).toBe('edit');
  });

  it('fills the maximum amount', async () => {
    const { S, to } = await ready();
    await S.startSend({ to });
    await S.fillMaxAmount();
    expect(S.sendDraft.value.amount).not.toBe('');
    expect(S.draftCheck.value.ready).toBe(true);
    expect(await S.reviewSend()).toBe(true);
  });

  it('asks for the passphrase when the wallet was locked meanwhile', async () => {
    const { S, M, backend, to } = await ready();
    const savings = S.wallets.value.find((w) => w.name === 'Savings')!;
    await S.unlockWallet(savings.id, { passphrase: M.MOCK_PASSPHRASE });
    await settle();
    await S.startSend({ to, amount: '1' });
    await backend.lockWallet(savings.id);
    expect(await S.reviewSend()).toBe(false);
    expect(S.sendFlow.value.needsPassphrase).toBe(true);
    expect(S.sendNeedsPassphrase.value).toBe(true);
    expect(await S.unlockForSend('wrong')).toBe(false);
    expect(S.sendFlow.value.error?.field).toBe('passphrase');
    expect(await S.unlockForSend(M.MOCK_PASSPHRASE)).toBe(true);
    expect(S.sendFlow.value.step).toBe('review');
  });
});

describe('contacts and labels', () => {
  it('adds, validates, edits, deletes and restores contacts', async () => {
    const { S, backend } = await boot();
    const trading = S.wallets.value.find((w) => w.name === 'Trading')!;
    const address = (await backend.getAddresses(trading.id))[0]!;
    const bad = await S.addContact({ name: '', address: 'x' });
    expect(bad).toEqual({ ok: false, errors: { name: 'empty', address: 'invalid' } });
    const own = await S.addContact({ name: 'Me', address: S.addresses.value[0]! });
    expect(own.ok).toBe(false);
    const added = await S.addContact({ name: ' Bob ', address: address.toUpperCase(), note: ' Trader ' });
    expect(added.ok && added.contact).toMatchObject({ name: 'Bob', address, note: 'Trader' });
    expect((await S.addContact({ name: 'Bob 2', address })).ok).toBe(false);
    const id = added.ok ? added.contact.id : '';
    expect((await S.updateContact(id, { name: 'Bobby', address })).ok).toBe(true);
    expect(S.contactForAddress(address)?.name).toBe('Bobby');
    expect(S.searchContacts('bob').map((c) => c.name)).toEqual(['Bobby']);
    expect(S.searchContacts('').length).toBe(4);
    const removed = await S.deleteContact(id);
    expect(S.contactForAddress(address)).toBeUndefined();
    expect(await S.restoreContact(removed!)).toBe('restored');
    expect(await S.restoreContact(removed!)).toBe('duplicate');
    expect((await backend.loadAppState()).contacts).toHaveLength(4);
  });

  it('exports and imports contacts', async () => {
    const { S, backend } = await boot();
    expect(await S.exportContacts()).toBe(true);
    const exported = backend.exported!;
    expect(exported.name).toBe('mmx-contacts.json');
    await S.deleteContact(S.contacts.value[0]!.id);
    backend.nextImport = exported.contents;
    expect(await S.importContacts()).toEqual({ status: 'imported', added: 1, skipped: 2, full: 0 });
    expect(S.contacts.value).toHaveLength(3);
    backend.nextImport = 'garbage';
    expect(await S.importContacts()).toEqual({ status: 'invalid' });
    expect(await S.importContacts()).toEqual({ status: 'cancelled' });
  });

  it('sets and clears address labels', async () => {
    const { S, backend } = await boot();
    await S.setAddressLabel(3, '  Rent  ');
    expect(S.activeLabels.value[3]).toBe('Rent');
    expect(S.describeAddress(S.addresses.value[3]!)).toMatchObject({ name: 'Rent', source: 'own' });
    await S.setAddressLabel(3, '');
    expect(S.activeLabels.value[3]).toBeUndefined();
    expect(await codeOf(S.setAddressLabel(3, 'x'.repeat(33)))).toBe('invalid_argument');
    expect(await codeOf(S.setAddressLabel(100, 'x'))).toBe('invalid_argument');
    expect((await backend.loadAppState()).labels['wallet.dat']).toEqual({ 0: 'Farming', 1: 'Spending' });
  });
});

describe('wallet management', () => {
  it('changes the address count and warns about funded addresses', async () => {
    const { S } = await boot();
    const trading = S.wallets.value.find((w) => w.name === 'Trading')!;
    await S.selectWallet(trading.id);
    await settle();
    expect(S.addresses.value).toHaveLength(12);
    expect(S.fundedAddressesFrom(10)).toContain(11);
    await S.setAddressCount(3);
    await settle();
    expect(S.addresses.value).toHaveLength(3);
    expect(S.activeWallet.value?.addressCount).toBe(3);
    expect(S.historyLoaded.value).toBe(true);
    await S.setAddressCount(12);
    await settle();
    expect(S.addresses.value).toHaveLength(12);
    expect(S.addressRows.value[11]!.hasFunds).toBe(true);
    expect(await codeOf(S.setAddressCount(0))).toBe('invalid_argument');
    expect(await codeOf(S.setAddressCount(101))).toBe('invalid_argument');
  });

  it('renames, removes and re-imports with used-address discovery', async () => {
    const { S, M } = await boot();
    const trading = S.wallets.value.find((w) => w.name === 'Trading')!;
    await S.renameWallet(trading.id, '  Desk  ');
    expect(S.wallets.value.find((w) => w.id === trading.id)!.name).toBe('Desk');
    expect(await codeOf(S.renameWallet(trading.id, ''))).toBe('invalid_argument');
    await S.selectWallet(trading.id);
    await settle();
    await S.removeWallet(trading.id);
    await settle();
    expect(S.wallets.value.some((w) => w.id === trading.id)).toBe(false);
    expect(S.activeWallet.value?.name).toBe('Main');
    expect(S.phase.value).toBe('ready');
    const info = await S.importWallet({ name: 'Trading', mnemonic: `1. ${M.MOCK_WALLETS[2].mnemonic.toUpperCase()}` });
    await settle();
    expect(info.addressCount).toBe(12);
    expect(S.activeWallet.value?.id).toBe(info.id);
    expect(S.addresses.value).toHaveLength(12);
    expect(S.discovery.value).toBeNull();
  });

  it('imports an unused phrase with three addresses and creates wallets', async () => {
    const { S } = await boot();
    const words = Array.from({ length: 24 }, (_, i) => ['zoo', 'abandon', 'ability'][i % 3]).join(' ');
    const info = await S.importWallet({ name: 'Empty', mnemonic: words, passphrase: '' });
    expect(info.addressCount).toBe(3);
    expect(info.withPassphrase).toBe(false);
    expect(await codeOf(S.importWallet({ name: 'Dup', mnemonic: words }))).toBe('wallet_exists');
    expect(await codeOf(S.importWallet({ name: '', mnemonic: words }))).toBe('invalid_argument');
    expect(S.suggestWalletName()).toBe('Wallet');
    const mnemonic = await S.newRecoveryPhrase();
    expect(mnemonic.split(' ')).toHaveLength(24);
    // Nothing is stored before the words are confirmed.
    expect(S.suggestWalletName()).toBe('Wallet');
    expect(S.activeWallet.value?.id).toBe(info.id);
    const created = await S.createWallet({ name: 'Wallet', mnemonic, passphrase: 'pw' });
    await settle();
    expect(S.suggestWalletName()).toBe('Wallet 2');
    expect(S.activeWallet.value?.id).toBe(created.id);
    expect(S.phase.value).toBe('ready');
  });

  it('removing the last wallet returns to onboarding', async () => {
    const { S } = await boot();
    for (const w of [...S.wallets.value]) await S.removeWallet(w.id);
    await settle(10);
    expect(S.phase.value).toBe('onboarding');
    expect(S.route.value.name).toBe('welcome');
  });
});

describe('web wallet', () => {
  it('starts locked, unlocks with the recovery words and locks everything', async () => {
    const { S, M } = await boot('web');
    expect(S.phase.value).toBe('unlock');
    expect(S.canLock.value).toBe(false);
    const main = S.wallets.value[0]!;
    expect(await codeOf(S.unlockWallet(main.id, { mnemonic: M.MOCK_WALLETS[1].mnemonic }))).toBe('wallet_mismatch');
    await S.unlockWallet(main.id, { mnemonic: `  ${M.MOCK_WALLETS[0].mnemonic.replace(/ /g, '\n')} ` });
    await settle();
    expect(S.phase.value).toBe('ready');
    expect(S.canLock.value).toBe(true);
    expect(S.autoLockDelayMinutes()).toBe(5);
    // "Never" turns the idle lock off on the web too (closing the tab still locks).
    await S.setAutoLockMinutes(0);
    expect(S.autoLockDelayMinutes()).toBeNull();
    await S.setAutoLockMinutes(6 * 60);
    expect(S.autoLockDelayMinutes()).toBe(6 * 60);
    await S.lock();
    await settle(10);
    expect(S.phase.value).toBe('unlock');
    expect(S.wallets.value.every((w) => w.locked)).toBe(true);
    expect(S.walletPending.value).toEqual([]);
  });

  it('opens saved credentials: known wallets, new devices and passphrase detection', async () => {
    const { S, M, backend } = await boot('web');
    const [main, savings] = S.wallets.value as [typeof S.wallets.value[0], typeof S.wallets.value[0]];
    const username = S.credentialUsername(main.name, main.fingerprint);
    expect(username).toBe(`Main (${main.fingerprint})`);
    expect(S.parseCredentialUsername(username)).toEqual({ name: 'Main', fingerprint: main.fingerprint });
    const opened = await S.openSavedWallet({ username, words: M.MOCK_WALLETS[0].mnemonic });
    expect(opened.id).toBe(main.id);
    await settle();
    expect(S.phase.value).toBe('ready');
    // A new device: the wallet is not known yet.
    await backend.removeWallet(savings.id);
    await S.lock();
    const savingsUser = S.credentialUsername('Savings', savings.fingerprint);
    expect(await codeOf(S.openSavedWallet({ username: savingsUser, words: M.MOCK_WALLETS[1].mnemonic }))).toBe('passphrase_required');
    expect(await codeOf(S.openSavedWallet({ username: savingsUser, words: M.MOCK_WALLETS[1].mnemonic, passphrase: 'x' }))).toBe('invalid_passphrase');
    const restored = await S.openSavedWallet({ username: savingsUser, words: M.MOCK_WALLETS[1].mnemonic, passphrase: M.MOCK_PASSPHRASE });
    await settle();
    expect(restored.fingerprint).toBe(savings.fingerprint);
    expect(S.activeWallet.value?.name).toBe('Savings');
    await S.lock();
    // No wallet ID in the username, wallet already on this device.
    const again = await S.openSavedWallet({ username: 'whatever', words: M.MOCK_WALLETS[0].mnemonic });
    expect(again.id).toBe(main.id);
  });

  it('returns to the confirm step for a wallet an earlier version stored unconfirmed', async () => {
    vi.resetModules();
    const S: State = await import('../../src/state');
    const M: Mock = await import('../../src/backend/mock');
    const backend = M.createMockBackend({ platform: 'web' });
    const list = await backend.listWallets();
    const wallet = list[2]!;
    vi.spyOn(backend, 'listWallets').mockImplementation(async () => list.map((w) => (w.id === wallet.id ? { ...w, verified: false } : w)));
    await S.init({ backend });
    await settle();
    expect(S.activeWallet.value?.id).toBe(wallet.id);
    expect(S.activeWallet.value?.verified).toBe(false);
    expect(S.route.value.name).toBe('unlock');
  });

  it('allows opening a saved wallet during onboarding', async () => {
    const { S } = await boot('web', { empty: true });
    expect(S.phase.value).toBe('onboarding');
    S.navigate('unlock');
    await settle(10);
    expect(S.route.value.name).toBe('unlock');
    S.navigate('overview');
    await settle(10);
    expect(S.route.value.name).toBe('welcome');
  });
});

describe('settings', () => {
  it('persists theme and validates the network server', async () => {
    const { S, backend } = await boot();
    await S.setTheme('dark');
    expect((await backend.loadAppState()).settings.theme).toBe('dark');
    expect(S.normalizeRpcUrl('node.example:11380/')).toBe('https://node.example:11380');
    expect(S.normalizeRpcUrl('ftp://x')).toBeNull();
    expect(S.normalizeRpcUrl('https://u:p@x.example')).toBeNull();
    expect(S.normalizeRpcUrl('https://x.example/?a=1')).toBeNull();
    expect(await codeOf(S.setRpcUrl('https://offline.example'))).toBe('rpc_unreachable');
    expect(S.settings.value.rpcUrl).toBeUndefined();
    await S.setRpcUrl('node.example');
    expect(S.settings.value.rpcUrl).toBe('https://node.example');
    await settle();
    expect(S.phase.value).toBe('ready');
    await S.setRpcUrl(null);
    expect(S.settings.value.rpcUrl).toBeUndefined();
  });
});
