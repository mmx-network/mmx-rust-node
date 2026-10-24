/**
 * Locally tracked broadcasts. The public server does not report pending
 * entries, so sends are followed via `/transaction?id=` until included
 * (confirmed or failed) or expired (`expires < height`).
 */
import type { TxInfo } from './types';

export type PendingStatus = 'pending' | 'confirmed' | 'failed' | 'expired';

export interface PendingTx {
  txid: string;
  walletId: string;
  to: string;
  contract: string;
  symbol: string;
  decimals: number;
  amount: bigint;
  fee: bigint;
  memo?: string;
  /** Last height at which the transaction can be included. */
  expires: number;
  createdAt: number;
  status: PendingStatus;
  /** The broadcast result was not confirmed by the server (connection failed). */
  uncertain?: boolean;
  /** Height of inclusion once confirmed or failed. */
  includedHeight?: number;
  /** When the status last changed to a final state. */
  settledAt?: number;
}

/** How long settled (failed/expired) local entries stay visible. */
export const SETTLED_RETENTION_MS = 60 * 60 * 1000;

export function nextStatus(p: PendingTx, tx: TxInfo | null, height: number, now = Date.now()): PendingTx {
  if (p.status !== 'pending') return p;
  if (tx && tx.height !== undefined) {
    return { ...p, status: tx.didFail ? 'failed' : 'confirmed', includedHeight: tx.height, settledAt: now };
  }
  if (height > p.expires) return { ...p, status: 'expired', settledAt: now };
  return p;
}

/**
 * Drops entries that history now contains (except failed ones, which stay so
 * the matching history item can show the failure) and settled entries older
 * than the retention period.
 */
export function prunePending(list: readonly PendingTx[], historyTxids: ReadonlySet<string>, now = Date.now()): PendingTx[] {
  return list.filter((p) => {
    if (p.status !== 'failed' && historyTxids.has(p.txid)) return false;
    if (p.status === 'pending') return true;
    return now - (p.settledAt ?? now) < SETTLED_RETENTION_MS;
  });
}

/** Total pending outgoing per contract (native includes the fees). */
export function pendingOutgoing(list: readonly PendingTx[], nativeContract: string): Map<string, bigint> {
  const out = new Map<string, bigint>();
  for (const p of list) {
    if (p.status !== 'pending') continue;
    out.set(p.contract, (out.get(p.contract) ?? 0n) + p.amount);
    if (p.fee > 0n) out.set(nativeContract, (out.get(nativeContract) ?? 0n) + p.fee);
  }
  return out;
}
