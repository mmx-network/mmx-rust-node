/**
 * History cache (docs/wallet-app.md §6.4).
 *
 * Per address the cache holds every row from `bottom` up to the newest block.
 * `since` and `until` are inclusive and the node always completes the last
 * block of a page, so a full page covers [minHeight, …] completely and the
 * next older page uses `until = minHeight − 1`. A page with fewer entries than
 * the limit means nothing older exists (bottom = 0). Page sizes and heights
 * are taken from everything the node returned, NFT rows included, because the
 * node's limit counts them although the cache does not keep them.
 *
 * Across addresses, rows at or above the frontier — the highest bottom among
 * addresses that still have older rows — are complete for the whole wallet.
 *
 * Refresh re-reads from a few blocks below `top`, the newest height the
 * history server has actually returned, and replaces those rows, so a short
 * reorg cannot leave stale rows. For inactive addresses, the last successful
 * refresh's node head also advances `since`, with a 1,000-block overlap for
 * lagging history servers. This uses the previous check, so a long pause never
 * skips the unchecked gap. Older cached rows are retained.
 */
import { BackendError } from '../backend/types';
import type { RpcClient } from './rpc';
import type { ChainParams, HistoryPage, HistoryRow } from './types';

export const FIRST_PAGE_LIMIT = 25;
export const PAGE_LIMIT = 1000;
export const WALLET_ROW_CAP = 20_000;
export const REFRESH_OVERLAP = 6;
/** Head/history skew allowed when advancing past an address's last activity. */
export const REFRESH_HEAD_OVERLAP = 1000;

export interface AddressHistory {
  /** Newest first. */
  rows: HistoryRow[];
  /**
   * Newest height the history server returned for this address (NFT rows
   * included), as of the last first page or refresh; −1 when none.
   */
  top: number;
  /** Node head sampled before the last complete successful refresh, if known. */
  checkedHead?: number;
  /** Lowest height known complete (inclusive). */
  bottom: number;
  /** True when no rows exist below `bottom` (bottom is 0). */
  exhausted: boolean;
}

export type WalletHistory = ReadonlyMap<string, AddressHistory>;

/** Newest height among `rows`; −1 when there are none. */
export function maxHeight(rows: readonly HistoryRow[]): number {
  let m = -1;
  for (const r of rows) if (r.height > m) m = r.height;
  return m;
}

/** What a page tells about older rows: none left, or complete down to its lowest height. */
function pageBottom(page: HistoryPage, limit: number): { bottom: number; exhausted: boolean } {
  if (page.count < limit) return { bottom: 0, exhausted: true };
  // RpcClient.history rejects a full page without a confirmed height.
  if (!Number.isFinite(page.minHeight)) throw new BackendError('rpc_response_invalid', 'invalid field: history');
  return { bottom: page.minHeight, exhausted: page.minHeight === 0 };
}

/** Newest first; ties keep a deterministic order. */
export function compareRows(a: HistoryRow, b: HistoryRow): number {
  return (
    b.height - a.height ||
    (a.txid < b.txid ? 1 : a.txid > b.txid ? -1 : 0) ||
    a.type.localeCompare(b.type) ||
    a.address.localeCompare(b.address) ||
    a.contract.localeCompare(b.contract) ||
    (a.amount < b.amount ? 1 : a.amount > b.amount ? -1 : 0)
  );
}

/** Result of the first page for an address (no since/until). */
export function fromFirstPage(page: HistoryPage, limit: number): AddressHistory {
  return { rows: page.rows.slice().sort(compareRows), top: page.maxHeight, ...pageBottom(page, limit) };
}

/** Request for the next older page, or null if the address is exhausted. */
export function olderPageQuery(h: AddressHistory, limit = PAGE_LIMIT): { until: number; limit: number } | null {
  if (h.exhausted || h.bottom <= 0) return null;
  return { until: h.bottom - 1, limit };
}

/** Applies an older page fetched with `until = bottom − 1`. */
export function applyOlderPage(h: AddressHistory, page: HistoryPage, limit: number): AddressHistory {
  const until = h.bottom - 1;
  const older = page.rows.filter((r) => r.height <= until).sort(compareRows);
  return { ...h, rows: h.rows.concat(older), ...pageBottom(page, limit) };
}

/** Retain recent activity or 1,000 blocks behind the previous successful check. */
export function refreshSince(h: AddressHistory, overlap = REFRESH_OVERLAP, head?: number): number {
  // A rollback must reopen the range below the new head and remove cached
  // rows above it. The current head can lower the cursor, never advance it.
  const top = Math.min(h.top, head ?? h.top);
  const checkedHead = Math.min(h.checkedHead ?? -1, head ?? Infinity);
  return Math.max(h.bottom, top - overlap + 1, checkedHead - REFRESH_HEAD_OVERLAP + 1, 0);
}

/** Everything a refresh read: the rows at or above `since` and the newest height returned. */
export interface RangeResult {
  rows: HistoryRow[];
  /** Newest height in the responses (NFT rows included); −1 when none. */
  maxHeight: number;
}

/**
 * Replaces everything at or above `since` with `fresh` — the complete set of
 * rows the server holds from `since` on — and reports whether anything changed.
 */
export function applyRefresh(
  h: AddressHistory,
  since: number,
  fresh: RangeResult,
): { history: AddressHistory; changed: boolean } {
  const kept = h.rows.filter((r) => r.height < since);
  const replaced = h.rows.filter((r) => r.height >= since);
  const incoming = fresh.rows.filter((r) => r.height >= since).sort(compareRows);
  const changed = !sameRows(replaced, incoming);
  return {
    history: {
      ...h,
      rows: incoming.concat(kept),
      top: Math.max(maxHeight(kept), fresh.maxHeight),
      bottom: Math.min(h.bottom, since),
      exhausted: h.exhausted,
    },
    changed,
  };
}

function rowKey(r: HistoryRow): string {
  return `${r.height}|${r.txid}|${r.type}|${r.address}|${r.contract}|${r.amount}|${r.memo ?? ''}`;
}

/** Multiset equality of rows. */
export function sameRows(a: readonly HistoryRow[], b: readonly HistoryRow[]): boolean {
  if (a.length !== b.length) return false;
  const counts = new Map<string, number>();
  for (const r of a) counts.set(rowKey(r), (counts.get(rowKey(r)) ?? 0) + 1);
  for (const r of b) {
    const k = rowKey(r);
    const n = counts.get(k);
    if (!n) return false;
    counts.set(k, n - 1);
  }
  return true;
}

/** Highest bottom among addresses with older rows left; 0 when all are exhausted. */
export function frontier(history: WalletHistory, addresses: readonly string[]): number {
  let f = 0;
  for (const a of addresses) {
    const h = history.get(a);
    if (!h) return Infinity;
    if (!h.exhausted && h.bottom > f) f = h.bottom;
  }
  return f;
}

export function totalRows(history: WalletHistory, addresses: readonly string[]): number {
  let n = 0;
  for (const a of addresses) n += history.get(a)?.rows.length ?? 0;
  return n;
}

/** True when all addresses are loaded and exhausted. */
export function isComplete(history: WalletHistory, addresses: readonly string[]): boolean {
  return addresses.every((a) => history.get(a)?.exhausted === true);
}

/** Whether older rows can still be fetched (not exhausted and under the cap). */
export function canExtend(history: WalletHistory, addresses: readonly string[], cap = WALLET_ROW_CAP): boolean {
  return !isComplete(history, addresses) && totalRows(history, addresses) < cap;
}

/** Rows complete for the whole wallet (height ≥ frontier), newest first. */
export function completeRows(history: WalletHistory, addresses: readonly string[]): HistoryRow[] {
  const f = frontier(history, addresses);
  if (f === Infinity) return [];
  const out: HistoryRow[] = [];
  for (const a of addresses) {
    const h = history.get(a);
    if (!h) continue;
    for (const r of h.rows) {
      if (r.height >= f) out.push(r);
    }
  }
  return out.sort(compareRows);
}

/**
 * Addresses to extend next to move the frontier down: every non-exhausted
 * address whose bottom equals the frontier.
 */
export function addressesToExtend(history: WalletHistory, addresses: readonly string[]): string[] {
  const f = frontier(history, addresses);
  if (f === Infinity || f === 0) return [];
  return addresses.filter((a) => {
    const h = history.get(a);
    return h !== undefined && !h.exhausted && h.bottom === f;
  });
}

// ---------------------------------------------------------------------------
// Fetching

export async function loadFirstPage(
  rpc: RpcClient,
  address: string,
  params: ChainParams,
  limit = FIRST_PAGE_LIMIT,
): Promise<AddressHistory> {
  return fromFirstPage(await rpc.history(address, { limit }, params), limit);
}

export async function loadOlderPage(
  rpc: RpcClient,
  address: string,
  h: AddressHistory,
  params: ChainParams,
  limit = PAGE_LIMIT,
): Promise<AddressHistory> {
  const q = olderPageQuery(h, limit);
  if (!q) return h;
  return applyOlderPage(h, await rpc.history(address, q, params), limit);
}

/**
 * Fetches all rows from `since` to the newest block: pages downward with
 * `until = minHeight − 1` while pages are full. The client rejects rows
 * outside the requested range, so every page moves strictly downward.
 */
export async function fetchRange(
  rpc: RpcClient,
  address: string,
  since: number,
  params: ChainParams,
  limit = PAGE_LIMIT,
  maxPages = 20,
): Promise<RangeResult> {
  const rows: HistoryRow[] = [];
  let newest = -1;
  let until: number | undefined;
  for (let n = 0; n < maxPages; n++) {
    const query: { limit: number; since: number; until?: number } = { limit, since };
    if (until !== undefined) query.until = until;
    const page = await rpc.history(address, query, params);
    rows.push(...page.rows);
    newest = Math.max(newest, page.maxHeight);
    const { bottom, exhausted } = pageBottom(page, limit);
    if (exhausted || bottom <= since) return { rows, maxHeight: newest };
    until = bottom - 1;
  }
  throw new RangeError('history range too large');
}

export async function refreshAddress(
  rpc: RpcClient,
  address: string,
  h: AddressHistory,
  params: ChainParams,
  head?: number,
): Promise<{ history: AddressHistory; changed: boolean }> {
  if (head !== undefined && head < h.bottom) {
    // The loaded range is now above the chain: rebuild its first page rather
    // than retaining rows that a rollback may have removed.
    return { history: await loadFirstPage(rpc, address, params), changed: true };
  }
  const since = refreshSince(h, REFRESH_OVERLAP, head);
  const result = applyRefresh(h, since, await fetchRange(rpc, address, since, params));
  // Only a fully fetched range can advance the checkpoint. Capture the head
  // supplied before the request; blocks arriving during it remain unchecked.
  if (head !== undefined) result.history.checkedHead = head;
  return result;
}
