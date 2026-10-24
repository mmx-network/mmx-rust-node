/** App start-up and route guarding. */
import { effect } from '@preact/signals';
import { getBackend } from '../backend';
import { isBackendError, type Backend } from '../backend/types';
import { sanitizeAppState } from '../lib/appstate';
import { errorMessage } from '../lib/errors';
import {
  activeWalletId,
  appState,
  initError,
  initialized,
  phase,
  platform,
  reloadAppState,
  refreshWallets,
  setBackend,
  setWallets,
  usableWallets,
  activeWallet,
} from './core';
import { startIdleLock } from './idle';
import { homeRoute, navigate, route, routeAllowed, startRouter } from './router';
import { finishSend } from './send';
import { closeSession, openSession, resetChain, startSessionEffects } from './session';
import { activateWallet } from './wallets';

let started: Promise<void> | null = null;

/**
 * Loads the backend, persisted state and wallets, and opens the active wallet
 * if unlocked. Idempotent. `backend` overrides the platform backend (tests).
 */
export function init(options: { backend?: Backend } = {}): Promise<void> {
  if (!started) started = start(options.backend);
  return started;
}

async function start(override?: Backend): Promise<void> {
  try {
    const b = override ?? (await getBackend());
    setBackend(b);
    platform.value = b.platform;
    resetChain();
    const [raw, list] = await Promise.all([b.loadAppState(), b.listWallets()]);
    appState.value = sanitizeAppState(raw, b.platform);
    setWallets(list);
    finishSend();
    const usable = list.filter((w) => !w.error);
    // Web: a wallet whose saved credential was never opened returns the user to that step.
    const unverified = b.platform === 'web' ? usable.find((w) => w.verified === false) : undefined;
    const preferred = unverified ?? usable.find((w) => w.id === appState.value.activeWallet) ?? usable[0];
    if (unverified) navigate('unlock', undefined, { replace: true });
    if (preferred) {
      activeWalletId.value = preferred.id;
      if (!preferred.locked) {
        try {
          await openSession(preferred.id);
        } catch (e) {
          if (!isBackendError(e, 'locked')) throw e;
          await refreshWallets();
        }
      }
    }
    // Started once for the lifetime of the page.
    if (b.onExternalChange) b.onExternalChange(() => void onExternalChange());
    startSessionEffects();
    startIdleLock();
    startRouteGuard();
  } catch (e) {
    initError.value = errorMessage(e);
  } finally {
    initialized.value = true;
  }
}

/** Keeps the route valid for the current phase. */
export function startRouteGuard(): () => void {
  return effect(() => {
    const p = phase.value;
    const r = route.value;
    if (!routeAllowed(p, r, platform.value)) {
      const home = homeRoute(p);
      navigate(home.name, undefined, { replace: true });
    }
  });
}

/** Another window changed persisted state (web: other tab). */
async function onExternalChange(): Promise<void> {
  try {
    await reloadAppState();
    const list = await refreshWallets();
    const active = list.find((w) => w.id === activeWalletId.value);
    if (activeWalletId.value && (!active || active.error)) {
      closeSession();
      const next = usableWallets.value[0];
      if (next) await activateWallet(next.id);
      else activeWalletId.value = undefined;
    } else if (active?.locked && activeWallet.value?.id === active.id) {
      closeSession();
    }
  } catch {
    // Keep the current state; the next change retries.
  }
}

export { startRouter };
