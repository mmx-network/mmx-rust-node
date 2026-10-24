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
  PAGE_LIMIT,
  refreshAddress,
  totalRows,
  WALLET_ROW_CAP,
  type AddressHistory,
} from '../chain/history';
import { nextStatus, pendingOutgoing, prunePending, type PendingTx } from '../chain/pending';
import { Backoff, checkNode, RpcClient } from '../chain/rpc';
import { buildSeries, coverageTarget, estimateTime, type BalanceSeries, type SeriesRange } from '../chain/series';
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
  head?: number;
  headTimeMs: number;
  lastSeenHeight?: number;
  lastSyncAt: number;
  lastFullBalanceAt: number;
  needsSync: boolean;
  syncing: boolean;
  syncAgain: boolean;
  syncTimer: ReturnType<typeof setTimeout> | null;
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
const verifiedContracts = computed(() => new Set(verifiedAssets.value.map((a) => a.contract)));

/** First pages of all addresses are loaded. */
export const historyLoaded = computed(() => frontier(historyMap.value, addresses.value) !== Infinity);
export const balancesLoaded = computed(() => assets.value !== null);

const completeHistoryRows = computed(() => completeRows(historyMap.value, addresses.value));
const historyItems = computed(() => groupRows(completeHistoryRows.value, (c) => verifiedContracts.value.has(c)));
export const walletPending = computed(() => pendingTxs.value.filter((p) => p.walletId === sessionWalletId.value));
/** All activity items, newest first (local sends on top). */
export const activity = computed<ActivityItem[]>(() => mergeActivity(historyItems.value, walletPending.value, ownAddressSet.value));
export const filteredActivity = computed(() => filterItems(activity.value, activityFilter.value));
export const visibleActivity = computed(() => filteredActivity.value.slice(0, activityLimit.value));
export const activityHasMore = computed(
  () => filteredActivity.value.length > activityLimit.value || canExtend(historyMap.value, addresses.value),
);
export const recentActivity = computed(() => activity.value.slice(0, 5));
/** Pending outgoing per contract (MMX includes fees). */
export const pendingOutgoingTotals = computed(() => pendingOutgoing(walletPending.value, NATIVE_CONTRACT));

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
    headTimeMs: 0,
    lastSyncAt: 0,
    lastFullBalanceAt: 0,
    needsSync: true,
    syncing: false,
    syncAgain: false,
    syncTimer: null,
    synced: false,
    lock: Promise.resolve(),
  };
  current = s;
  batch(() => {
    addresses.value = s.addresses.slice();
    balanceMap.value = null;
    historyMap.value = new Map();
    chainError.value = null;
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

/** Clears cached transaction details and local sends of the given wallets (on lock). */
export function forgetWalletData(walletIds: ReadonlySet<string> | 'all'): void {
  txCache.clear();
  batch(() => {
    txCacheVersion.value++;
    pendingTxs.value = walletIds === 'all' ? [] : pendingTxs.value.filter((p) => !walletIds.has(p.walletId));
  });
}

function withLock<T>(s: Session, fn: () => Promise<T>): Promise<T> {
  const run = s.lock.then(fn);
  s.lock = run.catch(() => undefined);
  return run;
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
        const head = s.head;
        const [pages, balances] = await Promise.all([
          Promise.all(added.map((a) => loadFirstPage(rpc(), a, p, head))),
          fetchBalances(rpc(), added, p),
        ]);
        if (s !== current) return;
        added.forEach((a, i) => s.history.set(a, pages[i]!));
        for (const [a, b] of balances) s.balances.set(a, b);
      } catch {
        s.needsSync = true;
      }
    }
    if (s !== current) return;
    const keep = new Set(next);
    for (const a of [...s.history.keys()]) if (!keep.has(a)) s.history.delete(a);
    for (const a of [...s.balances.keys()]) if (!keep.has(a)) s.balances.delete(a);
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

function requestSync(s: Session, force = false): void {
  if (s !== current) return;
  const wait = force ? 0 : s.lastSyncAt + MIN_SYNC_INTERVAL_MS - Date.now();
  if (wait <= 0) {
    if (s.syncTimer) {
      clearTimeout(s.syncTimer);
      s.syncTimer = null;
    }
    void runSync(s);
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

async function runSync(s: Session): Promise<void> {
  if (s.syncing) {
    s.syncAgain = true;
    return;
  }
  const head = s.head;
  if (head === undefined) return;
  s.syncing = true;
  const startedAt = Date.now();
  try {
    const p = await ensureParams();
    const before = new Set(historyItems.peek().map((i) => i.key));
    const wasSynced = s.synced;
    await withLock(s, async () => {
      const changed = new Set<string>();
      const historyTask = firstError(
        s.addresses.map(async (a) => {
          const h = s.history.get(a);
          if (!h) {
            const fresh = await loadFirstPage(rpc(), a, p, head);
            if (s === current) {
              s.history.set(a, fresh);
              changed.add(a);
            }
            return;
          }
          let result: { history: AddressHistory; changed: boolean };
          try {
            result = await refreshAddress(rpc(), a, h, p, head);
          } catch (e) {
            // Too many new rows to page through: start this address over.
            if (!(e instanceof RangeError)) throw e;
            result = { history: await loadFirstPage(rpc(), a, p, head), changed: true };
          }
          if (s === current) {
            s.history.set(a, result.history);
            if (result.changed) changed.add(a);
          }
        }),
      );
      const full = s.lastFullBalanceAt === 0 || startedAt - s.lastFullBalanceAt >= FULL_BALANCE_INTERVAL_MS;
      if (full) {
        // Wait for both so no request mutates the session after the lock is released.
        const [history, balances] = await Promise.allSettled([historyTask, fetchBalances(rpc(), s.addresses, p)]);
        if (history.status === 'rejected') throw history.reason;
        if (balances.status === 'rejected') throw balances.reason;
        if (s !== current) return;
        s.balances = new Map(balances.value);
        s.lastFullBalanceAt = startedAt;
      } else {
        await historyTask;
        const targets = s.addresses.filter((a) => changed.has(a) || !s.balances.has(a));
        if (targets.length > 0) {
          const balances = await fetchBalances(rpc(), targets, p);
          if (s !== current) return;
          for (const [a, b] of balances) s.balances.set(a, b);
        }
      }
      if (s !== current) return;
      batch(() => {
        historyMap.value = new Map(s.history);
        balanceMap.value = new Map(s.balances);
      });
    });
    if (s !== current) return;
    await checkPending(s, head, p);
    if (s !== current) return;
    s.lastSyncAt = Date.now();
    s.needsSync = false;
    s.synced = true;
    chainError.value = null;
    if (wasSynced) announceReceived(before);
  } catch (e) {
    if (s === current) {
      s.needsSync = true;
      chainError.value = toBackendError(e).code;
    }
  } finally {
    s.syncing = false;
    if (s === current && s.syncAgain) {
      s.syncAgain = false;
      requestSync(s);
    }
  }
}

function announceReceived(before: ReadonlySet<string>): void {
  const fresh = historyItems.peek().filter((i) => !before.has(i.key) && i.kind === 'received' && i.primary?.verified);
  if (fresh.length === 1) {
    const a = fresh[0]!.primary!;
    notify(`Received ${formatAmount(a.amount, a.decimals)} ${a.symbol}`, 'success');
  } else if (fresh.length > 1) {
    notify(`Received ${fresh.length} transfers`, 'success');
  }
}

/** Follows locally tracked sends of the session's wallet. */
async function checkPending(s: Session, head: number, p: ChainParams): Promise<void> {
  const open = pendingTxs.peek().filter((x) => x.walletId === s.walletId && x.status === 'pending');
  const updates = new Map<string, PendingTx>();
  if (open.length > 0) {
    const results = await Promise.allSettled(open.map((x) => rpc().transaction(x.txid, p)));
    if (s !== current) return;
    results.forEach((r, i) => {
      if (r.status !== 'fulfilled') return;
      const x = open[i]!;
      const next = nextStatus(x, r.value, head);
      if (next !== x) updates.set(x.txid, next);
      if (r.value) cacheTx(r.value);
    });
  }
  const historyTxids = new Set(historyItems.peek().map((i) => i.txid));
  const merged = pendingTxs.peek().map((x) => updates.get(x.txid) ?? x);
  pendingTxs.value = prunePending(merged, historyTxids);
  for (const x of updates.values()) {
    if (x.status === 'failed') notify('A transfer failed. Check Activity for details.', 'error');
    else if (x.status === 'expired') notify('A transfer expired and was not sent.', 'error');
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
      if (s !== current || done() || !canExtend(s.history, s.addresses)) return;
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

/** Fetches older history until the chart range is covered (or nothing more exists). */
async function ensureCoverage(range: SeriesRange): Promise<void> {
  const s = current;
  if (!s) return;
  const target = coverageTarget(range, Date.now());
  const done = () => {
    const f = frontier(s.history, s.addresses);
    if (f === 0) return true;
    if (f === Infinity || target === null) return false;
    return frontierTime(s.history, s.addresses, f) <= target;
  };
  if (done() || !canExtend(s.history, s.addresses)) return;
  seriesLoading.value = true;
  try {
    await extendHistory(s, done, 40);
  } catch {
    // The chart shows the covered part; the next range change retries.
  } finally {
    if (s === current) seriesLoading.value = false;
  }
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
  const stopCoverage = effect(() => {
    const range = seriesRange.value;
    if (!historyLoaded.value || !sessionWalletId.value) return;
    untracked(() => void ensureCoverage(range));
  });
  if (typeof document === 'undefined') return stopCoverage;
  const onVisible = () => {
    if (document.visibilityState === 'visible' && current) schedulePoll(0);
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
