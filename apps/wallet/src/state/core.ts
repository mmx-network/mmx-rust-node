/** Backend handle, wallets, persisted app state and the app phase. */
import { computed, signal } from '@preact/signals';
import { BackendError, toBackendError, type AppState, type Backend, type Platform, type WalletId, type WalletInfo } from '../backend/types';
import { defaultAppState, sanitizeAppState } from '../lib/appstate';
import { sanitizeText } from '../lib/text';
import type { AppPhase } from './router';

let backendRef: Backend | null = null;

export function backend(): Backend {
  if (!backendRef) throw new BackendError('internal', 'backend not initialized');
  return backendRef;
}

export function setBackend(b: Backend | null): void {
  backendRef = b;
}

export const platform = signal<Platform>('desktop');
/** Initialization finished (successfully or not). */
export const initialized = signal(false);
/** Set when the app could not start; a user-facing message. */
export const initError = signal<string | null>(null);

export const wallets = signal<readonly WalletInfo[]>([]);
export const appState = signal<AppState>(defaultAppState());
export const activeWalletId = signal<WalletId | undefined>(undefined);
/** Wallet whose data session is open (addresses loaded). */
export const sessionWalletId = signal<WalletId | null>(null);
/** A wallet is being opened (switching wallets). */
export const opening = signal(false);

export const activeWallet = computed(() => wallets.value.find((w) => w.id === activeWalletId.value));
export const usableWallets = computed(() => wallets.value.filter((w) => !w.error));
export const settings = computed(() => appState.value.settings);
export const contacts = computed(() => appState.value.contacts);
/** Address labels of the active wallet by index. */
export const activeLabels = computed<Readonly<Record<number, string>>>(() => {
  const id = activeWalletId.value;
  return (id && appState.value.labels[id]) || {};
});

export const phase = computed<AppPhase>(() => {
  if (!initialized.value) return 'loading';
  if (usableWallets.value.length === 0) return 'onboarding';
  const active = activeWallet.value;
  if (!active || active.error || active.locked || sessionWalletId.value !== active.id) return 'unlock';
  return 'ready';
});

/**
 * Whether "Lock" is offered: web always (frees the in-memory keys); desktop
 * only for an unlocked wallet protected by a passphrase.
 */
export const canLock = computed(() => {
  const w = activeWallet.value;
  if (!w || w.locked || w.error) return false;
  return platform.value === 'web' || w.withPassphrase;
});

/** Wallet names may come from hand-edited files: display them safely. */
export function cleanWallet(info: WalletInfo): WalletInfo {
  const name = sanitizeText(info.name, 64).trim() || 'Wallet';
  return name === info.name ? info : { ...info, name };
}

export function setWallets(list: readonly WalletInfo[]): void {
  wallets.value = list.map(cleanWallet);
}

export async function refreshWallets(): Promise<readonly WalletInfo[]> {
  setWallets(await backend().listWallets());
  return wallets.value;
}

/** Replaces one wallet entry in the list (after unlock etc.). */
export function upsertWallet(raw: WalletInfo): void {
  const info = cleanWallet(raw);
  const list = wallets.value.slice();
  const i = list.findIndex((w) => w.id === info.id);
  if (i >= 0) list[i] = info;
  else list.push(info);
  wallets.value = list;
}

let saveQueue: Promise<unknown> = Promise.resolve();

/**
 * Updates the app state in memory and persists it. Saves run in order and
 * always write the latest state. On failure the persisted state is reloaded
 * and the error is rethrown.
 */
export async function persistAppState(update: (current: AppState) => AppState): Promise<void> {
  const next = update(appState.value);
  appState.value = next;
  const run = saveQueue.then(() => backend().saveAppState(appState.value));
  saveQueue = run.catch(() => undefined);
  try {
    await run;
  } catch (e) {
    await reloadAppState().catch(() => undefined);
    throw toBackendError(e);
  }
}

/**
 * Like persistAppState, but the in-memory state changes only after the
 * backend accepted the change (e.g. a new network server it checks first).
 * Saves made meanwhile keep the previous value; on failure nothing changes.
 */
export async function commitAppState(update: (current: AppState) => AppState): Promise<void> {
  const run = saveQueue.then(async () => {
    await backend().saveAppState(update(appState.value));
    // Re-applied to the newest state: edits made while saving are kept.
    appState.value = update(appState.value);
  });
  saveQueue = run.catch(() => undefined);
  try {
    await run;
  } catch (e) {
    throw toBackendError(e);
  }
}

export async function reloadAppState(): Promise<void> {
  const b = backend();
  const raw = await b.loadAppState();
  const state = sanitizeAppState(raw, b.platform);
  // The active wallet is owned by this window; keep it.
  if (activeWalletId.value) state.activeWallet = activeWalletId.value;
  appState.value = state;
}
