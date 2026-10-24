/**
 * Activity items: history rows grouped per transaction with net amounts over
 * the wallet's own addresses (docs/wallet-app.md §6.4).
 */
import { NATIVE_CONTRACT } from '../lib/address';
import { dayKey, formatDayHeading } from '../lib/time';
import type { PendingTx } from './pending';
import type { Currency, HistoryRow, TxInfo, TxType } from './types';

export type ActivityKind = 'received' | 'sent' | 'moved' | 'reward' | 'fee';
export type RewardType = 'farming' | 'timelord' | 'project';
export type ActivityStatus = 'confirmed' | 'pending' | 'failed' | 'expired';

export interface ActivityAmount extends Currency {
  verified: boolean;
  /** Signed net change over own addresses; positive gross amount for 'moved'. */
  amount: bigint;
}

export interface ActivityItem {
  /** Unique, URL-safe: the txid, or `<blockhash>-<farm|vdf|project>` for rewards. */
  key: string;
  txid: string;
  kind: ActivityKind;
  rewardType?: RewardType;
  status: ActivityStatus;
  /** Undefined while pending. */
  height?: number;
  timeMs: number;
  amounts: ActivityAmount[];
  /** The amount to show in a list row. Undefined for fee-only items. */
  primary?: ActivityAmount;
  /** Network fee paid by the wallet (native atomic units). */
  fee: bigint;
  memo?: string;
  /** The wallet's addresses involved. */
  ownAddresses: string[];
  /** Recipient of a locally tracked send. */
  to?: string;
  /** Item comes from a locally tracked broadcast, not from history. */
  local?: boolean;
}

export type ActivityFilterKind = 'all' | 'received' | 'sent' | 'rewards';

export interface ActivityFilter {
  kind: ActivityFilterKind;
  /** Only items involving this contract; null = all assets. */
  contract: string | null;
}

const REWARD_SUFFIX: Partial<Record<TxType, { suffix: string; type: RewardType }>> = {
  REWARD: { suffix: 'farm', type: 'farming' },
  VDF_REWARD: { suffix: 'vdf', type: 'timelord' },
  PROJECT_REWARD: { suffix: 'project', type: 'project' },
};

export const REWARD_LABELS: Record<RewardType, string> = {
  farming: 'Farming reward',
  timelord: 'Timelord reward',
  project: 'Project reward',
};

export const KEY_PATTERN = /^[0-9A-F]{64}(-(farm|vdf|project))?$/;

export function isActivityKey(key: string): boolean {
  return KEY_PATTERN.test(key);
}

export function groupKey(row: HistoryRow): string {
  const reward = REWARD_SUFFIX[row.type];
  return reward ? `${row.txid}-${reward.suffix}` : row.txid;
}

export type VerifiedLookup = (contract: string) => boolean;

function pickPreferred(list: ActivityAmount[]): ActivityAmount | undefined {
  return list.find((a) => a.native) ?? list[0];
}

/** Groups rows (any order) into items, newest first. */
export function groupRows(rows: readonly HistoryRow[], isVerified: VerifiedLookup = () => false): ActivityItem[] {
  const groups = new Map<string, HistoryRow[]>();
  for (const row of rows) {
    const key = groupKey(row);
    const list = groups.get(key);
    if (list) list.push(row);
    else groups.set(key, [row]);
  }
  const items: ActivityItem[] = [];
  for (const [key, group] of groups) items.push(buildItem(key, group, isVerified));
  return items.sort(compareItems);
}

function buildItem(key: string, rows: HistoryRow[], isVerified: VerifiedLookup): ActivityItem {
  const first = rows[0]!;
  const nets = new Map<string, ActivityAmount>();
  const received = new Map<string, ActivityAmount>();
  const own = new Set<string>();
  let fee = 0n;
  let hasSpend = false;
  let hasReceive = false;
  let height = first.height;
  let timeMs = first.timeMs;
  let memo: string | undefined;

  const entry = (map: Map<string, ActivityAmount>, row: HistoryRow): ActivityAmount => {
    let a = map.get(row.contract);
    if (!a) {
      a = {
        contract: row.contract,
        symbol: row.symbol,
        decimals: row.decimals,
        native: row.native,
        verified: row.native || isVerified(row.contract),
        amount: 0n,
      };
      map.set(row.contract, a);
    }
    return a;
  };

  for (const row of rows) {
    own.add(row.address);
    height = Math.max(height, row.height);
    timeMs = Math.max(timeMs, row.timeMs);
    if (!memo && row.memo) memo = row.memo;
    switch (row.type) {
      case 'TXFEE':
        fee += row.amount;
        break;
      case 'SPEND':
        hasSpend = true;
        entry(nets, row).amount -= row.amount;
        break;
      case 'RECEIVE':
        hasReceive = true;
        entry(nets, row).amount += row.amount;
        entry(received, row).amount += row.amount;
        break;
      default:
        entry(nets, row).amount += row.amount;
        break;
    }
  }

  const reward = REWARD_SUFFIX[first.type];
  const nonZero = [...nets.values()].filter((a) => a.amount !== 0n);
  let kind: ActivityKind;
  let amounts: ActivityAmount[];
  let primary: ActivityAmount | undefined;
  if (reward) {
    kind = 'reward';
    amounts = nonZero;
    primary = pickPreferred(nonZero);
  } else if (nonZero.some((a) => a.amount < 0n)) {
    kind = 'sent';
    amounts = nonZero;
    primary = pickPreferred(nonZero.filter((a) => a.amount < 0n));
  } else if (nonZero.length > 0) {
    kind = 'received';
    amounts = nonZero;
    primary = pickPreferred(nonZero);
  } else if (hasSpend && hasReceive) {
    kind = 'moved';
    amounts = [...received.values()].filter((a) => a.amount > 0n);
    primary = pickPreferred(amounts);
  } else {
    kind = 'fee';
    amounts = [];
  }

  const item: ActivityItem = {
    key,
    txid: first.txid,
    kind,
    status: 'confirmed',
    height,
    timeMs,
    amounts,
    fee,
    ownAddresses: [...own].sort(),
  };
  if (reward) item.rewardType = reward.type;
  if (primary) item.primary = primary;
  if (memo) item.memo = memo;
  return item;
}

/** Pending first (newest first), then by height and time, newest first. */
export function compareItems(a: ActivityItem, b: ActivityItem): number {
  const pa = a.height === undefined ? 1 : 0;
  const pb = b.height === undefined ? 1 : 0;
  if (pa !== pb) return pb - pa;
  return (b.height ?? 0) - (a.height ?? 0) || b.timeMs - a.timeMs || (a.key < b.key ? -1 : a.key > b.key ? 1 : 0);
}

export function matchesFilter(item: ActivityItem, filter: ActivityFilter): boolean {
  switch (filter.kind) {
    case 'received':
      if (item.kind !== 'received') return false;
      break;
    case 'sent':
      if (item.kind !== 'sent' && item.kind !== 'fee') return false;
      break;
    case 'rewards':
      if (item.kind !== 'reward') return false;
      break;
    default:
      break;
  }
  if (filter.contract) {
    const involved =
      item.amounts.some((a) => a.contract === filter.contract) || (filter.contract === NATIVE_CONTRACT && item.fee > 0n);
    if (!involved) return false;
  }
  return true;
}

export function filterItems(items: readonly ActivityItem[], filter: ActivityFilter): ActivityItem[] {
  if (filter.kind === 'all' && !filter.contract) return items.slice();
  return items.filter((i) => matchesFilter(i, filter));
}

/** A locally tracked send as an activity item. */
export function pendingItem(p: PendingTx, ownAddresses: ReadonlySet<string>, isVerified: VerifiedLookup = () => false): ActivityItem {
  const toSelf = ownAddresses.has(p.to);
  const native = p.contract === NATIVE_CONTRACT;
  const amount: ActivityAmount = {
    contract: p.contract,
    symbol: p.symbol,
    decimals: p.decimals,
    native,
    verified: native || isVerified(p.contract),
    amount: toSelf ? p.amount : -p.amount,
  };
  const item: ActivityItem = {
    key: p.txid,
    txid: p.txid,
    kind: toSelf ? 'moved' : 'sent',
    status: p.status === 'confirmed' ? 'pending' : p.status,
    timeMs: p.createdAt,
    amounts: [amount],
    primary: amount,
    fee: p.fee,
    ownAddresses: [],
    to: p.to,
    local: true,
  };
  if (p.memo) item.memo = p.memo;
  return item;
}

/**
 * History items plus locally tracked sends that history does not contain yet.
 * A failed local send marks the matching history item as failed.
 */
export function mergeActivity(
  historyItems: readonly ActivityItem[],
  pending: readonly PendingTx[],
  ownAddresses: ReadonlySet<string>,
  isVerified: VerifiedLookup = () => false,
): ActivityItem[] {
  const known = new Set(historyItems.map((i) => i.txid));
  const failed = new Set(pending.filter((p) => p.status === 'failed').map((p) => p.txid));
  const local = pending.filter((p) => !known.has(p.txid)).map((p) => pendingItem(p, ownAddresses, isVerified));
  const merged = historyItems.map((i) => (failed.has(i.txid) ? { ...i, status: 'failed' as const } : i));
  return local.concat(merged).sort(compareItems);
}

/**
 * The other party of a transfer: for received funds the sender (else the first
 * foreign input), for sent funds the foreign recipient (preferring the primary
 * currency). A send that paid several recipients in that currency has no
 * single counterparty.
 */
export function counterpartyAddress(item: ActivityItem, tx: TxInfo, own: ReadonlySet<string>): string | undefined {
  if (item.kind === 'received') {
    if (tx.sender && !own.has(tx.sender)) return tx.sender;
    return tx.inputs.find((i) => !own.has(i.address))?.address;
  }
  if (item.kind === 'sent' || item.kind === 'fee') {
    if (item.to && !own.has(item.to)) return item.to;
    const foreign = tx.outputs.filter((o) => !own.has(o.address));
    const contract = item.primary?.contract;
    const inContract = new Set(foreign.filter((o) => o.contract === contract).map((o) => o.address));
    if (inContract.size > 1) return undefined;
    return inContract.size === 1 ? [...inContract][0] : foreign[0]?.address;
  }
  return undefined;
}

/**
 * The recipient when a sent item can be repeated as a plain transfer: no
 * contract call, every input from the wallet, every input and output in the
 * one asset sent, and all funds that left the wallet went to one other
 * address. Anything else (an NFT purchase or swap, a payment to several
 * recipients) is undefined, since a plain transfer would not do the same.
 */
export function repeatRecipient(item: ActivityItem, tx: TxInfo, own: ReadonlySet<string>): string | undefined {
  const primary = item.primary;
  if (item.kind !== 'sent' || !primary || primary.amount >= 0n || item.amounts.length !== 1 || tx.contractCall) return undefined;
  if (tx.inputs.length === 0 || tx.inputs.some((i) => !own.has(i.address) || i.contract !== primary.contract)) return undefined;
  if (tx.outputs.some((o) => o.contract !== primary.contract)) return undefined;
  const foreign = tx.outputs.filter((o) => !own.has(o.address));
  const to = foreign[0]?.address;
  if (!to || foreign.some((o) => o.address !== to)) return undefined;
  const total = foreign.reduce((sum, o) => sum + o.amount, 0n);
  return total === -primary.amount ? to : undefined;
}

/**
 * Memo of the transfer as seen by the counterparty (sent) or by the wallet
 * (received). Outputs of a contract call carry the contract's notes, not a
 * memo someone wrote, so they are not shown.
 */
export function memoFromTx(item: ActivityItem, tx: TxInfo, own: ReadonlySet<string>): string | undefined {
  if (item.memo) return item.memo;
  if (tx.contractCall) return undefined;
  if (item.kind === 'sent') return tx.outputs.find((o) => !own.has(o.address) && o.memo)?.memo;
  if (item.kind === 'received' || item.kind === 'moved') return tx.outputs.find((o) => own.has(o.address) && o.memo)?.memo;
  return undefined;
}

export interface DayGroup {
  key: string;
  label: string;
  items: ActivityItem[];
}

/** Consecutive items grouped by local calendar day; pending items form their own group. */
export function groupByDay(items: readonly ActivityItem[], now = Date.now()): DayGroup[] {
  const groups: DayGroup[] = [];
  for (const item of items) {
    const pending = item.height === undefined;
    const key = pending ? 'pending' : dayKey(item.timeMs);
    let g = groups[groups.length - 1];
    if (!g || g.key !== key) {
      g = { key, label: pending ? 'Pending' : formatDayHeading(item.timeMs, now), items: [] };
      groups.push(g);
    }
    g.items.push(item);
  }
  for (const g of groups) {
    if (g.key === 'pending' && g.items.some((i) => i.status !== 'pending')) g.label = 'Recent';
  }
  return groups;
}

/** Confirmations of an item at the given chain height (0 while pending). */
export function confirmations(item: ActivityItem, height: number | undefined): number {
  if (item.height === undefined || height === undefined || height < item.height) return 0;
  return height - item.height + 1;
}
