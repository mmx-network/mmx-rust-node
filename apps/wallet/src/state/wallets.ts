/** Wallet lifecycle: create, import, unlock, lock, switch, rename, addresses, remove. */
import { batch, signal } from '@preact/signals';
import { BackendError, isBackendError, type WalletId, type WalletInfo } from '../backend/types';
import { discoveredCount, findLastUsedIndex } from '../chain/discovery';
import { DEFAULT_ADDRESS_COUNT, MAX_ADDRESSES, WALLET_NAME_MAX } from '../lib/appstate';
import { parseCredentialUsername } from '../backend/web/credentials';
import { joinWords, splitWords } from '../lib/mnemonic';
import { validateName } from '../lib/text';
import {
  activeWallet,
  activeWalletId,
  appState,
  backend,
  opening,
  persistAppState,
  platform,
  refreshWallets,
  upsertWallet,
  usableWallets,
  wallets,
} from './core';
import { isShellRoute, navigate, route, type RouteId } from './router';
import { discardSend } from './send';
import { addressRows, applyAddresses, closeSession, ensureParams, forgetWalletData, openSession, rpc } from './session';
import { clearToasts, notify } from './toasts';

function invalidName(name: string): void {
  if (validateName(name, WALLET_NAME_MAX)) throw new BackendError('invalid_argument', 'invalid wallet name');
}

/** Empty input means no passphrase. */
function optionalPassphrase(passphrase: string | undefined): string | undefined {
  return passphrase ? passphrase : undefined;
}

/** "Wallet", "Wallet 2", … — the first name not in use. */
export function suggestWalletName(base = 'Wallet'): string {
  const names = new Set(wallets.value.map((w) => w.name.trim().toLowerCase()));
  if (!names.has(base.toLowerCase())) return base;
  for (let i = 2; ; i++) {
    const name = `${base} ${i}`;
    if (!names.has(name.toLowerCase())) return name;
  }
}

/**
 * Makes a wallet active. An unlocked wallet's session opens before anything
 * switches, so the app stays usable; a locked wallet leads to #/unlock.
 */
export async function activateWallet(id: WalletId, options: { navigateTo?: RouteId | null } = {}): Promise<void> {
  const target = wallets.value.find((w) => w.id === id);
  if (!target || target.error) throw new BackendError('wallet_not_found');
  await discardSend();
  if (target.locked) {
    closeSession();
    batch(() => {
      activeWalletId.value = id;
      appState.value = { ...appState.value, activeWallet: id };
    });
    void backend().setActiveWallet(id).catch(() => undefined);
    navigate('unlock');
    return;
  }
  opening.value = true;
  let opened: boolean;
  try {
    opened = await openSession(id);
  } finally {
    opening.value = false;
  }
  if (!opened) return;
  appState.value = { ...appState.value, activeWallet: id };
  void backend().setActiveWallet(id).catch(() => undefined);
  if (options.navigateTo !== null) navigate(options.navigateTo ?? 'overview');
}

/** Switches to another wallet (shell wallet switcher, unlock screen list). Stays on the current page when possible. */
export async function selectWallet(id: WalletId): Promise<void> {
  if (id === activeWalletId.value && activeWallet.value && !activeWallet.value.locked) return;
  const current = route.value.name;
  const navigateTo: RouteId = isShellRoute(current) ? current : 'overview';
  try {
    await activateWallet(id, { navigateTo });
  } catch (e) {
    if (isBackendError(e, 'locked')) {
      await refreshWallets();
      await activateWallet(id);
      return;
    }
    throw e;
  }
}

/**
 * Unlocks a wallet with its passphrase (desktop) or recovery words from the
 * password manager (web) and opens it. Throws BackendError
 * (invalid_passphrase, passphrase_required, wallet_mismatch, invalid_mnemonic).
 */
export async function unlockWallet(
  id: WalletId,
  request: { passphrase?: string; mnemonic?: string },
  options: { navigateTo?: RouteId | null } = {},
): Promise<WalletInfo> {
  const req: { passphrase?: string; mnemonic?: string } = {};
  if (request.passphrase !== undefined && request.passphrase !== '') req.passphrase = request.passphrase;
  if (request.mnemonic !== undefined) req.mnemonic = joinWords(splitWords(request.mnemonic));
  const info = await backend().unlockWallet(id, req);
  upsertWallet(info);
  await activateWallet(info.id, options);
  return info;
}

/** Verifies a passphrase without changing what is open (desktop). */
export async function verifyPassphrase(id: WalletId, passphrase: string): Promise<void> {
  const info = await backend().unlockWallet(id, { passphrase });
  upsertWallet(info);
}

/**
 * Locks: web frees every wallet's keys; desktop forgets the passphrases of
 * passphrase-protected wallets. If the active wallet is now locked the app
 * shows the unlock screen.
 */
export async function lock(): Promise<void> {
  await discardSend();
  try {
    await backend().lockAll();
  } finally {
    const before = new Set(wallets.value.filter((w) => !w.locked).map((w) => w.id));
    const list = await refreshWallets().catch(() => wallets.value);
    const nowLocked = new Set([...before].filter((id) => list.find((w) => w.id === id)?.locked !== false));
    const active = list.find((w) => w.id === activeWalletId.value);
    if (!active || active.locked) {
      closeSession();
      forgetWalletData(platform.value === 'web' ? 'all' : nowLocked);
      clearToasts();
      navigate('unlock', undefined, { replace: true });
    } else if (nowLocked.size > 0) {
      forgetWalletData(nowLocked);
    }
  }
}

/** Creates a wallet. The caller shows the words and confirms them, then calls completeCreate. */
export async function createWallet(request: { name: string; passphrase?: string }): Promise<{ wallet: WalletInfo; mnemonic: string }> {
  invalidName(request.name);
  const req: { name: string; passphrase?: string; addressCount: number } = {
    name: request.name.trim(),
    addressCount: DEFAULT_ADDRESS_COUNT,
  };
  const passphrase = optionalPassphrase(request.passphrase);
  if (passphrase) req.passphrase = passphrase;
  const result = await backend().createWallet(req);
  await refreshWallets().catch(() => upsertWallet(result.wallet));
  return result;
}

/** Opens a newly created wallet after its recovery words were confirmed. */
export async function completeCreate(id: WalletId): Promise<void> {
  const list = await refreshWallets();
  const w = list.find((x) => x.id === id);
  if (!w) throw new BackendError('wallet_not_found');
  await activateWallet(id, { navigateTo: 'overview' });
}

/** Progress of the used-address scan after an import; null when idle. */
export const discovery = signal<{ checked: number; total: number } | null>(null);

/**
 * Imports a wallet from 24 words, finds used addresses (up to 100) and sets
 * the address count to max(10, last used + 1), then opens it.
 */
export async function importWallet(
  request: {
    name: string;
    mnemonic: string;
    passphrase?: string;
    /** Web: fingerprint from a saved credential's username. */
    expectedFingerprint?: string;
  },
  /** `navigateTo: null` opens the wallet but stays on the current page (web import save step). */
  options: { navigateTo?: RouteId | null } = {},
): Promise<WalletInfo> {
  invalidName(request.name);
  const mnemonic = joinWords(splitWords(request.mnemonic));
  const req: { name: string; mnemonic: string; passphrase?: string; addressCount: number; expectedFingerprint?: string } = {
    name: request.name.trim(),
    mnemonic,
    addressCount: MAX_ADDRESSES,
  };
  const passphrase = optionalPassphrase(request.passphrase);
  if (passphrase) req.passphrase = passphrase;
  if (request.expectedFingerprint) req.expectedFingerprint = request.expectedFingerprint;
  const info = await backend().importWallet(req);
  upsertWallet(info);

  let count = DEFAULT_ADDRESS_COUNT;
  discovery.value = { checked: 0, total: MAX_ADDRESSES };
  try {
    const list = await backend().getAddresses(info.id);
    discovery.value = { checked: 0, total: list.length };
    const params = await ensureParams();
    await rpc().height(params);
    const last = await findLastUsedIndex(rpc(), list, {
      concurrency: 4,
      onProgress: (checked, total) => {
        discovery.value = { checked, total };
      },
    });
    count = discoveredCount(last, DEFAULT_ADDRESS_COUNT, MAX_ADDRESSES);
  } catch {
    notify('Could not check for used addresses. You can show more addresses in Settings.', 'warning', 9000);
  } finally {
    discovery.value = null;
  }
  if (count !== info.addressCount) await backend().setAddressCount(info.id, count);
  const list = await refreshWallets();
  const updated = list.find((w) => w.id === info.id) ?? info;
  await activateWallet(updated.id, { navigateTo: options.navigateTo === undefined ? 'overview' : options.navigateTo });
  return updated;
}

/**
 * Web: opens a wallet from a saved credential (username + recovery words).
 * A wallet known on this device is unlocked; otherwise it is added, using the
 * fingerprint in the username for passphrase detection. Throws
 * passphrase_required (ask for the passphrase and call again),
 * invalid_passphrase, wallet_mismatch or invalid_mnemonic.
 */
export async function openSavedWallet(request: { username: string; words: string; passphrase?: string }): Promise<WalletInfo> {
  const { name, fingerprint } = parseCredentialUsername(request.username);
  const words = joinWords(splitWords(request.words));
  const unlockReq: { mnemonic: string; passphrase?: string } = { mnemonic: words };
  const passphrase = optionalPassphrase(request.passphrase);
  if (passphrase) unlockReq.passphrase = passphrase;
  if (fingerprint) {
    const known = wallets.value.find((w) => w.fingerprint === fingerprint && !w.error);
    if (known) return unlockWallet(known.id, unlockReq);
  }
  try {
    const importReq: { name: string; mnemonic: string; passphrase?: string; expectedFingerprint?: string } = {
      name: name.slice(0, WALLET_NAME_MAX),
      mnemonic: words,
    };
    if (passphrase) importReq.passphrase = passphrase;
    if (fingerprint) importReq.expectedFingerprint = fingerprint;
    return await importWallet(importReq);
  } catch (e) {
    if (!isBackendError(e, 'wallet_exists')) throw e;
    // Known on this device but the username has no wallet ID: find it by opening each locked wallet.
    let needsPassphrase = false;
    let wrongPassphrase = false;
    for (const w of wallets.value) {
      if (!w.locked || w.error) continue;
      try {
        return await unlockWallet(w.id, unlockReq);
      } catch (inner) {
        if (isBackendError(inner, 'passphrase_required')) needsPassphrase = true;
        else if (isBackendError(inner, 'invalid_passphrase')) wrongPassphrase = true;
        else if (!isBackendError(inner, 'wallet_mismatch')) throw inner;
      }
    }
    if (needsPassphrase) throw new BackendError('passphrase_required');
    if (wrongPassphrase) throw new BackendError('invalid_passphrase');
    throw e;
  }
}

export async function renameWallet(id: WalletId, name: string): Promise<void> {
  invalidName(name);
  await backend().renameWallet(id, name.trim());
  await refreshWallets();
}

/** Indices at or above `count` that hold funds (they would be hidden). */
export function fundedAddressesFrom(count: number): number[] {
  return addressRows.value.filter((r) => r.index >= count && r.hasFunds).map((r) => r.index);
}

/** Changes the active wallet's number of addresses (1–100). */
export async function setAddressCount(count: number): Promise<void> {
  const id = activeWalletId.value;
  if (!id) throw new BackendError('wallet_not_found');
  if (!Number.isInteger(count) || count < 1 || count > MAX_ADDRESSES) throw new BackendError('invalid_argument');
  const list = await backend().setAddressCount(id, count);
  await refreshWallets().catch(() => undefined);
  await applyAddresses(list);
}

/** Removes a wallet from this device (desktop keeps a backup of the key file). */
export async function removeWallet(id: WalletId): Promise<void> {
  await backend().removeWallet(id);
  if (appState.value.labels[id]) {
    await persistAppState((s) => {
      const labels = { ...s.labels };
      delete labels[id];
      return { ...s, labels };
    }).catch(() => undefined);
  }
  forgetWalletData(new Set([id]));
  await refreshWallets();
  if (activeWalletId.value !== id) return;
  await discardSend();
  closeSession();
  const next = usableWallets.value[0];
  if (next) {
    await activateWallet(next.id, { navigateTo: 'overview' });
  } else {
    batch(() => {
      activeWalletId.value = undefined;
      appState.value = { ...appState.value, activeWallet: undefined };
    });
    navigate('welcome', undefined, { replace: true });
  }
}

/** The wallet's recovery words. Callers keep them only while shown. */
export async function revealMnemonic(id: WalletId): Promise<string> {
  return backend().revealMnemonic(id);
}
