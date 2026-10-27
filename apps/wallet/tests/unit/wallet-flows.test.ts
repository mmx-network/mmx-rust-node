// Regression tests for wallet switching, locking, sending and settings races.
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { effect } from '@preact/signals';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Backend } from '../../src/backend/types';
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

async function settle(ms = 200): Promise<void> {
  await vi.advanceTimersByTimeAsync(ms);
}

async function boot(platform: 'desktop' | 'web' = 'desktop', prepare?: (b: InstanceType<Mock['MockBackend']>) => void) {
  vi.resetModules();
  const S: State = await import('../../src/state');
  const M: Mock = await import('../../src/backend/mock');
  const session = await import('../../src/state/session');
  const types = await import('../../src/backend/types');
  const backend = M.createMockBackend({ platform });
  prepare?.(backend);
  await S.init({ backend });
  await settle();
  return { S, M, backend, session, types };
}

/** A promise the test resolves itself. */
function gate(): { wait: Promise<void>; open: () => void } {
  let open!: () => void;
  const wait = new Promise<void>((resolve) => {
    open = resolve;
  });
  return { wait, open };
}

describe('address count and wallet switches', () => {
  it('a count change finishing after a switch does not replace the other wallet’s addresses', async () => {
    const { S, backend } = await boot();
    const main = S.wallets.value.find((w) => w.name === 'Main')!;
    const trading = S.wallets.value.find((w) => w.name === 'Trading')!;
    const tradingAddresses = await backend.getAddresses(trading.id);
    const hold = gate();
    const original = backend.setAddressCount.bind(backend);
    vi.spyOn(backend, 'setAddressCount').mockImplementation(async (id, n) => {
      await hold.wait;
      return original(id, n);
    });
    const saving = S.setAddressCount(20);
    await S.selectWallet(trading.id);
    await settle();
    hold.open();
    await saving;
    await settle();
    expect(S.activeWallet.value?.id).toBe(trading.id);
    expect(S.addresses.value).toEqual(tradingAddresses);
    expect(S.wallets.value.find((w) => w.id === main.id)!.addressCount).toBe(20);
  });

  it('checks for funds against the stored count when it changed elsewhere', async () => {
    const { S, backend } = await boot();
    const trading = S.wallets.value.find((w) => w.name === 'Trading')!;
    await S.selectWallet(trading.id);
    await settle();
    await S.setAddressCount(3);
    await settle();
    expect(S.addresses.value).toHaveLength(3);
    // Another tab or the command line shows all 12 addresses again; address 12 holds funds.
    await backend.setAddressCount(trading.id, 12);
    expect(S.activeWallet.value?.addressCount).toBe(3);
    expect(await S.checkHiddenFunds(5)).toEqual({ kind: 'funded', indices: [11] });
    expect(S.addresses.value).toHaveLength(12);
    expect(await S.checkHiddenFunds(12)).toEqual({ kind: 'none' });
  });

  it('asks before hiding addresses whose balance is unknown', async () => {
    const { S, session } = await boot();
    const trading = S.wallets.value.find((w) => w.name === 'Trading')!;
    await S.selectWallet(trading.id);
    await settle();
    session.balanceMap.value = null;
    expect(await S.checkHiddenFunds(5)).toEqual({ kind: 'unknown' });
  });
});

describe('network server', () => {
  it('stays on Settings and keeps the wallet open while switching servers', async () => {
    const { S } = await boot();
    S.navigate('settings');
    await settle(10);
    const seen: string[] = [];
    const stop = effect(() => {
      seen.push(`${S.phase.value}:${S.route.value.name}`);
    });
    await S.setRpcUrl('node.example');
    await settle();
    stop();
    expect(new Set(seen)).toEqual(new Set(['ready:settings']));
    expect(S.settings.value.rpcUrl).toBe('https://node.example');
    expect(S.network.value.state).toBe('online');
  });

  it('changes the saved server only after it was accepted', async () => {
    const { S, backend, types } = await boot();
    const hold = gate();
    const original = backend.saveAppState.bind(backend);
    vi.spyOn(backend, 'saveAppState').mockImplementation(async (state) => {
      if (state.settings.rpcUrl) await hold.wait;
      return original(state);
    });
    const switching = S.setRpcUrl('https://unreachable.example');
    await settle(10);
    // While the server is checked the setting is unchanged, and other saves don't carry the untested address.
    expect(S.settings.value.rpcUrl).toBeUndefined();
    const theme = S.setTheme('dark');
    hold.open();
    await expect(switching).rejects.toBeInstanceOf(types.BackendError);
    await theme;
    expect(S.settings.value.rpcUrl).toBeUndefined();
    expect(S.settings.value.theme).toBe('dark');
    expect((await backend.loadAppState()).settings).toMatchObject({ theme: 'dark' });
    expect((await backend.loadAppState()).settings.rpcUrl).toBeUndefined();
  });
});

describe('sending', () => {
  async function ready() {
    const ctx = await boot();
    const trading = ctx.S.wallets.value.find((w) => w.name === 'Trading')!;
    const to = (await ctx.backend.getAddresses(trading.id))[3]!;
    return { ...ctx, trading, to };
  }

  it('a broadcast that finishes after a wallet switch is not shown in the other wallet', async () => {
    const { S, backend, session, trading, to } = await ready();
    const main = S.activeWallet.value!;
    await S.startSend({ to, amount: '1' });
    expect(await S.reviewSend()).toBe(true);
    const txid = S.sendFlow.value.prepared!.txid;
    const hold = gate();
    const original = backend.broadcast.bind(backend);
    vi.spyOn(backend, 'broadcast').mockImplementation(async (id, tx) => {
      await hold.wait;
      return original(id, tx);
    });
    const sending = S.confirmSend();
    await S.selectWallet(trading.id);
    await settle();
    S.updateDraft({ to, amount: '2' });
    hold.open();
    await sending;
    expect(S.activeWallet.value?.id).toBe(trading.id);
    expect(S.sendFlow.value.step).toBe('edit');
    expect(S.sendDraft.value.amount).toBe('2');
    // Main still tracks its transfer; the user is told it was sent.
    expect(session.pendingTxs.value.some((p) => p.txid === txid && p.walletId === main.id)).toBe(true);
    expect(S.walletPending.value).toEqual([]);
    expect(S.toasts.value.some((t) => t.message === 'Transfer sent')).toBe(true);
    expect(backend.preparedCount()).toBe(0);
  });

  it('a higher priority the balance cannot pay keeps the review and says why', async () => {
    const { S, backend, to } = await ready();
    await S.startSend({ to });
    await S.fillMaxAmount();
    expect(await S.reviewSend()).toBe(true);
    const first = S.sendFlow.value.prepared!;
    await S.setSendFeeLevel(10);
    const flow = S.sendFlow.value;
    expect(flow).toMatchObject({ step: 'review', feeLevel: 1, busy: false });
    expect(flow.prepared?.txid).toBe(first.txid);
    expect(flow.error).toMatchObject({ field: 'fee', message: 'Not enough MMX for this priority.' });
    expect(backend.preparedCount()).toBe(1);
    // A priority that fits clears the message.
    await S.backToEdit();
    S.updateDraft({ amount: '1' });
    await S.reviewSend();
    await S.setSendFeeLevel(2);
    expect(S.sendFlow.value).toMatchObject({ step: 'review', feeLevel: 2 });
    expect(S.sendFlow.value.error).toBeUndefined();
    expect(backend.preparedCount()).toBe(1);
  });

  it('expires the kept review if its time ran out during a refused priority change', async () => {
    const { S, backend, types, to } = await ready();
    await S.startSend({ to, amount: '1' });
    await S.reviewSend();
    const hold = gate();
    vi.spyOn(backend, 'prepareSend').mockImplementation(async () => {
      await hold.wait;
      throw new types.BackendError('insufficient_funds');
    });
    const changing = S.setSendFeeLevel(10);
    await settle(S.PREPARED_TTL_MS + 1000);
    hold.open();
    await changing;
    expect(S.sendFlow.value.step).toBe('edit');
    expect(S.sendFlow.value.error?.code).toBe('prepared_not_found');
  });

  it('available amount and checks leave out funds of pending sends', async () => {
    const { S, to } = await ready();
    const total = S.nativeAsset.value!.total;
    await S.startSend({ to });
    await S.fillMaxAmount();
    const max = S.sendDraft.value.amount;
    expect(await S.reviewSend()).toBe(true);
    await S.confirmSend();
    expect(S.sendFlow.value.step).toBe('done');
    S.finishSend();
    const asset = S.nativeAsset.value!;
    const reserved = S.pendingReservedTotals.value.get(NATIVE_CONTRACT) ?? 0n;
    expect(reserved).toBeGreaterThan(0n);
    expect(S.spendableBalance(asset, S.pendingReservedTotals.value)).toBe(total - reserved > 0n ? total - reserved : 0n);
    // The same amount again is more than what is left.
    S.updateDraft({ to, amount: max });
    expect(S.draftCheck.value.errors.amount).toBe('This is more than your balance.');
  });

  it('checkDraft compares with the balance minus pending amounts', async () => {
    const { S, to } = await ready();
    const asset = S.nativeAsset.value!;
    const draft = { to, contract: NATIVE_CONTRACT, amount: '1', memo: '' };
    const pending = new Map([[NATIVE_CONTRACT, asset.total - 500_000n]]);
    expect(S.checkDraft(draft, [asset], [], new Set(), pending).errors.amount).toBe('This is more than your balance.');
    expect(S.checkDraft({ ...draft, amount: '0.5' }, [asset], [], new Set(), pending).ready).toBe(true);
    expect(S.spendableBalance(asset, new Map([[NATIVE_CONTRACT, asset.total + 1n]]))).toBe(0n);
    expect(S.checkDraft({ ...draft, memo: 'x'.repeat(65) }, [asset], [], new Set()).errors.memo).toBe('The memo is too long.');
  });
});

describe('locking', () => {
  it('only an explicit lock reaches other tabs', async () => {
    const { S, M, backend } = await boot('web');
    const main = S.wallets.value[0]!;
    const spy = vi.spyOn(backend, 'lockAll');
    await S.unlockWallet(main.id, { mnemonic: M.MOCK_WALLETS[0].mnemonic });
    await settle();
    await S.lock();
    expect(spy).toHaveBeenLastCalledWith({ everywhere: false });
    await S.unlockWallet(main.id, { mnemonic: M.MOCK_WALLETS[0].mnemonic });
    await settle();
    await S.lock({ everywhere: true });
    expect(spy).toHaveBeenLastCalledWith({ everywhere: true });
    expect(S.phase.value).toBe('unlock');
  });

  it('a lock from another tab closes the wallet here without locking again', async () => {
    let external: (() => void) | null = null;
    const { S, M, backend } = await boot('web', (b) => {
      Object.assign(b, {
        onExternalLock(listener: () => void) {
          external = listener;
          return () => undefined;
        },
      } satisfies Pick<Backend, 'onExternalLock'>);
    });
    const main = S.wallets.value[0]!;
    await S.unlockWallet(main.id, { mnemonic: M.MOCK_WALLETS[0].mnemonic });
    await settle();
    expect(S.phase.value).toBe('ready');
    await S.startSend({ to: S.addresses.value[1]!, amount: '1' });
    const spy = vi.spyOn(backend, 'lockAll');
    // The backend frees its keys, then tells the app.
    await backend.lockWallet(main.id);
    expect(external).not.toBeNull();
    external!();
    await settle();
    expect(spy).not.toHaveBeenCalled();
    expect(S.phase.value).toBe('unlock');
    expect(S.route.value.name).toBe('unlock');
    expect(S.addresses.value).toEqual([]);
    expect(S.sendDraft.value.to).toBe('');
  });

  it('a lock from another tab leaves a tab that had nothing open where it is', async () => {
    const { S } = await boot('web');
    S.navigate('import');
    await settle(10);
    await S.lockedElsewhere();
    expect(S.route.value.name).toBe('import');
  });
});

describe('import with a passphrase', () => {
  it('warns when the words and passphrase open an unused wallet', async () => {
    const { S } = await boot();
    const words = Array.from({ length: 24 }, (_, i) => ['zoo', 'abandon', 'ability'][i % 3]).join(' ');
    const done = S.importWallet({ name: 'Typo', mnemonic: words, passphrase: 'correct hors' });
    await settle(30_000);
    await done;
    expect(S.toasts.value.some((t) => t.kind === 'warning' && t.message.startsWith('No activity found'))).toBe(true);
  });

  it('does not warn without a passphrase', async () => {
    const { S } = await boot();
    const words = Array.from({ length: 24 }, (_, i) => ['zoo', 'abandon', 'ability'][i % 3]).join(' ');
    const done = S.importWallet({ name: 'Plain', mnemonic: words });
    await settle(30_000);
    await done;
    expect(S.toasts.value.some((t) => t.message.startsWith('No activity found'))).toBe(false);
  });
});

describe('copying recovery words', () => {
  it('promises a clipboard time only on the desktop', async () => {
    const desktop = await boot('desktop');
    expect(await desktop.S.copyToClipboard('words', { sensitive: true })).toBe(true);
    expect(desktop.S.toasts.value.at(-1)?.message).toBe('Copied. The clipboard clears in 1 minute.');
    const web = await boot('web');
    expect(await web.S.copyToClipboard('words', { sensitive: true })).toBe(true);
    expect(web.S.toasts.value.at(-1)?.message).toBe('Copied. Clear your clipboard after pasting.');
  });
});

describe('passphrase fields', () => {
  const screens = join(__dirname, '../../src/ui/screens');
  const files = [
    'create/WalletDetails.tsx',
    'unlock/forms.tsx',
    'settings/WalletSettings.tsx',
  ];

  it('are never offered to password managers as passwords', async () => {
    const { PASSPHRASE_FIELD } = await import('../../src/ui/screens/welcome/flow');
    expect(PASSPHRASE_FIELD.autocomplete).toBe('one-time-code');
    let count = 0;
    for (const file of files) {
      const source = readFileSync(join(screens, file), 'utf8');
      const fields = (source.match(/<PasswordInput\b[\s\S]*?\/>/gu) ?? []).filter((f) => f.includes('name="wallet-passphrase'));
      expect(fields.length, file).toBeGreaterThan(0);
      for (const field of fields) {
        expect(field, file).toContain('{...PASSPHRASE_FIELD}');
        expect(field, file).not.toMatch(/autocomplete=|current-password|new-password/u);
      }
      count += fields.length;
    }
    // Details (passphrase + repeat), unlock passphrase step, desktop unlock, reveal prompt.
    // Browser tests check the shared AuthorizationDialog in each credential mode.
    expect(count).toBe(5);
  });
});
