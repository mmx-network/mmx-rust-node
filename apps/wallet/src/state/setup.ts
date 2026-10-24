/**
 * Wallet setup in progress: the create flow holds new recovery words in
 * memory until they are confirmed, and the import flow holds the words typed
 * into it (nothing is stored before). Auto-lock also applies to it, and a
 * lock ends it.
 */
import { signal } from '@preact/signals';

const holders = new Set<symbol>();

/** A create or import flow holds recovery words that are not stored yet. */
export const setupActive = signal(false);

/** One line for the welcome screen after a setup was cancelled by a lock. */
export const setupNotice = signal<string | null>(null);

/** Marks new recovery words as held; call the returned function when they are dropped. */
export function beginSetup(): () => void {
  const token = Symbol('setup');
  holders.add(token);
  setupActive.value = true;
  setupNotice.value = null;
  return () => {
    if (holders.delete(token)) setupActive.value = holders.size > 0;
  };
}

/** Ends every setup in progress (on lock). Returns whether one was running. */
export function cancelSetup(): boolean {
  if (holders.size === 0) return false;
  holders.clear();
  setupActive.value = false;
  return true;
}

export function clearSetupNotice(): void {
  setupNotice.value = null;
}
