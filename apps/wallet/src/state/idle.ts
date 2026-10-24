/**
 * Auto-lock after inactivity (§6.6). Web: always (default 15 min) and on
 * `pagehide`; desktop: only passphrase wallets and only when
 * autoLockMinutes > 0.
 */
import { DEFAULT_AUTO_LOCK_MINUTES } from '../lib/appstate';
import { platform, settings, wallets } from './core';
import { lock } from './wallets';

const ACTIVITY_EVENTS = ['pointerdown', 'pointermove', 'keydown', 'wheel', 'touchstart', 'focus'] as const;
const CHECK_INTERVAL_MS = 15_000;

let lastActivity = Date.now();
let locking = false;

export function markActivity(now = Date.now()): void {
  lastActivity = now;
}

/** Minutes until auto-lock for the current state, or null when auto-lock does not apply. */
export function autoLockDelayMinutes(): number | null {
  const minutes = settings.value.autoLockMinutes;
  if (platform.value === 'web') {
    if (!wallets.value.some((w) => !w.locked)) return null;
    return minutes > 0 ? minutes : DEFAULT_AUTO_LOCK_MINUTES;
  }
  if (minutes <= 0) return null;
  return wallets.value.some((w) => w.withPassphrase && !w.locked) ? minutes : null;
}

export function idleExpired(now = Date.now()): boolean {
  const minutes = autoLockDelayMinutes();
  return minutes !== null && now - lastActivity >= minutes * 60_000;
}

async function lockNow(): Promise<void> {
  if (locking) return;
  locking = true;
  try {
    await lock();
  } catch {
    // Locking is retried on the next check.
  } finally {
    locking = false;
    lastActivity = Date.now();
  }
}

/** Starts idle tracking; returns a stop function. */
export function startIdleLock(): () => void {
  if (typeof window === 'undefined') return () => undefined;
  lastActivity = Date.now();
  let lastMove = 0;
  const onActivity = (e: Event) => {
    const now = Date.now();
    if (e.type === 'pointermove') {
      if (now - lastMove < 1000) return;
      lastMove = now;
    }
    // Activity after the deadline (e.g. returning to a sleeping laptop) must not postpone the lock.
    if (idleExpired(now)) {
      void lockNow();
      return;
    }
    lastActivity = now;
  };
  const check = () => {
    if (idleExpired()) void lockNow();
  };
  const onVisibility = () => {
    if (document.visibilityState === 'visible') check();
  };
  const onPageHide = () => {
    if (platform.value === 'web') void lockNow();
  };
  for (const type of ACTIVITY_EVENTS) window.addEventListener(type, onActivity, { capture: true, passive: true });
  document.addEventListener('visibilitychange', onVisibility);
  window.addEventListener('pagehide', onPageHide);
  const timer = setInterval(check, CHECK_INTERVAL_MS);
  return () => {
    for (const type of ACTIVITY_EVENTS) window.removeEventListener(type, onActivity, { capture: true });
    document.removeEventListener('visibilitychange', onVisibility);
    window.removeEventListener('pagehide', onPageHide);
    clearInterval(timer);
  };
}
