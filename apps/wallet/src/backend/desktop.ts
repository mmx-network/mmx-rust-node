/**
 * Desktop backend: every operation is one command of the native app
 * (src-tauri), which implements it with the mmxwallet service. Commands take
 * one object with exactly the fields listed here and fail with
 * `{code, message}`; anything else becomes `internal`.
 */
import { invoke, isTauri } from '@tauri-apps/api/core';
import { sanitizeAppState } from '../lib/appstate';
import { isExplorerUrl } from '../lib/explorer';
import {
  BackendError,
  toBackendError,
  type AppState,
  type Backend,
  type CreateWalletRequest,
  type ImportWalletRequest,
  type PreparedSend,
  type SendRequest,
  type UnlockRequest,
  type WalletId,
  type WalletInfo,
} from './types';

/** Must match src-tauri/src/commands.rs. */
type Command =
  | 'load_app_state'
  | 'save_app_state'
  | 'list_wallets'
  | 'new_recovery_phrase'
  | 'create_wallet'
  | 'import_wallet'
  | 'unlock_wallet'
  | 'lock_wallet'
  | 'lock_all'
  | 'rename_wallet'
  | 'set_address_count'
  | 'remove_wallet'
  | 'reveal_mnemonic'
  | 'get_addresses'
  | 'set_active_wallet'
  | 'rpc_get'
  | 'max_sendable'
  | 'prepare_send'
  | 'broadcast'
  | 'discard_prepared'
  | 'copy_text'
  | 'open_external'
  | 'export_file'
  | 'import_file';

async function call<T>(command: Command, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (e) {
    throw toBackendError(e);
  }
}

/** The app state with only the fields the backend stores. */
function statePayload(s: AppState): Record<string, unknown> {
  const settings: Record<string, unknown> = {
    theme: s.settings.theme,
    autoLockMinutes: s.settings.autoLockMinutes,
    feeLevel: s.settings.feeLevel,
  };
  if (s.settings.rpcUrl !== undefined) settings.rpcUrl = s.settings.rpcUrl;
  const state: Record<string, unknown> = {
    labels: s.labels,
    contacts: s.contacts.map((c) =>
      c.note !== undefined ? { id: c.id, name: c.name, address: c.address, note: c.note } : { id: c.id, name: c.name, address: c.address },
    ),
    settings,
  };
  if (s.activeWallet !== undefined) state.activeWallet = s.activeWallet;
  return state;
}

function sendPayload(r: Omit<SendRequest, 'amount'> & { amount?: string }): Record<string, unknown> {
  const payload: Record<string, unknown> = { walletId: r.walletId, to: r.to, contract: r.contract, feeLevel: r.feeLevel };
  if (r.amount !== undefined) payload.amount = r.amount;
  if (r.memo !== undefined) payload.memo = r.memo;
  return payload;
}

class DesktopBackend implements Backend {
  readonly platform = 'desktop' as const;

  async loadAppState(): Promise<AppState> {
    return sanitizeAppState(await call<unknown>('load_app_state'), 'desktop');
  }

  saveAppState(s: AppState): Promise<void> {
    return call('save_app_state', { state: statePayload(s) });
  }

  listWallets(): Promise<WalletInfo[]> {
    return call('list_wallets');
  }

  async newRecoveryPhrase(): Promise<string> {
    const words = await call<unknown>('new_recovery_phrase');
    if (typeof words !== 'string') throw new BackendError('internal');
    return words;
  }

  createWallet(r: CreateWalletRequest): Promise<WalletInfo> {
    const args: Record<string, unknown> = { name: r.name, mnemonic: r.mnemonic, addressCount: r.addressCount };
    if (r.passphrase !== undefined) args.passphrase = r.passphrase;
    return call('create_wallet', args);
  }

  async importWallet(r: ImportWalletRequest): Promise<WalletInfo> {
    // Saved password manager wallets exist on the web only.
    if (r.expectedFingerprint !== undefined) throw new BackendError('invalid_argument');
    const args: Record<string, unknown> = { name: r.name, mnemonic: r.mnemonic, addressCount: r.addressCount };
    if (r.passphrase !== undefined) args.passphrase = r.passphrase;
    return call('import_wallet', args);
  }

  async unlockWallet(id: WalletId, r: UnlockRequest): Promise<WalletInfo> {
    if (r.mnemonic !== undefined) throw new BackendError('invalid_argument');
    const args: Record<string, unknown> = { id };
    if (r.passphrase !== undefined) args.passphrase = r.passphrase;
    return call('unlock_wallet', args);
  }

  lockWallet(id: WalletId): Promise<void> {
    return call('lock_wallet', { id });
  }

  /** The app runs as a single window: there are no other windows to lock. */
  lockAll(): Promise<void> {
    return call('lock_all');
  }

  renameWallet(id: WalletId, name: string): Promise<void> {
    return call('rename_wallet', { id, name });
  }

  setAddressCount(id: WalletId, n: number, passphrase?: string): Promise<string[]> {
    return call('set_address_count', { id, count: n, ...(passphrase !== undefined ? { passphrase } : {}) });
  }

  removeWallet(id: WalletId): Promise<void> {
    return call('remove_wallet', { id });
  }

  revealMnemonic(id: WalletId, passphrase?: string): Promise<string> {
    return call('reveal_mnemonic', { id, ...(passphrase !== undefined ? { passphrase } : {}) });
  }

  getAddresses(id: WalletId): Promise<string[]> {
    return call('get_addresses', { id });
  }

  setActiveWallet(id: WalletId): Promise<void> {
    return call('set_active_wallet', { id });
  }

  rpcGet(path: string): Promise<unknown> {
    return call('rpc_get', { path });
  }

  maxSendable(r: Omit<SendRequest, 'amount'>): Promise<string> {
    return call('max_sendable', sendPayload(r));
  }

  prepareSend(r: SendRequest): Promise<PreparedSend> {
    return call('prepare_send', sendPayload(r));
  }

  broadcast(walletId: WalletId, txid: string): Promise<void> {
    return call('broadcast', { walletId, txid });
  }

  discardPrepared(txid: string): Promise<void> {
    return call('discard_prepared', { txid });
  }

  copyText(text: string, sensitive?: boolean): Promise<void> {
    return call('copy_text', { text, sensitive: sensitive === true });
  }

  async openExternal(url: string): Promise<void> {
    if (!isExplorerUrl(url)) throw new BackendError('invalid_argument');
    return call('open_external', { url });
  }

  exportFile(suggestedName: string, contents: string): Promise<boolean> {
    return call('export_file', { suggestedName, contents });
  }

  importFile(): Promise<string | null> {
    return call('import_file');
  }
}

export async function createBackend(): Promise<Backend> {
  if (!isTauri()) throw new BackendError('internal');
  return new DesktopBackend();
}
