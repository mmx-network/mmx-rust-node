/**
 * Presentation of activity items: titles, icons, amount tone and the
 * counterparty line. Pure functions shared by the Activity screen and the
 * Overview's recent activity.
 */
import { shortenAddress, type IconName, type Tone } from '../../components';
import { REWARD_LABELS, type ActivityAmount, type ActivityItem, type ActivityStatus, type Counterparty } from '../../../state';

export function activityTitle(item: ActivityItem): string {
  switch (item.kind) {
    case 'received':
      return 'Received';
    case 'sent':
      return 'Sent';
    case 'moved':
      return 'Moved between your addresses';
    case 'reward':
      return REWARD_LABELS[item.rewardType ?? 'farming'];
    case 'fee':
      return 'Network fee';
  }
}

export function activityIcon(item: ActivityItem, status: ActivityStatus): { icon: IconName; tone: Tone } {
  if (status === 'failed' || status === 'expired') return { icon: 'x-circle', tone: 'negative' };
  if (status === 'pending') return { icon: 'clock', tone: 'warning' };
  switch (item.kind) {
    case 'received':
      return { icon: 'arrow-down-left', tone: isUnverified(item) ? 'neutral' : 'positive' };
    case 'reward':
      return { icon: 'reward', tone: 'accent' };
    case 'moved':
      return { icon: 'swap', tone: 'neutral' };
    default:
      return { icon: 'arrow-up-right', tone: 'neutral' };
  }
}

export const STATUS_LABELS: Record<ActivityStatus, string> = {
  confirmed: 'Confirmed',
  pending: 'Pending',
  failed: 'Failed',
  expired: 'Expired',
};

export function statusTone(status: ActivityStatus): Tone {
  switch (status) {
    case 'confirmed':
      return 'positive';
    case 'pending':
      return 'warning';
    default:
      return 'negative';
  }
}

export interface DisplayAmount {
  /** Signed atomic value to show. */
  value: bigint;
  decimals: number;
  symbol: string;
  contract: string;
  native: boolean;
  verified: boolean;
  sign: 'auto' | 'always' | 'never';
  tone: 'positive' | 'neutral' | 'muted';
}

/**
 * The amount of a list row: incoming in the positive tone with "+", outgoing
 * with "−", moves without a sign. Unverified tokens and unsuccessful
 * transfers are muted so they never look like real income.
 */
export function rowAmount(item: ActivityItem, status: ActivityStatus, nativeDecimals: number): DisplayAmount | null {
  const unsuccessful = status === 'failed' || status === 'expired';
  const a: ActivityAmount | undefined = item.primary;
  if (!a) {
    if (item.fee <= 0n) return null;
    return {
      value: -item.fee,
      decimals: nativeDecimals,
      symbol: 'MMX',
      contract: '',
      native: true,
      verified: true,
      sign: 'auto',
      tone: unsuccessful ? 'muted' : 'neutral',
    };
  }
  const base = { decimals: a.decimals, symbol: a.symbol, contract: a.contract, native: a.native, verified: a.verified };
  if (item.kind === 'moved') {
    return { ...base, value: a.amount < 0n ? -a.amount : a.amount, sign: 'never', tone: 'muted' };
  }
  const incoming = a.amount > 0n;
  const tone = unsuccessful || !a.verified ? 'muted' : incoming ? 'positive' : 'neutral';
  return { ...base, value: a.amount, sign: incoming ? 'always' : 'auto', tone };
}

/** Name or short address of a counterparty. */
export function counterpartyName(cp: Counterparty): string {
  return cp.name ?? shortenAddress(cp.address);
}

/** "From Alice" / "To mmx1abcdef…wxyz" for the row subtitle; null when not applicable. */
export function counterpartyLine(item: ActivityItem, cp: Counterparty | null | undefined): string | null {
  if (!cp) return null;
  const name = counterpartyName(cp);
  if (item.kind === 'received') return `From ${name}`;
  if (item.kind === 'sent' || item.kind === 'fee') return `To ${name}`;
  return null;
}

/** Label of the counterparty row in the detail panel. */
export function counterpartyLabel(item: ActivityItem): string {
  return item.kind === 'received' ? 'From' : 'To';
}

/** Extra assets beyond the primary one (multi-asset transfers). */
export function extraAssetCount(item: ActivityItem): number {
  return Math.max(0, item.amounts.length - (item.primary ? 1 : 0));
}

/** Whether an item's primary asset is an unverified token. */
export function isUnverified(item: ActivityItem): boolean {
  return item.primary !== undefined && !item.primary.native && !item.primary.verified;
}
