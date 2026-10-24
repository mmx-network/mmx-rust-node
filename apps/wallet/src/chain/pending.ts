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

function addTo(out: Map<string, bigint>, contract: string, amount: bigint): void {
  if (amount > 0n) out.set(contract, (out.get(contract) ?? 0n) + amount);
}

/**
 * What open sends will take out of the wallet, per contract: fees always, the
 * amount only when it goes to someone else (a move between the wallet's own
 * addresses costs just the fee).
 */
export function pendingOutgoing(
  list: readonly PendingTx[],
  nativeContract: string,
  ownAddresses: ReadonlySet<string>,
): Map<string, bigint> {
  const out = new Map<string, bigint>();
  for (const p of list) {
    if (p.status !== 'pending') continue;
    if (!ownAddresses.has(p.to)) addTo(out, p.contract, p.amount);
    addTo(out, nativeContract, p.fee);
  }
  return out;
}

/**
 * What open sends hold back from spending until they are included, per
 * contract: amount plus fee for every send, moves to own addresses included
 * (the moved amount is not spendable before the transaction confirms).
 */
export function pendingReserved(list: readonly PendingTx[], nativeContract: string): Map<string, bigint> {
  const out = new Map<string, bigint>();
  for (const p of list) {
    if (p.status !== 'pending') continue;
    addTo(out, p.contract, p.amount);
    addTo(out, nativeContract, p.fee);
  }
  return out;
}
