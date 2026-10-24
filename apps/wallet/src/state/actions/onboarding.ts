/**
 * Onboarding helpers for the welcome, create, import and unlock screens:
 * password manager access (web), discarding an unfinished new wallet and
 * leaving the flows.
 */
import type { WalletId } from '../../backend/types';
import {
  canUseCredentialApi,
  preventSilentAccess,
  requestCredential,
  storeCredential,
  type StoredCredential,
} from '../../backend/web/credentials';
import { phase, platform, refreshWallets, wallets } from '../core';
import { navigate } from '../router';
import { notify } from '../toasts';
import { completeCreate, removeWallet } from '../wallets';

export type { StoredCredential };

/** True on the web when the browser can save and offer passwords from script (Chromium). */
export function passwordManagerApi(): boolean {
  return platform.value === 'web' && canUseCredentialApi();
}

/**
 * Web: asks the browser to save a wallet credential (username + recovery
 * words). Other browsers and password manager extensions pick it up from the
 * submitted form. Resolves false when not supported or declined.
 */
export async function saveWalletCredential(username: string, words: string): Promise<boolean> {
  if (platform.value !== 'web') return false;
  return storeCredential(username, words);
}

/**
 * Web: lets the user pick a saved wallet in the browser's own chooser
 * (Chromium). Silent sign-in is turned off first so a wallet never opens
 * without the user choosing it. Resolves null when unsupported or cancelled.
 */
export async function requestWalletCredential(): Promise<StoredCredential | null> {
  if (!passwordManagerApi()) return null;
  await preventSilentAccess();
  return requestCredential();
}

/**
 * Removes a wallet whose creation was abandoned before its recovery words were
 * confirmed (desktop keeps a backup of the key file). Never throws.
 */
export async function discardNewWallet(id: WalletId): Promise<void> {
  if (!wallets.value.some((w) => w.id === id)) return;
  try {
    await removeWallet(id);
  } catch {
    await refreshWallets().catch(() => undefined);
  }
}

/** Opens a newly created wallet after its recovery words were confirmed (desktop). */
export async function finishCreate(id: WalletId): Promise<void> {
  await completeCreate(id);
  notify('Wallet created', 'success');
}

/** Leaves a welcome/create/import flow: back into the app, to the unlock screen or to the start. */
export function leaveOnboarding(): void {
  const p = phase.value;
  if (p === 'ready') navigate('overview');
  else if (p === 'unlock') navigate('unlock');
  else navigate('welcome');
}

/** Whether the flows should offer a way back (wallets already exist on this device). */
export function hasWallets(): boolean {
  return wallets.value.some((w) => !w.error);
}
