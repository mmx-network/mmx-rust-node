/**
 * The open wallet's chain data: addresses, balances, history, activity,
 * balance series, locally tracked sends and the refresh scheduler (§6.4).
 */
import { batch, computed, effect, signal, untracked } from '@preact/signals';
import { BackendError, toBackendError, type ErrorCode } from '../backend/types';
import {
  confirmations,
  counterpartyAddress,
  filterItems,
  groupRows,
  memoFromTx,
  mergeActivity,
  repeatRecipient,
  type ActivityFilter,
  type ActivityItem,
  type ActivityStatus,
} from '../chain/activity';
import { computeAssets, fetchBalances, type AddressBalance, type AssetBalance } from '../chain/balances';
import {
  addressesToExtend,
  canExtend,
  completeRows,
  frontier,
  loadFirstPage,
  loadOlderPage,
  maxHeight,
  PAGE_LIMIT,
  REFRESH_OVERLAP,
  refreshAddress,
  totalRows,
  WALLET_ROW_CAP,
  type AddressHistory,
} from '../chain/history';
import { nextStatus, pendingOutgoing, pendingReserved, prunePending, type PendingTx } from '../chain/pending';
import { Backoff, checkNode, RpcClient } from '../chain/rpc';
import { buildSeries, coverageTarget, estimateTime, type BalanceSeries, type SeriesRange } from '../chain/series';
import { tokenVerifier } from '../chain/tokens';
import type { ChainParams, TxInfo } from '../chain/types';
import { NATIVE_CONTRACT, normalizeAddress } from '../lib/address';
import { formatAmount } from '../lib/amount';
import { errorMessage } from '../lib/errors';
import { isTxid } from '../lib/explorer';
import { Lru } from '../lib/lru';
import { activeLabels, activeWalletId, backend, contacts, sessionWalletId } from './core';
import { notify } from './toasts';

// ---------------------------------------------------------------------------
// Chain server

let rpcClient: RpcClient | null = null;

/** The chain client (created on first use, reset when the server changes). */
export function rpc(): RpcClient {
  if (!rpcClient) rpcClient = new RpcClient((path) => backend().rpcGet(path));
  return rpcClient;
}

export interface NetworkStatus {
  state: 'online' | 'connecting' | 'offline' | 'syncing';
  height?: number;
  network?: string;
  error?: ErrorCode;
}

export const network = signal<NetworkStatus>({ state: 'connecting' });
export const params = signal<ChainParams | null>(null);
/** Current chain height, when known. */
export const chainHeight = computed(() => network.value.height);
/** Wall clock in ms, advanced on every poll (for relative times and series). */
export const clock = signal(Date.now());

export async function ensureParams(): Promise<ChainParams> {
  const p = await rpc().params();
  if (params.value !== p) params.value = p;
  return p;
}

/** Forgets the server's parameters and cached data (after a server change). */
export function resetChain(): void {
  rpcClient = null;
  txCache.clear();
  batch(() => {
    params.value = null;
    network.value = { state: 'connecting' };
    txCacheVersion.value++;
  });
}

// ---------------------------------------------------------------------------
// Session

interface Session {
  walletId: string;
  addresses: string[];
  balances: Map<string, AddressBalance>;
  history: Map<string, AddressHistory>;
  /** Last successful history check per address; failed checks remain due. */
  historyCheckedAt: Map<string, number>;
  /**
   * Addresses whose history changed since their balance was last fetched.
   * Kept on the session so a failed sync cannot lose a needed refetch.
   */
  balanceDirty: Set<string>;
  /**
   * Per address, the newest history height already shown after a sync. Only
   * rows above it can be announced as received, so older rows revealed by
   * "load more", chart coverage or a newly added address never are, and rows
   * a lagging server hid for a while are not announced twice.
   */
  seenTop: Map<string, number>;
  head?: number;
  headTimeMs: number;
  lastSeenHeight?: number;
  lastSyncAt: number;
  lastFullBalanceAt: number;
  needsSync: boolean;
  syncing: boolean;
  syncAgain: boolean;
  /** The sync requested while one was running was a forced one (user refresh). */
  syncAgainForced: boolean;
  syncTimer: ReturnType<typeof setTimeout> | null;
  /** Retry delay after failed syncs (starts at the minimum sync interval). */
  syncBackoff: Backoff;
  /** No automatic sync before this time (ms) after a failure. */
  syncRetryAt: number;
  synced: boolean;
  lock: Promise<unknown>;
}

let current: Session | null = null;

export const addresses = signal<readonly string[]>([]);
export const balanceMap = signal<ReadonlyMap<string, AddressBalance> | null>(null);
export const historyMap = signal<ReadonlyMap<string, AddressHistory>>(new Map());
/** Locally tracked sends of all wallets. */
export const pendingTxs = signal<readonly PendingTx[]>([]);
/** Last failure loading chain data (null when the latest refresh succeeded). */
export const chainError = signal<ErrorCode | null>(null);
/** Older history is being fetched for "load more". */
export const activityLoadingMore = signal(false);
/** Older history is being fetched for the balance chart. */
export const seriesLoading = signal(false);

export const ACTIVITY_PAGE = 50;
export const activityFilter = signal<ActivityFilter>({ kind: 'all', contract: null });
/** Number of filtered items to show. */
export const activityLimit = signal(ACTIVITY_PAGE);
export const seriesRange = signal<SeriesRange>('1M');
export const seriesContract = signal<string>(NATIVE_CONTRACT);

export const ownAddressSet = computed(() => new Set(addresses.value));

export const assets = computed<AssetBalance[] | null>(() => {
  const p = params.value;
  const b = balanceMap.value;
  if (!p || !b) return null;
  return computeAssets(b, addresses.value, p);
});
/** MMX balance entry (always present once balances are loaded). */
export const nativeAsset = computed(() => assets.value?.find((a) => a.native) ?? null);
/** MMX and whitelisted tokens. */
export const verifiedAssets = computed(() => assets.value?.filter((a) => a.verified) ?? []);
/** Tokens not on the whitelist ("Other tokens"). */
export const otherAssets = computed(() => assets.value?.filter((a) => !a.verified) ?? []);
/** Whether a contract is native MMX or a verified token on the server's network. */
const isVerified = computed(() => tokenVerifier(params.value?.network));

/** Lowest height down to which activity is complete for the whole wallet (Infinity until loaded). */
const historyFrontier = computed(() => frontier(historyMap.value, addresses.value));
/** First pages of all addresses are loaded. */
export const historyLoaded = computed(() => historyFrontier.value !== Infinity);
export const balancesLoaded = computed(() => assets.value !== null);

const completeHistoryRows = computed(() => completeRows(historyMap.value, addresses.value));
const historyItems = computed(() => groupRows(completeHistoryRows.value, isVerified.value));
/**
 * Local sends of the open wallet on the server's network. Sends made on
 * another network are kept (for when that server is used again) but neither
 * shown nor deducted here: addresses are the same on every network.
 */
export const walletPending = computed(() => {
  const net = params.value?.network;
  return pendingTxs.value.filter((p) => p.walletId === sessionWalletId.value && onNetwork(p, net));
});

function onNetwork(p: PendingTx, net: string | undefined): boolean {
  return net === undefined || p.network === undefined || p.network === net;
}
/** All activity items, newest first (local sends on top). */
export const activity = computed<ActivityItem[]>(() =>
  mergeActivity(historyItems.value, walletPending.value, ownAddressSet.value, isVerified.value),
);
export const filteredActivity = computed(() => filterItems(activity.value, activityFilter.value));
export const visibleActivity = computed(() => filteredActivity.value.slice(0, activityLimit.value));
export const activityHasMore = computed(
  () => filteredActivity.value.length > activityLimit.value || canExtend(historyMap.value, addresses.value),
);
export const recentActivity = computed(() => activity.value.slice(0, 5));
/** What open sends will take out of the wallet, per contract (MMX includes fees; moves count only the fee). */
export const pendingOutgoingTotals = computed(() => pendingOutgoing(walletPending.value, NATIVE_CONTRACT, ownAddressSet.value));
/**
 * What open sends hold back from spending until they confirm, per contract
 * (amount plus fee, moves to own addresses included). Subtract it from the
 * balance for the amount that can be sent now.
 */
export const pendingReservedTotals = computed(() => pendingReserved(walletPending.value, NATIVE_CONTRACT));

export interface AddressRow {
  index: number;
  address: string;
  label?: string;
  /** MMX balance. */
  native: bigint;
  /** Holds any balance (MMX or tokens). */
  hasFunds: boolean;
}

export const addressRows = computed<AddressRow[]>(() => {
  const b = balanceMap.value;
  const labels = activeLabels.value;
  return addresses.value.map((address, index) => {
    const rows = b?.get(address)?.rows ?? [];
    const row: AddressRow = {
      index,
      address,
      native: rows.find((r) => r.native)?.amount ?? 0n,
      hasFunds: rows.some((r) => r.amount > 0n),
    };
    const label = labels[index];
    if (label) row.label = label;
    return row;
  });
});

export const balanceSeries = computed<BalanceSeries | null>(() => {
  const p = params.value;
  const list = assets.value;
  if (!p || !list || !historyLoaded.value) return null;
  const h = historyMap.value;
  const addrs = addresses.value;
  const contract = seriesContract.value;
  const f = frontier(h, addrs);
  const input = {
    rows: completeHistoryRows.value,
    contract,
    current: list.find((a) => a.contract === contract)?.total ?? 0n,
    now: clock.value,
    range: seriesRange.value,
    historyComplete: f === 0,
  };
  return buildSeries(f === 0 ? input : { ...input, frontierTimeMs: frontierTime(h, addrs, f) });
});

function frontierTime(h: ReadonlyMap<string, AddressHistory>, addrs: readonly string[], f: number): number {
  for (const a of addrs) {
    const rows = h.get(a)?.rows;
    const row = rows?.find((r) => r.height === f);
    if (row) return row.timeMs;
  }
  const s = current;
  const p = params.value;
  return estimateTime(f, s?.head ?? f, s?.headTimeMs || Date.now(), p?.blockIntervalMs ?? 10_000);
}

let openGeneration = 0;

/**
 * Opens the data session for an unlocked wallet and makes it active.
 * Resolves false when a later call superseded this one.
 */
export async function openSession(walletId: string): Promise<boolean> {
  const generation = ++openGeneration;
  const raw = await backend().getAddresses(walletId);
  if (generation !== openGeneration) return false;
  const list = raw.map((a) => normalizeAddress(a));
  if (list.length === 0 || list.some((a) => a === null)) throw new BackendError('internal', 'invalid address list');
  stopSession();
  const s: Session = {
    walletId,
    addresses: list as string[],
    balances: new Map(),
    history: new Map(),
    historyCheckedAt: new Map(),
    balanceDirty: new Set(),
    seenTop: new Map(),
    headTimeMs: 0,
    lastSyncAt: 0,
    lastFullBalanceAt: 0,
    needsSync: true,
    syncing: false,
    syncAgain: false,
    syncAgainForced: false,
    syncTimer: null,
    syncBackoff: new Backoff(MIN_SYNC_INTERVAL_MS, MAX_SYNC_RETRY_MS),
    syncRetryAt: 0,
    synced: false,
    lock: Promise.resolve(),
  };
  current = s;
  batch(() => {
    addresses.value = s.addresses.slice();
    balanceMap.value = null;
    historyMap.value = new Map();
    chainError.value = null;
    // Requests of the previous wallet may still run; they no longer own these flags.
    activityLoadingMore.value = false;
    seriesLoading.value = false;
    activityFilter.value = { kind: 'all', contract: null };
    activityLimit.value = ACTIVITY_PAGE;
    seriesContract.value = NATIVE_CONTRACT;
    activeWalletId.value = walletId;
    sessionWalletId.value = walletId;
  });
  startPolling();
  return true;
}

function stopSession(): void {
  const s = current;
  current = null;
  if (s?.syncTimer) clearTimeout(s.syncTimer);
  stopPolling();
}

/** Closes the data session (wallet locked, switched or removed). */
export function closeSession(): void {
  openGeneration++;
  stopSession();
  batch(() => {
    sessionWalletId.value = null;
    addresses.value = [];
    balanceMap.value = null;
    historyMap.value = new Map();
    chainError.value = null;
    activityLoadingMore.value = false;
    seriesLoading.value = false;
  });
}

/**
 * Clears cached transaction details and the local sends of the given wallets.
 * On lock (the default) sends still on their way are kept: the server does not
 * report them, so dropping them would hide a just-sent or unconfirmed transfer
 * after unlocking and invite sending it twice. They stay hidden while the
 * wallet is locked and are followed again once it is open. Removing a wallet
 * passes `keepPending: false` to drop everything.
 */
export function forgetWalletData(walletIds: ReadonlySet<string> | 'all', options: { keepPending?: boolean } = {}): void {
  const keepPending = options.keepPending ?? true;
  txCache.clear();
  batch(() => {
    txCacheVersion.value++;
    pendingTxs.value = pendingTxs.value.filter(
      (p) => (keepPending && p.status === 'pending') || (walletIds !== 'all' && !walletIds.has(p.walletId)),
    );
  });
}

function withLock<T>(s: Session, fn: () => Promise<T>): Promise<T> {
  const run = s.lock.then(fn);
  s.lock = run.catch(() => undefined);
  return run;
}

/** Notes the newest rows of an address as already shown (see Session.seenTop). */
function markSeen(s: Session, address: string): void {
  const h = s.history.get(address);
  if (!h) return;
  s.seenTop.set(address, Math.max(s.seenTop.get(address) ?? -1, maxHeight(h.rows)));
}

/**
 * Pages newly loaded addresses down to `floor` (the wallet's frontier before
 * they were added) so adding them does not cut the activity short. Stops at
 * the wallet row cap; a failed page keeps what was loaded, which is still
 * contiguous. Returns null when the session was replaced meanwhile.
 */
async function pageDownTo(
  s: Session,
  added: readonly string[],
  pages: readonly AddressHistory[],
  floor: number,
  p: ChainParams,
): Promise<AddressHistory[] | null> {
  const out = pages.slice();
  for (;;) {
    const open = out.flatMap((h, i) => (!h.exhausted && h.bottom > floor ? [i] : []));
    const rows = totalRows(s.history, s.addresses) + out.reduce((n, h) => n + h.rows.length, 0);
    if (open.length === 0 || rows >= WALLET_ROW_CAP) return out;
    const results = await Promise.allSettled(open.map((i) => loadOlderPage(rpc(), added[i]!, out[i]!, p)));
    if (s !== current) return null;
    let failed = false;
    results.forEach((r, k) => {
      if (r.status === 'fulfilled') out[open[k]!] = r.value;
      else failed = true;
    });
    if (failed) return out;
  }
}

/**
 * Applies a new address list (address count changed): data for new addresses
 * is loaded before the list is published so the activity stays complete.
 */
export async function applyAddresses(raw: readonly string[]): Promise<void> {
  const s = current;
  if (!s) return;
  const list = raw.map((a) => normalizeAddress(a));
  if (list.length === 0 || list.some((a) => a === null)) throw new BackendError('internal', 'invalid address list');
  const next = list as string[];
  await withLock(s, async () => {
    const added = next.filter((a) => !s.history.has(a));
    if (added.length > 0 && s.head === undefined) s.needsSync = true;
    if (added.length > 0 && s.head !== undefined) {
      try {
        const p = await ensureParams();
        const floor = frontier(s.history, s.addresses);
        const [firstPages, balances] = await Promise.all([
          Promise.all(added.map((a) => loadFirstPage(rpc(), a, p))),
          fetchBalances(rpc(), added, p),
        ]);
        if (s !== current) return;
        const pages = await pageDownTo(s, added, firstPages, floor, p);
        if (!pages) return;
        added.forEach((a, i) => {
          s.history.set(a, pages[i]!);
          s.historyCheckedAt.set(a, Date.now());
          markSeen(s, a);
        });
        for (const [a, b] of balances) {
          s.balances.set(a, b);
          s.balanceDirty.delete(a);
        }
      } catch {
        s.needsSync = true;
      }
    }
    if (s !== current) return;
    const keep = new Set(next);
    for (const a of [...s.history.keys()]) if (!keep.has(a)) s.history.delete(a);
    for (const a of [...s.historyCheckedAt.keys()]) if (!keep.has(a)) s.historyCheckedAt.delete(a);
    for (const a of [...s.balances.keys()]) if (!keep.has(a)) s.balances.delete(a);
    for (const a of [...s.balanceDirty]) if (!keep.has(a)) s.balanceDirty.delete(a);
    for (const a of [...s.seenTop.keys()]) if (!keep.has(a)) s.seenTop.delete(a);
    s.addresses = next;
    batch(() => {
      addresses.value = next.slice();
      historyMap.value = new Map(s.history);
      if (balanceMap.value !== null) balanceMap.value = new Map(s.balances);
    });
  });
  if (s === current && s.needsSync) requestSync(s, true);
}

// ---------------------------------------------------------------------------
// Polling and refresh

const POLL_VISIBLE_MS = 10_000;
const POLL_HIDDEN_MS = 30_000;
const MIN_SYNC_INTERVAL_MS = 20_000;
/** Empty spare addresses need checking, but not on every active-wallet refresh. */
const IDLE_ADDRESS_INTERVAL_MS = 60_000;
/** Longest wait between automatic retries of a failing sync. */
const MAX_SYNC_RETRY_MS = 5 * 60_000;
const FULL_BALANCE_INTERVAL_MS = 5 * 60_000;

let pollTimer: ReturnType<typeof setTimeout> | null = null;
const backoff = new Backoff(POLL_VISIBLE_MS, 5 * 60_000);

function isHidden(): boolean {
  return typeof document !== 'undefined' && document.visibilityState === 'hidden';
}

function schedulePoll(ms: number): void {
  if (pollTimer !== null) clearTimeout(pollTimer);
  pollTimer = setTimeout(() => void poll(), ms);
}

function startPolling(): void {
  backoff.reset();
  network.value = { state: 'connecting' };
  schedulePoll(0);
}

function stopPolling(): void {
  if (pollTimer !== null) clearTimeout(pollTimer);
  pollTimer = null;
}

async function poll(): Promise<void> {
  pollTimer = null;
  const s = current;
  if (!s) return;
  if (isHidden()) {
    schedulePoll(POLL_HIDDEN_MS);
    return;
  }
  try {
    const p = await ensureParams();
    const info = await rpc().nodeInfo();
    if (s !== current) return;
    clock.value = Date.now();
    checkNode(info, p);
    s.head = info.height;
    s.headTimeMs = info.timeMs ?? Date.now();
    network.value = { state: 'online', height: info.height, network: p.network };
    backoff.reset();
    if (info.height !== s.lastSeenHeight || s.needsSync) {
      s.lastSeenHeight = info.height;
      requestSync(s);
    }
    schedulePoll(isHidden() ? POLL_HIDDEN_MS : POLL_VISIBLE_MS);
  } catch (e) {
    if (s !== current) return;
    const code = toBackendError(e).code;
    const prev = network.value;
    network.value = {
      state: code === 'rpc_not_synced' ? 'syncing' : 'offline',
      error: code,
      ...(prev.height !== undefined ? { height: prev.height } : {}),
      ...(prev.network !== undefined ? { network: prev.network } : {}),
    };
    schedulePoll(Math.max(backoff.fail(), isHidden() ? POLL_HIDDEN_MS : 0));
  }
}

/**
 * Runs a sync now or arms a timer for it: at most every 20 s, and after
 * failures not before the backoff allows. `force` (user refresh, address
 * change) skips both.
 */
function requestSync(s: Session, force = false): void {
  if (s !== current) return;
  const wait = force ? 0 : Math.max(s.lastSyncAt + MIN_SYNC_INTERVAL_MS, s.syncRetryAt) - Date.now();
  if (wait <= 0) {
    if (s.syncTimer) {
      clearTimeout(s.syncTimer);
      s.syncTimer = null;
    }
    void runSync(s, force);
  } else if (!s.syncTimer) {
    s.syncTimer = setTimeout(() => {
      s.syncTimer = null;
      void runSync(s);
    }, wait);
  }
}

/** Polls now and refreshes data without waiting for the next interval. */
export function refreshNow(): void {
  const s = current;
  if (!s) return;
  schedulePoll(0);
  if (s.head !== undefined) requestSync(s, true);
}

async function firstError<T>(tasks: Promise<T>[]): Promise<T[]> {
  const results = await Promise.allSettled(tasks);
  const failed = results.find((r): r is PromiseRejectedResult => r.status === 'rejected');
  if (failed) throw failed.reason;
  return results.map((r) => (r as PromiseFulfilledResult<T>).value);
}

async function runSync(s: Session, force = false): Promise<void> {
  if (s !== current) return;
  if (!force && isHidden()) {
    s.needsSync = true;
    return;
  }
  if (s.syncing) {
    // Run again when this one ends; a forced request (user refresh) then
    // skips the minimum interval and the failure delay as it would have.
    s.syncAgain = true;
    if (force) s.syncAgainForced = true;
    return;
  }
  const head = s.head;
  if (head === undefined) return;
  s.syncing = true;
  const startedAt = Date.now();
  try {
    const p = await ensureParams();
    // Sends are looked up before history and balances are read: one found in
    // a block is then already in what this sync reads and publishes.
    const updates = await lookupPending(s, head, p);
    if (s !== current) return;
    await withLock(s, async () => {
      /** Addresses with new rows found by this sync's refresh. */
      const changed = new Set<string>();
      const full = s.lastFullBalanceAt === 0 || startedAt - s.lastFullBalanceAt >= FULL_BALANCE_INTERVAL_MS;
      const ownRecipients = new Set(pendingTxs.peek()
        .filter((x) => x.walletId === s.walletId && (x.status === 'pending' || x.status === 'confirmed') && onNetwork(x, p.network))
        .map((x) => x.to));
      const targets = s.addresses.filter((a, index) => {
        if (force || full || !s.history.has(a)) return true;
        const history = s.history.get(a)!;
        const active = index === 0 || history.top >= head - REFRESH_OVERLAP ||
          s.balances.get(a)?.rows.some((row) => row.amount > 0n) || ownRecipients.has(a);
        const interval = active ? MIN_SYNC_INTERVAL_MS : IDLE_ADDRESS_INTERVAL_MS;
        return startedAt - (s.historyCheckedAt.get(a) ?? 0) >= interval;
      });
      const historyTask = firstError(
        targets.map(async (a) => {
          const h = s.history.get(a);
          if (!h) {
            const fresh = await loadFirstPage(rpc(), a, p);
            if (s === current) {
              s.history.set(a, fresh);
              s.historyCheckedAt.set(a, Date.now());
              s.balanceDirty.add(a);
            }
            return;
          }
          let result: { history: AddressHistory; changed: boolean };
          try {
            result = await refreshAddress(rpc(), a, h, p, head);
          } catch (e) {
            // Too many new rows to page through: start this address over.
            if (!(e instanceof RangeError)) throw e;
            result = { history: await loadFirstPage(rpc(), a, p), changed: true };
          }
          if (s === current) {
            s.history.set(a, result.history);
            s.historyCheckedAt.set(a, Date.now());
            if (result.changed) {
              s.balanceDirty.add(a);
              changed.add(a);
            }
          }
        }),
      );
      if (full) {
        // Wait for both so no request mutates the session after the lock is released.
        const [history, balances] = await Promise.allSettled([historyTask, fetchBalances(rpc(), s.addresses, p)]);
        if (s !== current) return;
        // A complete snapshot is kept even when history failed, so the retry
        // does not fetch every balance again; it is published with the history.
        if (balances.status === 'fulfilled') {
          s.balances = new Map(balances.value);
          s.lastFullBalanceAt = startedAt;
          // New rows the refresh found may postdate the balances read beside
          // it: those addresses are read again below.
          s.balanceDirty = changed;
        }
        if (history.status === 'rejected') throw history.reason;
        if (balances.status === 'rejected') throw balances.reason;
      } else {
        await historyTask;
      }
      const balanceTargets = s.addresses.filter((a) => s.balanceDirty.has(a) || !s.balances.has(a));
      if (balanceTargets.length > 0) {
        const balances = await fetchBalances(rpc(), balanceTargets, p);
        if (s !== current) return;
        for (const [a, b] of balances) {
          s.balances.set(a, b);
          s.balanceDirty.delete(a);
        }
      }
      if (s !== current) return;
      batch(() => {
        historyMap.value = new Map(s.history);
        balanceMap.value = new Map(s.balances);
      });
    });
    if (s !== current) return;
    settlePending(updates);
    s.lastSyncAt = Date.now();
    s.needsSync = false;
    s.synced = true;
    s.syncBackoff.reset();
    s.syncRetryAt = 0;
    chainError.value = null;
    announceReceived(s);
  } catch (e) {
    if (s === current) {
      s.needsSync = true;
      s.syncRetryAt = Date.now() + s.syncBackoff.fail();
      chainError.value = toBackendError(e).code;
    }
  } finally {
    s.syncing = false;
    if (s === current && s.syncAgain) {
      const forced = s.syncAgainForced;
      s.syncAgain = false;
      s.syncAgainForced = false;
      requestSync(s, forced);
    }
  }
}

/**
 * Announces verified transfers received since the previous successful sync:
 * items with a row above what each address had already shown (Session.seenTop),
 * then records the new tops. Addresses seen for the first time are recorded
 * without announcing, so nothing is announced on the first sync.
 */
function announceReceived(s: Session): void {
  const items = historyItems.peek();
  const fresh = items.filter((i) => {
    const height = i.height;
    if (i.kind !== 'received' || !i.primary?.verified || height === undefined) return false;
    return i.ownAddresses.some((a) => {
      const top = s.seenTop.get(a);
      return top !== undefined && height > top;
    });
  });
  for (const a of s.addresses) markSeen(s, a);
  if (fresh.length === 1) {
    const a = fresh[0]!.primary!;
    notify(`Received ${formatAmount(a.amount, a.decimals)} ${a.symbol}`, 'success');
  } else if (fresh.length > 1) {
    notify(`Received ${fresh.length} transfers`, 'success');
  }
}

/** Looks up the open sends of the session's wallet on the server's network. */
async function lookupPending(s: Session, head: number, p: ChainParams): Promise<Map<string, PendingTx>> {
  const open = pendingTxs.peek().filter((x) => x.walletId === s.walletId && x.status === 'pending' && onNetwork(x, p.network));
  const updates = new Map<string, PendingTx>();
  if (open.length === 0) return updates;
  const results = await Promise.allSettled(open.map((x) => rpc().transaction(x.txid, p)));
  if (s !== current) return updates;
  results.forEach((r, i) => {
    if (r.status !== 'fulfilled') return;
    const x = open[i]!;
    const next = nextStatus(x, r.value, head);
    if (next !== x) updates.set(x.txid, next);
    if (r.value) cacheTx(r.value);
  });
  return updates;
}

/**
 * Applies the lookups once the history and balances read after them are
 * published, drops the sends history now shows, and reports failures.
 */
function settlePending(updates: ReadonlyMap<string, PendingTx>): void {
  const historyTxids = new Set(historyItems.peek().map((i) => i.txid));
  const merged = pendingTxs.peek().map((x) => {
    const next = updates.get(x.txid);
    // Never expired while history shows it in a block.
    if (!next || (next.status === 'expired' && historyTxids.has(x.txid))) return x;
    return next;
  });
  pendingTxs.value = prunePending(merged, historyTxids);
  for (const x of updates.values()) {
    if (x.status === 'failed') notify('A transfer failed. Check Activity for details.', 'error');
    else if (x.status === 'expired' && !historyTxids.has(x.txid)) notify('A transfer expired and was not sent.', 'error');
  }
}

/** Starts tracking a broadcast transaction. */
export function trackPending(p: PendingTx): void {
  pendingTxs.value = [p, ...pendingTxs.value.filter((x) => x.txid !== p.txid)];
}

// ---------------------------------------------------------------------------
// Older history ("load more", balance chart coverage)

async function extendHistory(s: Session, done: () => boolean, maxRounds: number): Promise<void> {
  await withLock(s, async () => {
    for (let round = 0; round < maxRounds; round++) {
      if (s !== current || isHidden() || done() || !canExtend(s.history, s.addresses)) return;
      let targets = addressesToExtend(s.history, s.addresses);
      if (targets.length === 0) return;
      const room = Math.max(1, Math.floor((WALLET_ROW_CAP - totalRows(s.history, s.addresses)) / PAGE_LIMIT));
      targets = targets.slice(0, room);
      const p = await ensureParams();
      const results = await Promise.allSettled(targets.map((a) => loadOlderPage(rpc(), a, s.history.get(a)!, p)));
      if (s !== current) return;
      let failure: unknown;
      results.forEach((r, i) => {
        if (r.status === 'fulfilled') s.history.set(targets[i]!, r.value);
        else failure ??= r.reason;
      });
      historyMap.value = new Map(s.history);
      if (failure !== undefined) throw failure;
    }
  });
}

/** Shows more activity, fetching older history when needed. */
export async function loadMoreActivity(): Promise<void> {
  const s = current;
  if (!s || activityLoadingMore.peek()) return;
  const target = activityLimit.peek() + ACTIVITY_PAGE;
  activityLimit.value = target;
  if (filteredActivity.peek().length >= target) return;
  activityLoadingMore.value = true;
  try {
    await extendHistory(s, () => filteredActivity.peek().length >= target, 10);
  } catch (e) {
    if (s === current) notify(errorMessage(e), 'error');
  } finally {
    if (s === current) activityLoadingMore.value = false;
  }
}

export function setActivityFilter(filter: Partial<ActivityFilter>): void {
  batch(() => {
    activityFilter.value = { ...activityFilter.value, ...filter };
    activityLimit.value = ACTIVITY_PAGE;
  });
}

/** The coverage run in progress and the range to check again once it ends. */
let coverageRun: { session: Session; range: SeriesRange; again: SeriesRange | null } | null = null;

/**
 * Fetches older history until the chart range is covered (or nothing more
 * exists). Called whenever the range or the wallet's frontier changes; while a
 * run is in progress later calls only ask it to check again when it ends.
 */
async function ensureCoverage(range: SeriesRange): Promise<void> {
  const s = current;
  if (!s || isHidden()) return;
  if (coverageRun?.session === s) {
    coverageRun.again = range;
    return;
  }
  const target = coverageTarget(range, Date.now());
  const done = () => {
    const f = frontier(s.history, s.addresses);
    if (f === 0) return true;
    if (f === Infinity || target === null) return false;
    return frontierTime(s.history, s.addresses, f) <= target;
  };
  if (done() || !canExtend(s.history, s.addresses)) return;
  const run = { session: s, range, again: null as SeriesRange | null };
  coverageRun = run;
  seriesLoading.value = true;
  let failed = false;
  try {
    await extendHistory(s, done, 40);
  } catch {
    // The chart shows the covered part; the next range or frontier change retries.
    failed = true;
  } finally {
    if (coverageRun === run) coverageRun = null;
    if (s === current) seriesLoading.value = false;
  }
  // After a failure only a different range (a new choice) is tried right away.
  if (run.again && s === current && (!failed || run.again !== range)) await ensureCoverage(run.again);
}

export function setSeriesRange(range: SeriesRange): void {
  seriesRange.value = range;
}

export function setSeriesContract(contract: string): void {
  seriesContract.value = contract;
}

// ---------------------------------------------------------------------------
// Transaction details and counterparties

const txCache = new Lru<string, TxInfo | null>(500);
/** Bumped whenever the transaction cache changes (makes readers reactive). */
export const txCacheVersion = signal(0);
const txInflight = new Map<string, Promise<TxInfo | null>>();

function cacheTx(tx: TxInfo): void {
  txCache.set(tx.id, tx);
  txCacheVersion.value++;
}

/** Cached details: undefined = not loaded, null = unknown to the server. */
export function cachedTx(txid: string): TxInfo | null | undefined {
  void txCacheVersion.value;
  return txCache.peek(txid.toUpperCase());
}

/** Loads transaction details (de-duplicated). `force` re-fetches a cached entry. */
export function loadTx(txid: string, force = false): Promise<TxInfo | null> {
  const id = txid.toUpperCase();
  if (!isTxid(id)) return Promise.resolve(null);
  if (!force && txCache.has(id)) return Promise.resolve(txCache.get(id) ?? null);
  const existing = txInflight.get(id);
  if (existing) return existing;
  const p = (async () => {
    const params = await ensureParams();
    const info = await rpc().transaction(id, params);
    txCache.set(id, info);
    txCacheVersion.value++;
    return info;
  })().finally(() => txInflight.delete(id));
  txInflight.set(id, p);
  return p;
}

function requestTx(txid: string): void {
  if (txCache.has(txid) || txInflight.has(txid)) return;
  queueMicrotask(() => void loadTx(txid).catch(() => undefined));
}

export interface Counterparty {
  address: string;
  /** Contact name or own address label. */
  name?: string;
  source: 'contact' | 'own' | 'address';
}

/** Index of one of the wallet's addresses, or −1. */
export function ownAddressIndex(address: string): number {
  return addresses.value.indexOf(address);
}

/** How to show an address: contact name, own label, or just the address. */
export function describeAddress(address: string): Counterparty {
  const contact = contacts.value.find((c) => c.address === address);
  if (contact) return { address, name: contact.name, source: 'contact' };
  const index = ownAddressIndex(address);
  if (index >= 0) {
    const label = activeLabels.value[index];
    return label ? { address, name: label, source: 'own' } : { address, source: 'own' };
  }
  return { address, source: 'address' };
}

/**
 * The other party of an item. undefined = still loading (a lookup is
 * started), null = none (rewards, moves, unknown).
 */
export function counterpartyFor(item: ActivityItem): Counterparty | null | undefined {
  if (item.kind !== 'received' && item.kind !== 'sent' && item.kind !== 'fee') return null;
  if (item.local && item.to) return describeAddress(item.to);
  const tx = cachedTx(item.txid);
  if (tx === undefined) {
    requestTx(item.txid);
    return undefined;
  }
  if (tx === null) return null;
  const address = counterpartyAddress(item, tx, ownAddressSet.value);
  return address ? describeAddress(address) : null;
}

/**
 * Recipient for repeating a sent item as a plain transfer ("Send again").
 * undefined while transaction details load (a lookup is started), null when
 * the item cannot be repeated that way: not a send, a send to the wallet
 * itself, a contract call such as an NFT purchase, or a payment to several
 * recipients.
 */
export function repeatRecipientFor(item: ActivityItem): string | null | undefined {
  if (item.kind !== 'sent') return null;
  if (item.local) return item.to && !ownAddressSet.value.has(item.to) ? item.to : null;
  const tx = cachedTx(item.txid);
  if (tx === undefined) {
    requestTx(item.txid);
    return undefined;
  }
  if (tx === null) return null;
  return repeatRecipient(item, tx, ownAddressSet.value) ?? null;
}

/** Memo of an item, using transaction details when history lacks it. */
export function memoFor(item: ActivityItem): string | undefined {
  if (item.memo) return item.memo;
  const tx = cachedTx(item.txid);
  return tx ? memoFromTx(item, tx, ownAddressSet.value) : undefined;
}

/** Item status including failures reported by transaction details. */
export function statusFor(item: ActivityItem): ActivityStatus {
  if (item.local || item.status !== 'confirmed') return item.status;
  return cachedTx(item.txid)?.didFail ? 'failed' : 'confirmed';
}

export function confirmationsFor(item: ActivityItem): number {
  return confirmations(item, chainHeight.value);
}

/** Finds an item by key among loaded activity. */
export function activityItem(key: string): ActivityItem | undefined {
  return activity.value.find((i) => i.key === key);
}

// ---------------------------------------------------------------------------
// Effects

/** Starts visibility handling and chart coverage; returns a stop function. */
export function startSessionEffects(): () => void {
  // Follows the frontier, not just "loaded": an added address or an address
  // reloaded after a burst of activity raises it, and the chart range must be
  // covered again.
  const stopCoverage = effect(() => {
    const range = seriesRange.value;
    const f = historyFrontier.value;
    if (f === Infinity || !sessionWalletId.value) return;
    untracked(() => void ensureCoverage(range));
  });
  if (typeof document === 'undefined') return stopCoverage;
  const onVisible = () => {
    if (document.visibilityState === 'visible' && current) {
      current.needsSync = true;
      schedulePoll(0);
      void ensureCoverage(seriesRange.peek());
    }
  };
  const onOnline = () => {
    if (current) schedulePoll(0);
  };
  document.addEventListener('visibilitychange', onVisible);
  window.addEventListener('online', onOnline);
  return () => {
    stopCoverage();
    document.removeEventListener('visibilitychange', onVisible);
    window.removeEventListener('online', onOnline);
  };
}
