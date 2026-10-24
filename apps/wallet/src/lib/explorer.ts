/** Links to the public block explorer (the only external URLs the app opens). */
import { normalizeAddress } from './address';

export const EXPLORER_BASE = 'https://explore.mmx.network/#/explore';

const TXID = /^[0-9a-fA-F]{64}$/;

export function isTxid(value: string): boolean {
  return TXID.test(value);
}

export function transactionUrl(txid: string): string | null {
  return isTxid(txid) ? `${EXPLORER_BASE}/transaction/${txid.toUpperCase()}` : null;
}

export function addressUrl(address: string): string | null {
  const normalized = normalizeAddress(address);
  return normalized ? `${EXPLORER_BASE}/address/${normalized}` : null;
}

/** True for URLs this app is allowed to open externally. */
export function isExplorerUrl(url: string): boolean {
  const prefix = `${EXPLORER_BASE}/`;
  if (!url.startsWith(prefix)) return false;
  const [kind, id, ...rest] = url.slice(prefix.length).split('/');
  if (rest.length > 0 || !id) return false;
  if (kind === 'transaction') return isTxid(id);
  if (kind === 'address') return normalizeAddress(id) === id;
  return false;
}
