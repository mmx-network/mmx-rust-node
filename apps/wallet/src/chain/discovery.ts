/** Finds used addresses after an import (`/address/history?limit=1`, bounded concurrency). */
import type { RpcClient } from './rpc';
import { DEFAULT_ADDRESS_COUNT, MAX_ADDRESSES } from '../lib/appstate';

export interface DiscoveryOptions {
  concurrency?: number;
  onProgress?: (checked: number, total: number) => void;
  signal?: { cancelled: boolean };
}

/** Index of the last address with any history, or −1 when none was used. */
export async function findLastUsedIndex(
  rpc: RpcClient,
  addresses: readonly string[],
  options: DiscoveryOptions = {},
): Promise<number> {
  const concurrency = Math.max(1, options.concurrency ?? 4);
  let next = 0;
  let checked = 0;
  let last = -1;
  const worker = async () => {
    while (next < addresses.length) {
      if (options.signal?.cancelled) return;
      const index = next++;
      if (await rpc.isUsed(addresses[index]!)) last = Math.max(last, index);
      checked++;
      options.onProgress?.(checked, addresses.length);
    }
  };
  await Promise.all(Array.from({ length: Math.min(concurrency, addresses.length) }, worker));
  return last;
}

/** Address count after discovery: at least `minimum`, enough to include the last used address. */
export function discoveredCount(lastUsed: number, minimum = DEFAULT_ADDRESS_COUNT, maximum = MAX_ADDRESSES): number {
  return Math.min(maximum, Math.max(minimum, lastUsed + 1));
}
