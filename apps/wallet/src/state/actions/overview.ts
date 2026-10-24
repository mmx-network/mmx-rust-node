/** Overview helpers derived from the open wallet's data. */
import { computed } from '@preact/signals';
import { NATIVE_CONTRACT } from '../../lib/address';
import { balanceSeries, otherAssets, pendingOutgoingTotals, verifiedAssets } from '../session';

/** MMX leaving the wallet in pending sends (amounts plus fees). */
export const pendingNativeOutgoing = computed(() => pendingOutgoingTotals.value.get(NATIVE_CONTRACT) ?? 0n);

/** Whitelisted tokens held by the wallet (without MMX). */
export const verifiedTokens = computed(() => verifiedAssets.value.filter((a) => !a.native));

/** The wallet holds tokens of any kind. */
export const hasTokens = computed(() => verifiedTokens.value.length > 0 || otherAssets.value.length > 0);

export interface BalanceChange {
  /** Balance at the end minus balance at the start of the shown period. */
  delta: bigint;
  start: number;
  /** The period covers the whole selected range. */
  complete: boolean;
}

/** Change of the charted balance over the shown period. */
export const balanceChange = computed<BalanceChange | null>(() => {
  const s = balanceSeries.value;
  if (!s || s.points.length === 0) return null;
  const first = s.points[0]!;
  const last = s.points[s.points.length - 1]!;
  return { delta: last.v - first.v, start: s.start, complete: s.complete };
});
