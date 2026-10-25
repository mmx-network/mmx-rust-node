// Desktop backend (src/backend/desktop.ts) against a fake Tauri IPC. The
// payloads it sends must equal src-tauri/tests/payloads.json, which the
// desktop app's own tests decode with its strict argument types.
import { beforeEach, describe, expect, it, vi } from 'vitest';
import payloads from '../../src-tauri/tests/payloads.json';
import type { AppState, Backend } from '../../src/backend/types';

const ipc = vi.hoisted(() => ({ invoke: vi.fn(), tauri: true }));
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (command: string, args?: unknown) => ipc.invoke(command, args),
  isTauri: () => ipc.tauri,
}));

const { createBackend } = await import('../../src/backend/desktop');

type Command = keyof typeof payloads;
const W = 'wallet_3291348052.dat';
const ZERO = 'mmx1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqdgytev';
const TO = 'mmx1mktnzjz82tzre764afvttx0rj2886g24gnmuzxntcvnzdnm9kkvsqwz36c';
const TXID = 'AB01'.repeat(16);
const WORDS = payloads.import_wallet.mnemonic;

let backend: Backend;

beforeEach(async () => {
  ipc.invoke.mockReset();
  ipc.invoke.mockResolvedValue(null);
  ipc.tauri = true;
  backend = await createBackend();
});

function sent(): { command: string; args: unknown } {
  expect(ipc.invoke).toHaveBeenCalledTimes(1);
  const [command, args] = ipc.invoke.mock.calls[0] as [string, unknown];
  return { command, args: args ?? {} };
}

const state: AppState = {
  activeWallet: W,
  labels: { [W]: { 0: 'Rent', 3: 'Savings' } },
  contacts: payloads.save_app_state.state.contacts,
  settings: { theme: 'dark', autoLockMinutes: 30, feeLevel: 2, rpcUrl: 'https://rpc.mmx.network' },
};

const calls: Record<Command, (b: Backend) => Promise<unknown>> = {
  load_app_state: (b) => b.loadAppState(),
  save_app_state: (b) => b.saveAppState(state),
  list_wallets: (b) => b.listWallets(),
  new_recovery_phrase: (b) => b.newRecoveryPhrase(),
  create_wallet: (b) => b.createWallet({ name: 'Main', mnemonic: WORDS, passphrase: 'correct horse', addressCount: 10 }),
  import_wallet: (b) => b.importWallet({ name: 'Main', mnemonic: WORDS, passphrase: 'correct horse', addressCount: 100 }),
  unlock_wallet: (b) => b.unlockWallet(W, { passphrase: 'correct horse' }),
  lock_wallet: (b) => b.lockWallet(W),
  lock_all: (b) => b.lockAll(),
  rename_wallet: (b) => b.renameWallet(W, 'Savings'),
  set_address_count: (b) => b.setAddressCount(W, 12),
  remove_wallet: (b) => b.removeWallet(W),
  reveal_mnemonic: (b) => b.revealMnemonic(W),
  get_addresses: (b) => b.getAddresses(W),
  set_active_wallet: (b) => b.setActiveWallet(W),
  rpc_get: (b) => b.rpcGet(`/address?id=${TO}&limit=1000`),
  max_sendable: (b) => b.maxSendable({ walletId: W, to: '', contract: ZERO, memo: 'rent', feeLevel: 2 }),
  prepare_send: (b) => b.prepareSend({ walletId: W, to: TO, contract: ZERO, amount: '1500000', memo: 'rent', feeLevel: 2 }),
  broadcast: (b) => b.broadcast(W, TXID),
  discard_prepared: (b) => b.discardPrepared(TXID),
  copy_text: (b) => b.copyText(WORDS, true),
  open_external: (b) => b.openExternal(`https://explore.mmx.network/#/explore/transaction/${TXID}`),
  export_file: (b) => b.exportFile('mmx-contacts.json', '{"contacts":[]}\n'),
  import_file: (b) => b.importFile(),
};

describe('desktop backend', () => {
  it('reports the desktop platform', () => {
    expect(backend.platform).toBe('desktop');
  });

  for (const command of Object.keys(payloads) as Command[]) {
    it(`sends ${command} with exactly the expected fields`, async () => {
      // Only the request matters here; the fake app answers null to everything.
      await calls[command](backend).catch(() => undefined);
      expect(sent()).toEqual({ command, args: payloads[command] });
    });
  }

  it('leaves out optional fields that are not set', async () => {
    await backend.createWallet({ name: 'Main', mnemonic: WORDS, addressCount: 10 });
    expect(sent().args).toEqual({ name: 'Main', mnemonic: WORDS, addressCount: 10 });
    ipc.invoke.mockClear();
    await backend.unlockWallet(W, {});
    expect(sent().args).toEqual({ id: W });
    ipc.invoke.mockClear();
    await backend.prepareSend({ walletId: W, to: TO, contract: ZERO, amount: '1', feeLevel: 1 });
    expect(sent().args).toEqual({ walletId: W, to: TO, contract: ZERO, amount: '1', feeLevel: 1 });
    ipc.invoke.mockClear();
    await backend.copyText('mmx1');
    expect(sent().args).toEqual({ text: 'mmx1', sensitive: false });
  });

  it('sends only the stored fields of the app state', async () => {
    const extra = {
      ...state,
      unknown: 1,
      contacts: [{ ...state.contacts[0]!, extra: true }],
      settings: { ...state.settings, rpcUrl: undefined, extra: 'x' },
    } as unknown as AppState;
    await backend.saveAppState(extra);
    expect(sent().args).toEqual({
      state: {
        activeWallet: W,
        labels: state.labels,
        contacts: [payloads.save_app_state.state.contacts[0]],
        settings: { theme: 'dark', autoLockMinutes: 30, feeLevel: 2 },
      },
    });
  });

  it('refuses web-only requests without calling the app', async () => {
    await expect(backend.unlockWallet(W, { mnemonic: WORDS })).rejects.toMatchObject({ code: 'invalid_argument' });
    await expect(
      backend.importWallet({ name: 'Main', mnemonic: WORDS, addressCount: 10, expectedFingerprint: '1' }),
    ).rejects.toMatchObject({ code: 'invalid_argument' });
    await expect(backend.openExternal('https://example.com/')).rejects.toMatchObject({ code: 'invalid_argument' });
    expect(ipc.invoke).not.toHaveBeenCalled();
  });

  it('maps app errors to backend errors without carrying unknown messages', async () => {
    ipc.invoke.mockRejectedValueOnce({ code: 'locked', message: 'wallet is locked' });
    await expect(backend.getAddresses(W)).rejects.toMatchObject({ name: 'BackendError', code: 'locked' });
    ipc.invoke.mockRejectedValueOnce('Command get_addresses not allowed by ACL');
    const error = await backend.getAddresses(W).catch((e: unknown) => e);
    expect(error).toMatchObject({ name: 'BackendError', code: 'internal' });
    expect(String((error as Error).message)).not.toContain('ACL');
    ipc.invoke.mockRejectedValueOnce({ code: 'not_a_code', message: 'secret words' });
    await expect(backend.listWallets()).rejects.toMatchObject({ code: 'internal', message: 'internal' });
  });

  it('checks the stored app state it reads', async () => {
    ipc.invoke.mockResolvedValueOnce({ labels: 5, contacts: [{ name: 'x' }], settings: { theme: 'neon', feeLevel: 7 } });
    expect(await backend.loadAppState()).toEqual({
      labels: {},
      contacts: [],
      settings: { theme: 'system', autoLockMinutes: 24 * 60, feeLevel: 1 },
    });
  });

  it('returns new recovery words only as text', async () => {
    ipc.invoke.mockResolvedValueOnce(WORDS);
    expect(await backend.newRecoveryPhrase()).toBe(WORDS);
    ipc.invoke.mockResolvedValueOnce({ words: WORDS });
    await expect(backend.newRecoveryPhrase()).rejects.toMatchObject({ code: 'internal' });
  });

  it('locks its single window only', async () => {
    await backend.lockAll({ everywhere: true });
    expect(sent()).toEqual({ command: 'lock_all', args: {} });
  });

  it('is unavailable outside the desktop app', async () => {
    ipc.tauri = false;
    await expect(createBackend()).rejects.toMatchObject({ code: 'internal' });
  });
});
