/** Appearance, auto-lock, default fee and (desktop) network server. */
import { BackendError, type FeeLevel, type ThemeSetting } from '../backend/types';
import { AUTO_LOCK_CHOICES } from '../lib/appstate';
import { isFeeLevel } from '../lib/fees';
import { commitAppState, persistAppState, platform, sessionWalletId } from './core';
import { openSession, resetChain } from './session';

export const DEFAULT_RPC_URL = 'https://rpc.mmx.network';

export async function setTheme(theme: ThemeSetting): Promise<void> {
  if (theme !== 'system' && theme !== 'light' && theme !== 'dark') throw new BackendError('invalid_argument');
  await persistAppState((s) => ({ ...s, settings: { ...s.settings, theme } }));
}

/** Auto-lock delay in minutes: 5/15/30/60, desktop also 0 (never). */
export async function setAutoLockMinutes(minutes: number): Promise<void> {
  if (!AUTO_LOCK_CHOICES[platform.value].includes(minutes)) throw new BackendError('invalid_argument');
  await persistAppState((s) => ({ ...s, settings: { ...s.settings, autoLockMinutes: minutes } }));
}

export async function setDefaultFeeLevel(level: FeeLevel): Promise<void> {
  if (!isFeeLevel(level)) throw new BackendError('invalid_argument');
  await persistAppState((s) => ({ ...s, settings: { ...s.settings, feeLevel: level } }));
}

/** Normalizes user input to an http(s) URL, or null if it is not one. */
export function normalizeRpcUrl(input: string): string | null {
  let text = input.trim().replace(/\/+$/, '');
  if (!text) return null;
  if (!/^[a-z][a-z0-9+.-]*:\/\//i.test(text)) text = `https://${text}`;
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    return null;
  }
  if ((url.protocol !== 'http:' && url.protocol !== 'https:') || !url.hostname || url.username || url.password) return null;
  if (url.search || url.hash) return null;
  return text;
}

/**
 * Desktop: switches the network server. The backend checks that it responds
 * before saving (errors: rpc_unreachable, rpc_timeout, rpc_http_error,
 * rpc_not_synced, rpc_network_mismatch, invalid_argument); the settings change
 * only once it was saved. Pass null for the default server. The open wallet
 * reloads its data in place, so the current page stays.
 */
export async function setRpcUrl(input: string | null): Promise<void> {
  if (platform.value !== 'desktop') throw new BackendError('invalid_argument');
  let url: string | undefined;
  if (input !== null) {
    const normalized = normalizeRpcUrl(input);
    if (!normalized) throw new BackendError('invalid_argument');
    url = normalized === DEFAULT_RPC_URL ? undefined : normalized;
  }
  await commitAppState((s) => {
    const settings = { ...s.settings };
    if (url) settings.rpcUrl = url;
    else delete settings.rpcUrl;
    return { ...s, settings };
  });
  resetChain();
  // openSession replaces the session in one step: the wallet stays open meanwhile.
  const id = sessionWalletId.value;
  if (id) await openSession(id);
}
