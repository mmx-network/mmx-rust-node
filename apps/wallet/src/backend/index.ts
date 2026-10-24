import type { Backend } from './types';

let pending: Promise<Backend> | null = null;

/**
 * Returns the backend for this build. Each platform module is loaded with a
 * dynamic import guarded by the build-time constant, so other platforms'
 * code is not bundled.
 */
export function getBackend(): Promise<Backend> {
  if (!pending) {
    pending = load();
    pending.catch(() => {
      pending = null;
    });
  }
  return pending;
}

async function load(): Promise<Backend> {
  if (__PLATFORM__ === 'mock') {
    return (await import('./mock')).createBackend();
  }
  if (__PLATFORM__ === 'desktop') {
    return (await import('./desktop')).createBackend();
  }
  return (await import('./web/index')).createBackend();
}
