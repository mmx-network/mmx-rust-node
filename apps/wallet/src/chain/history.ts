/**
 * History cache (docs/wallet-app.md §6.4).
 *
 * Per address the cache holds one contiguous, fully fetched height range
 * [bottom, top]. `since` and `until` are inclusive and the node always
 * completes the last block of a page, so a full page covers [minHeight, …]
 * completely and the next older page uses `until = minHeight − 1`. A page with
 * fewer rows than the limit means nothing older exists (bottom = 0).
 *
 * Across addresses, rows at or above the frontier — the highest bottom among
 * addresses that still have older rows — are complete for the whole wallet.
 *
 * Refresh re-reads from a few blocks below the top (`since`), replacing those
 * rows, so a short reorg or a lagging server behind a load balancer cannot
 * leave stale or missing rows.
 */
import type { RpcClient } from './rpc';
import type { ChainParams, HistoryRow } from './types';

export const FIRST_PAGE_LIMIT = 25;
export const PAGE_LIMIT = 1000;
export const WALLET_ROW_CAP = 20_000;
export const REFRESH_OVERLAP = 6;

export interface AddressHistory {
  /** Newest first. */
  rows: HistoryRow[];
  /** Highest height known complete. */
  top: number;
  /** Lowest height known complete (inclusive). */
  bottom: number;
  /** True when no rows exist below `bottom` (bottom is 0). */
  exhausted: boolean;
}

export type WalletHistory = ReadonlyMap<string, AddressHistory>;

function maxHeight(rows: readonly HistoryRow[]): number {
  let m = -1;
  for (const r of rows) if (r.height > m) m = r.height;
  return m;
}

function minHeight(rows: readonly HistoryRow[]): number {
  let m = Infinity;
  for (const r of rows) if (r.height < m) m = r.height;
  return m;
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

/**
 * Result of the first page for an address (no since/until). `head` is the
 * chain height observed before the request.
 */
export function fromFirstPage(rows: readonly HistoryRow[], limit: number, head: number): AddressHistory {
  const sorted = rows.slice().sort(compareRows);
  const top = Math.max(head, maxHeight(sorted));
  if (sorted.length < limit) return { rows: sorted, top, bottom: 0, exhausted: true };
  const bottom = minHeight(sorted);
  return { rows: sorted, top, bottom, exhausted: bottom === 0 };
}

/** Request for the next older page, or null if the address is exhausted. */
export function olderPageQuery(h: AddressHistory, limit = PAGE_LIMIT): { until: number; limit: number } | null {
  if (h.exhausted || h.bottom <= 0) return null;
  return { until: h.bottom - 1, limit };
}

/** Applies an older page fetched with `until = bottom − 1`. */
export function applyOlderPage(h: AddressHistory, rows: readonly HistoryRow[], limit: number): AddressHistory {
  const until = h.bottom - 1;
  const page = rows.filter((r) => r.height <= until);
  const merged = h.rows.concat(page.slice().sort(compareRows));
  if (page.length < limit) return { rows: merged, top: h.top, bottom: 0, exhausted: true };
  const bottom = minHeight(page);
  return { rows: merged, top: h.top, bottom, exhausted: bottom === 0 };
}

/** Height to refresh from: a few blocks below the top, never below the covered range. */
export function refreshSince(h: AddressHistory, overlap = REFRESH_OVERLAP): number {
  return Math.max(h.bottom, h.top - overlap + 1, 0);
}

/**
 * Replaces everything at or above `since` with `fresh` — the complete set of
 * rows in [since, head] — and reports whether anything changed.
 */
export function applyRefresh(
  h: AddressHistory,
  since: number,
  fresh: readonly HistoryRow[],
  head: number,
): { history: AddressHistory; changed: boolean } {
  const kept = h.rows.filter((r) => r.height < since);
  const replaced = h.rows.filter((r) => r.height >= since);
  const incoming = fresh.filter((r) => r.height >= since).sort(compareRows);
  const changed = !sameRows(replaced, incoming);
  return {
    history: {
      rows: incoming.concat(kept),
      top: Math.max(h.top, head, maxHeight(incoming)),
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
  head: number,
  limit = FIRST_PAGE_LIMIT,
): Promise<AddressHistory> {
  const rows = await rpc.history(address, { limit }, params);
  return fromFirstPage(rows, limit, head);
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
  const rows = await rpc.history(address, q, params);
  return applyOlderPage(h, rows, limit);
}

/**
 * Fetches all rows in [since, head]: pages downward from the head with
 * `until = minHeight − 1` while pages are full.
 */
export async function fetchRange(
  rpc: RpcClient,
  address: string,
  since: number,
  params: ChainParams,
  limit = PAGE_LIMIT,
  maxPages = 20,
): Promise<HistoryRow[]> {
  const out: HistoryRow[] = [];
  let until: number | undefined;
  for (let page = 0; page < maxPages; page++) {
    const query: { limit: number; since: number; until?: number } = { limit, since };
    if (until !== undefined) query.until = until;
    const rows = (await rpc.history(address, query, params)).filter(
      (r) => r.height >= since && (until === undefined || r.height <= until),
    );
    out.push(...rows);
    if (rows.length < limit) return out;
    const low = minHeight(rows);
    if (low <= since) return out;
    until = low - 1;
  }
  throw new RangeError('history range too large');
}

export async function refreshAddress(
  rpc: RpcClient,
  address: string,
  h: AddressHistory,
  params: ChainParams,
  head: number,
): Promise<{ history: AddressHistory; changed: boolean }> {
  const since = refreshSince(h);
  const rows = await fetchRange(rpc, address, since, params);
  return applyRefresh(h, since, rows, head);
}
