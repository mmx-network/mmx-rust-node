/** Web wallet backend entry: wires the browser's storage, network and the WASM wallet core. */
import { BackendError, type Backend } from '../types';
import { WebBackend } from './backend';
import { memoryStorage } from './memory-storage';
import type { StorageLike } from './store';
import { loadBrowserWasm } from './wasm';

/** localStorage, or memory for this session when the browser blocks site storage. */
function browserStorage(): StorageLike {
  try {
    const storage = window.localStorage;
    if (storage) return storage;
  } catch {
    // Site data blocked (privacy settings): nothing is remembered after closing the tab.
  }
  return memoryStorage();
}

export async function createBackend(): Promise<Backend> {
  let backend: WebBackend;
  try {
    backend = new WebBackend({
      loadWasm: loadBrowserWasm,
      fetch: (input, init) => window.fetch(input, init),
      storage: browserStorage(),
      rpcUrl: import.meta.env.VITE_RPC_URL,
      events: window,
    });
  } catch {
    throw new BackendError('internal');
  }
  // The wallet core is needed for every wallet action: fetch it while the user reads the first screen.
  const idle = (window as { requestIdleCallback?: (cb: () => void, o?: { timeout: number }) => number }).requestIdleCallback;
  if (idle) idle(() => backend.preload(), { timeout: 2000 });
  else setTimeout(() => backend.preload(), 200);
  return backend;
}

export { WebBackend } from './backend';
