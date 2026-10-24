/** Activity screen helpers: asset filter choices, "Send again" and quiet copying. */
import { computed } from '@preact/signals';
import type { ActivityItem } from '../../chain/activity';
import { sortAssets } from '../../chain/balances';
import { NATIVE_CONTRACT } from '../../lib/address';
import { toInputString } from '../../lib/amount';
import { errorMessage } from '../../lib/errors';
import { backend } from '../core';
import { activity, assets, repeatRecipientFor, statusFor } from '../session';
import { startSend, type SendDraft } from '../send';
import { notify } from '../toasts';

export interface ActivityAssetOption {
  contract: string;
  symbol: string;
  native: boolean;
  verified: boolean;
}

/**
 * Assets for the activity filter: everything the wallet holds plus every
 * asset that appears in loaded activity. Order: MMX, verified tokens, other tokens.
 */
export const activityAssetOptions = computed<ActivityAssetOption[]>(() => {
  const map = new Map<string, ActivityAssetOption>();
  map.set(NATIVE_CONTRACT, { contract: NATIVE_CONTRACT, symbol: 'MMX', native: true, verified: true });
  for (const a of assets.value ?? []) {
    if (!map.has(a.contract)) map.set(a.contract, { contract: a.contract, symbol: a.symbol, native: a.native, verified: a.verified });
  }
  for (const item of activity.value) {
    for (const a of item.amounts) {
      const known = map.get(a.contract);
      if (!known) map.set(a.contract, { contract: a.contract, symbol: a.symbol, native: a.native, verified: a.verified });
      else if (a.verified && !known.verified) map.set(a.contract, { ...known, verified: true });
    }
  }
  return sortAssets([...map.values()]);
});

/**
 * Prefill for repeating a sent transfer (recipient, asset, amount and memo),
 * or null when the item cannot be repeated (not a send, still pending,
 * unverified token, a move, contract call or payment to several recipients,
 * or recipient not known yet).
 */
export function sendAgainDraft(item: ActivityItem, memo?: string): Partial<SendDraft> | null {
  if (item.kind !== 'sent') return null;
  // A transfer still on its way must not be repeated by accident.
  if (statusFor(item) === 'pending') return null;
  const primary = item.primary;
  if (!primary || primary.amount >= 0n) return null;
  if (!primary.native && !primary.verified) return null;
  const to = repeatRecipientFor(item);
  if (!to) return null;
  const draft: Partial<SendDraft> = {
    to,
    contract: primary.contract,
    amount: toInputString(-primary.amount, primary.decimals),
  };
  if (memo) draft.memo = memo;
  return draft;
}

/** Opens Send prefilled with a previous transfer. */
export async function sendAgain(item: ActivityItem, memo?: string): Promise<boolean> {
  const draft = sendAgainDraft(item, memo);
  if (!draft) return false;
  await startSend(draft);
  return true;
}

/**
 * Copies a non-secret value (address, transaction ID) without a toast, for
 * copy buttons that confirm inline. Failures are reported and rethrown.
 */
export async function copyValue(text: string): Promise<void> {
  try {
    await backend().copyText(text, false);
  } catch (e) {
    notify(errorMessage(e), 'error');
    throw e;
  }
}
