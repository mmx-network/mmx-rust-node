/**
 * Web wallet backend (docs/wallet-app.md §6.2, §6.3, §6.6). Recovery words come
 * from the user's password manager and are opened into a WASM `Vault` per
 * unlocked wallet. Public addresses are cached in localStorage; passphrase
 * wallets also keep their base recovery words there, never the passphrase or
 * signing keys. Chain data comes from the public chain server.
 *
 * Transfers are built, signed and checked locally; the only request carrying
 * a signed transfer is the broadcast after the user confirmed it. Funds of
 * the wallet's own broadcast transfers stay reserved until they are included
 * or expire (as in the C++ wallet, ECDSA_Wallet::update_from/update_cache).
 */
import {
  BackendError,
  isErrorCode,
  type AppState,
  type Backend,
  type CreateWalletRequest,
  type ErrorCode,
  type FeeLevel,
  type FingerprintRequest,
  type ImportWalletRequest,
  type LockOptions,
  type PreparedSend,
  type SendRequest,
  type UnlockRequest,
  type WalletId,
  type WalletInfo,
} from '../types';
import { checkNode, parseAddressBalances, parseChainParams, parseNodeInfo } from '../../chain/rpc';
import type { ChainParams } from '../../chain/types';
import { NATIVE_CONTRACT, normalizeAddress } from '../../lib/address';
import { MAX_ADDRESSES, WALLET_NAME_MAX } from '../../lib/appstate';
import { isFeeLevel } from '../../lib/fees';
import { MEMO_MAX_BYTES, utf8Length } from '../../lib/memo';
import { hasUnsafeCharacters, validateName } from '../../lib/text';
import { Clipboard, downloadFile, openExplorer, pickTextFile } from './browser';
import { preventSilentAccess } from './credentials';
import { RpcHttp, type FetchLike } from './http';
import { parseRpcUrl } from './rpc-url';
import { isFingerprint, LOCK_SIGNAL_KEY, MetadataStore, STORAGE_KEY, type StorageLike, type WalletRecord } from './store';
import type { VaultHandle, WasmApi } from './wasm';

/** Blocks a transfer stays valid for (same as the desktop wallet). */
export const EXPIRE_DELTA = 100;
/** Prepared transfers are discarded after this time. */
export const PREPARED_TTL_MS = 10 * 60_000;
const MAX_PREPARED = 16;
/** Parallel balance requests while preparing a transfer (same as the desktop service). */
const FETCH_WORKERS = 4;
/** Broadcast transfers remembered per wallet; far more than a wallet sends within their 100-block lifetime. */
const MAX_RESERVATIONS = 256;
/** Broadcast refusals: the node did not accept the transfer. Other errors leave the outcome open. */
const REFUSED: ReadonlySet<ErrorCode> = new Set(['tx_rejected', 'insufficient_funds', 'insufficient_fee', 'tx_expired']);

export interface StorageEventSource {
  addEventListener(type: 'storage', listener: (e: StorageEvent) => void): void;
  removeEventListener(type: 'storage', listener: (e: StorageEvent) => void): void;
}

export interface WebBackendOptions {
  loadWasm: () => Promise<WasmApi>;
  fetch: FetchLike;
  storage: StorageLike;
  /** Chain server URL (default https://rpc.mmx.network). */
  rpcUrl?: string;
  /** Source of `storage` events from other tabs. */
  events?: StorageEventSource;
  now?: () => number;
  timeoutMs?: number;
}

/**
 * A broadcast transfer of the wallet that is not included yet. Its static fee
 * (paid by `sender`) and its inputs from the wallet's own addresses are taken
 * out of fresh balances until it is included or expires.
 */
export interface Reservation {
  txid: string;
  expires: number;
  sender: string;
  /** static cost × fee ratio / 1024, in MMX. */
  senderFee: bigint;
  inputs: Array<{ address: string; contract: string; amount: bigint }>;
}

interface Prepared {
  walletId: WalletId;
  txJson: string;
  createdAt: number;
  reservation: Reservation;
}

interface Currency {
  symbol: string;
  decimals: number;
}

interface ChainSnapshot {
  paramsJson: string;
  params: ChainParams;
  height: number;
  balancesJson: string;
  currencies: Map<string, Currency>;
}

function fail(code: ConstructorParameters<typeof BackendError>[0]): never {
  throw new BackendError(code);
}

/** Maps anything thrown by the WASM module to a BackendError; messages are codes only. */
function wasmError(e: unknown): BackendError {
  if (e instanceof BackendError) return e;
  const message = e instanceof Error ? e.message : typeof e === 'string' ? e : '';
  return new BackendError(isErrorCode(message) ? message : 'internal');
}

function checkName(name: unknown): string {
  if (typeof name !== 'string' || validateName(name, WALLET_NAME_MAX)) fail('invalid_argument');
  return name.trim();
}

function checkCount(n: unknown): number {
  if (typeof n !== 'number' || !Number.isInteger(n) || n < 1 || n > MAX_ADDRESSES) fail('invalid_argument');
  return n;
}

/** Empty means no passphrase. */
function passphraseOf(p: unknown): string | null {
  if (p === undefined || p === null || p === '') return null;
  if (typeof p !== 'string') fail('invalid_argument');
  return p;
}

function feeLevelOf(level: unknown): FeeLevel {
  if (!isFeeLevel(level)) fail('invalid_argument');
  return level;
}

/** Optional memo: non-empty, at most 64 UTF-8 bytes, no control or direction characters. */
function memoOf(memo: unknown): string | undefined {
  if (memo === undefined || memo === null) return undefined;
  if (typeof memo !== 'string' || memo === '' || utf8Length(memo) > MEMO_MAX_BYTES || hasUnsafeCharacters(memo)) {
    fail('invalid_memo');
  }
  return memo;
}

function atomicOf(amount: unknown): bigint {
  if (typeof amount !== 'string' || !/^\d{1,39}$/u.test(amount)) fail('invalid_amount');
  const value = BigInt(amount);
  if (value <= 0n || value >= 1n << 128n) fail('invalid_amount');
  return value;
}

/** `error_code_e` values of a failed execution (the node's `exec_error_t.code`). */
const EXEC_INSUFFICIENT_FUNDS = 1975784893;
const EXEC_TX_EXPIRED = 327454547;

/** Error code for a dry run that failed, by the node's reason when it gives one. */
function failedExecutionCode(error: unknown): ErrorCode {
  const code = isRecord(error) ? error.code : undefined;
  if (code === EXEC_INSUFFICIENT_FUNDS) return 'insufficient_funds';
  if (code === EXEC_TX_EXPIRED) return 'tx_expired';
  return 'tx_rejected';
}

/** A non-negative atomic amount sent as a number or a decimal string; null otherwise. */
function atomicField(v: unknown): bigint | null {
  if (typeof v === 'number' && Number.isSafeInteger(v) && v >= 0) return BigInt(v);
  if (typeof v === 'string' && /^\d{1,39}$/u.test(v)) return BigInt(v);
  return null;
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

function safeInteger(v: unknown): number | null {
  return typeof v === 'number' && Number.isSafeInteger(v) && v >= 0 ? v : null;
}

/** Runs `fn` over `items` with at most `limit` in flight; stops starting new work after the first failure. */
async function mapLimit<T, R>(items: readonly T[], limit: number, fn: (item: T) => Promise<R>): Promise<R[]> {
  const results = new Array<R>(items.length);
  let next = 0;
  let failure: { error: unknown } | null = null;
  const worker = async (): Promise<void> => {
    while (!failure) {
      const i = next++;
      if (i >= items.length) return;
      try {
        results[i] = await fn(items[i]!);
      } catch (error) {
        failure ??= { error };
        return;
      }
    }
  };
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, worker));
  if (failure) throw (failure as { error: unknown }).error;
  return results;
}

export class WebBackend implements Backend {
  readonly platform = 'web' as const;

  private readonly store: MetadataStore;
  private readonly http: RpcHttp;
  private readonly clipboard = new Clipboard();
  private readonly vaults = new Map<WalletId, VaultHandle>();
  private readonly prepared = new Map<string, Prepared>();
  /**
   * Broadcast transfers per wallet, by txid, oldest first. Kept while the
   * wallet is locked (they hold no secret) so that funds stay reserved.
   */
  private readonly reserved = new Map<WalletId, Map<string, Reservation>>();
  private readonly now: () => number;
  private wasmPromise: Promise<WasmApi> | null = null;
  /** Bumped by lockAll; an unlock started before a lock does not complete. */
  private lockEpoch = 0;
  /** The WASM module crashed; it is not used again until the page is reloaded. */
  private crashed = false;

  constructor(private readonly options: WebBackendOptions) {
    this.store = new MetadataStore(options.storage);
    this.http = new RpcHttp(options.fetch, parseRpcUrl(options.rpcUrl).base, options.timeoutMs);
    this.now = options.now ?? (() => Date.now());
    // Registered first: the keys are freed before any listener hears of the lock.
    options.events?.addEventListener('storage', (e) => {
      if (isLockSignal(e)) this.lockedElsewhere();
    });
  }

  /** Starts loading the wallet core in the background. */
  preload(): void {
    void this.wasm().catch(() => undefined);
  }

  private wasm(): Promise<WasmApi> {
    if (this.crashed) return Promise.reject(new BackendError('internal'));
    if (!this.wasmPromise) {
      const p = this.options.loadWasm();
      this.wasmPromise = p;
      p.catch(() => {
        if (this.wasmPromise === p) this.wasmPromise = null;
      });
    }
    return this.wasmPromise.catch(() => fail('internal'));
  }

  /** Runs a WASM call. A crash of the module locks every wallet and disables the module. */
  private call<T>(fn: () => T): T {
    if (this.crashed) fail('internal');
    try {
      return fn();
    } catch (e) {
      if (typeof WebAssembly !== 'undefined' && e instanceof WebAssembly.RuntimeError) {
        this.freeAll();
        this.crashed = true;
      }
      throw wasmError(e);
    }
  }

  // -- wallets

  private record(id: WalletId): WalletRecord {
    const record = typeof id === 'string' && isFingerprint(id) ? this.store.get(id) : undefined;
    if (!record) fail('wallet_not_found');
    return record;
  }

  private info(id: WalletId, record: WalletRecord): WalletInfo {
    return {
      id,
      name: record.name,
      fingerprint: id,
      withPassphrase: record.withPassphrase,
      locked: !this.vaults.has(id),
      canView: !!record.addresses && record.addresses.length >= record.addressCount,
      addressCount: record.addressCount,
      verified: record.verified,
      ...(record.seedWords ? { hasStoredSeed: true } : {}),
    };
  }

  /** The open vault of a wallet, re-derived if another tab changed its address count. */
  private vault(id: WalletId): VaultHandle {
    const record = this.record(id);
    const vault = this.vaults.get(id);
    if (!vault) fail('locked');
    if (this.call(() => vault.addresses().length) !== record.addressCount) {
      this.call(() => vault.set_count(record.addressCount));
    }
    return vault;
  }

  private setVault(id: WalletId, vault: VaultHandle): void {
    const old = this.vaults.get(id);
    this.vaults.set(id, vault);
    if (old && old !== vault) this.freeVault(old);
  }

  private freeVault(vault: VaultHandle): void {
    try {
      vault.free();
    } catch {
      // Already released.
    }
  }

  private freeAll(): void {
    this.lockEpoch++;
    for (const vault of this.vaults.values()) this.freeVault(vault);
    this.vaults.clear();
    this.prepared.clear();
  }

  private dropPrepared(walletId: WalletId): void {
    for (const [txid, p] of this.prepared) if (p.walletId === walletId) this.prepared.delete(txid);
  }

  async loadAppState(): Promise<AppState> {
    return this.store.appState();
  }

  async saveAppState(s: AppState): Promise<void> {
    this.store.saveAppState(s);
  }

  async listWallets(): Promise<WalletInfo[]> {
    return this.store.list().map(({ id, record }) => this.info(id, record));
  }

  /** 24 new recovery words. Nothing is stored until createWallet. */
  async newRecoveryPhrase(): Promise<string> {
    const wasm = await this.wasm();
    return this.call(() => wasm.generate_mnemonic());
  }

  /** Wallet ID of new words (for the password manager username); an empty passphrase counts as none, as in createWallet. */
  async walletFingerprint(r: FingerprintRequest): Promise<string> {
    const passphrase = passphraseOf(r.passphrase);
    if (typeof r.mnemonic !== 'string') fail('invalid_mnemonic');
    const wasm = await this.wasm();
    return this.call(() => wasm.fingerprint(r.mnemonic, passphrase));
  }

  /**
   * Stores a new wallet once the user confirmed its recovery words, opens it
   * and makes it the active wallet.
   */
  async createWallet(r: CreateWalletRequest): Promise<WalletInfo> {
    return this.addWallet(r, null, true);
  }

  async importWallet(r: ImportWalletRequest): Promise<WalletInfo> {
    let expected: string | null = null;
    if (r.expectedFingerprint !== undefined) {
      if (!isFingerprint(r.expectedFingerprint)) fail('invalid_argument');
      expected = r.expectedFingerprint;
    }
    return this.addWallet(r, expected, false);
  }

  /** Opens the words, then stores the wallet (verified: its words are known to be saved). */
  private async addWallet(
    r: { name: string; mnemonic: string; passphrase?: string; addressCount: number },
    expected: string | null,
    active: boolean,
  ): Promise<WalletInfo> {
    const name = checkName(r.name);
    const count = checkCount(r.addressCount);
    const passphrase = passphraseOf(r.passphrase);
    if (typeof r.mnemonic !== 'string') fail('invalid_mnemonic');
    const epoch = this.lockEpoch;
    const wasm = await this.wasm();
    if (epoch !== this.lockEpoch) fail('locked');
    const vault = this.call(() => wasm.Vault.open(r.mnemonic, passphrase, expected, count));
    let kept = false;
    try {
      const id = this.call(() => vault.fingerprint());
      if (expected !== null && id !== expected) fail('internal');
      if (this.store.has(id)) fail('wallet_exists');
      const record: WalletRecord = {
        name,
        withPassphrase: this.call(() => vault.with_passphrase()),
        addressCount: count,
        verified: true,
        added: this.now(),
      };
      record.addresses = this.call(() => vault.addresses());
      if (record.withPassphrase) {
        record.seedWords = this.call(() => vault.mnemonic());
        if (epoch !== this.lockEpoch) fail('locked');
      }
      this.store.putWallet(id, record, { active });
      this.setVault(id, vault);
      kept = true;
      return this.info(id, record);
    } finally {
      if (!kept) this.freeVault(vault);
    }
  }

  private safeFingerprint(vault: VaultHandle): string {
    try {
      return vault.fingerprint();
    } catch {
      return '';
    }
  }

  /** The caller owns this vault and must free it in finally, within one operation. */
  private async openStored(id: WalletId, record: WalletRecord, passphrase: string | null, count: number): Promise<VaultHandle> {
    if (!passphrase) fail('passphrase_required');
    if (!record.seedWords) fail('locked');
    const words = record.seedWords;
    const epoch = this.lockEpoch;
    const wasm = await this.wasm();
    if (epoch !== this.lockEpoch) fail('locked');
    const vault = this.call(() => wasm.Vault.open(words, passphrase, id, count));
    try {
      const addresses = this.call(() => vault.addresses());
      if (record.addresses?.slice(0, count).some((address, i) => addresses[i] !== address)) fail('wallet_mismatch');
      return vault;
    } catch (e) {
      this.freeVault(vault);
      throw e;
    }
  }

  async unlockWallet(id: WalletId, r: UnlockRequest): Promise<WalletInfo> {
    const record = this.record(id);
    const passphrase = passphraseOf(r.passphrase);
    if (record.withPassphrase && record.seedWords && r.mnemonic === undefined) {
      const epoch = this.lockEpoch;
      const vault = await this.openStored(id, record, passphrase, record.addressCount);
      try {
        if (epoch !== this.lockEpoch) fail('locked');
        this.setVault(id, vault);
        return this.info(id, record);
      } catch (e) { this.freeVault(vault); throw e; }
    }
    if (r.mnemonic === undefined) {
      const vault = this.vaults.get(id);
      if (!vault) fail('locked');
      if (passphrase !== null) await this.checkPassphrase(id, vault, passphrase);
      return this.info(id, record);
    }
    if (typeof r.mnemonic !== 'string') fail('invalid_mnemonic');
    const epoch = this.lockEpoch;
    const wasm = await this.wasm();
    if (epoch !== this.lockEpoch) fail('locked');
    let vault: VaultHandle;
    try {
      vault = this.call(() => wasm.Vault.open(r.mnemonic as string, passphrase, id, record.addressCount));
    } catch (e) {
      const err = wasmError(e);
      // Without a passphrase the stored ID either matches the words or they belong to another wallet.
      if (!record.withPassphrase && (err.code === 'passphrase_required' || err.code === 'invalid_passphrase')) {
        fail('wallet_mismatch');
      }
      throw err;
    }
    if (this.safeFingerprint(vault) !== id) {
      this.freeVault(vault);
      fail('internal');
    }
    const withPassphrase = this.call(() => vault.with_passphrase());
    if (withPassphrase) {
      try {
        const addresses = this.call(() => vault.addresses());
        const seedWords = this.call(() => vault.mnemonic());
        if (epoch !== this.lockEpoch) fail('locked');
        const updated = this.store.patchWallet(id, { withPassphrase, verified: true, addresses, seedWords });
        const old = this.vaults.get(id);
        this.vaults.delete(id);
        if (old) this.freeVault(old);
        this.setVault(id, vault);
        return this.info(id, updated);
      } catch (e) { this.freeVault(vault); throw e; }
    }
    this.setVault(id, vault);
    let updated: WalletRecord = { ...record, withPassphrase, verified: true };
    try {
      updated = this.store.patchWallet(id, { withPassphrase, verified: true, addresses: this.call(() => vault.addresses()) });
    } catch (e) {
      if (e instanceof BackendError && e.code === 'wallet_not_found') {
        this.vaults.delete(id);
        this.freeVault(vault);
        throw e;
      }
      // Storage unavailable: the wallet is open; it asks to be confirmed again next time.
    }
    return this.info(id, updated);
  }

  /** Checks a passphrase against an open wallet (the recovery words never leave this call). */
  private async checkPassphrase(id: WalletId, vault: VaultHandle, passphrase: string): Promise<void> {
    const wasm = await this.wasm();
    this.stillOpen(id, vault);
    const words = this.call(() => vault.mnemonic());
    if (this.call(() => wasm.fingerprint(words, passphrase)) !== id) fail('invalid_passphrase');
  }

  async lockWallet(id: WalletId): Promise<void> {
    this.lockEpoch++;
    if (typeof id !== 'string') fail('wallet_not_found');
    const vault = this.vaults.get(id);
    this.vaults.delete(id);
    this.dropPrepared(id);
    if (vault) this.freeVault(vault);
    this.clipboard.clearPending();
    void preventSilentAccess();
    if (!vault && !this.store.has(id)) fail('wallet_not_found');
  }

  /**
   * Locks every wallet in this tab. `everywhere` (the user chose Lock) also
   * locks the other tabs; idle and page-hide locks stay local, so closing a
   * second tab does not lock the one the user works in.
   */
  async lockAll(opts?: LockOptions): Promise<void> {
    this.freeAll();
    this.clipboard.clearPending();
    void preventSilentAccess();
    if (opts?.everywhere === true) this.signalLock();
  }

  /** Tells the other tabs to lock (they receive a `storage` event; this tab does not). */
  private signalLock(): void {
    try {
      this.options.storage.setItem(LOCK_SIGNAL_KEY, `${this.now()}-${Math.random().toString(36).slice(2)}`);
    } catch {
      // Site storage blocked: there are no other tabs sharing wallets.
    }
  }

  /** Another tab chose Lock. */
  private lockedElsewhere(): void {
    this.freeAll();
    this.clipboard.clearPending();
  }

  async renameWallet(id: WalletId, name: string): Promise<void> {
    this.record(id);
    this.store.patchWallet(id, { name: checkName(name) });
  }

  async setAddressCount(id: WalletId, n: number, passphrase?: string): Promise<string[]> {
    const count = checkCount(n);
    const record = this.record(id);
    const epoch = this.lockEpoch;
    let addresses = record.addresses ?? [];
    if (count > addresses.length) {
      const temporary = record.withPassphrase && !!record.seedWords;
      const vault = temporary ? await this.openStored(id, record, passphrase ?? null, count) : this.vault(id);
      try {
        if (!temporary) this.call(() => vault.set_count(count));
        addresses = this.call(() => vault.addresses());
      } finally { if (temporary) this.freeVault(vault); }
    }
    if (epoch !== this.lockEpoch) fail('locked');
    this.store.patchWallet(id, { addressCount: count, addresses });
    return addresses.slice(0, count);
  }

  async removeWallet(id: WalletId): Promise<void> {
    this.record(id);
    const vault = this.vaults.get(id);
    this.vaults.delete(id);
    this.dropPrepared(id);
    this.reserved.delete(id);
    if (vault) this.freeVault(vault);
    this.store.removeWallet(id);
  }

  async revealMnemonic(id: WalletId, passphrase?: string): Promise<string> {
    const record = this.record(id);
    if (record.withPassphrase && record.seedWords) {
      const epoch = this.lockEpoch;
      const vault = await this.openStored(id, record, passphrase ?? null, record.addressCount);
      try {
        if (epoch !== this.lockEpoch) fail('locked');
        return this.call(() => vault.mnemonic());
      } finally { this.freeVault(vault); }
    }
    const vault = this.vault(id);
    return this.call(() => vault.mnemonic());
  }

  async getAddresses(id: WalletId): Promise<string[]> {
    const record = this.record(id);
    if (record.addresses && record.addresses.length >= record.addressCount) return record.addresses.slice(0, record.addressCount);
    const vault = this.vault(id);
    const addresses = this.call(() => vault.addresses());
    this.store.patchWallet(id, { addresses });
    return addresses;
  }

  async setActiveWallet(id: WalletId): Promise<void> {
    this.record(id);
    this.store.setActiveWallet(id);
  }

  /**
   * Notifies when another tab changed the stored wallets or app state, and
   * when it chose Lock (this tab's wallets are already locked then).
   */
  onExternalChange(listener: () => void): () => void {
    return this.onStorage((e) => {
      if (isLockSignal(e)) {
        listener();
        return;
      }
      if (e.key !== null && e.key !== STORAGE_KEY) return;
      // A wallet removed in another tab is closed here too.
      const known = new Set(this.store.list().map((w) => w.id));
      for (const [id, vault] of [...this.vaults]) {
        if (known.has(id)) continue;
        this.vaults.delete(id);
        this.dropPrepared(id);
        this.freeVault(vault);
      }
      for (const id of [...this.reserved.keys()]) if (!known.has(id)) this.reserved.delete(id);
      listener();
    });
  }

  /**
   * Notifies when another tab chose Lock, after this tab's wallets were
   * locked. The listener cleans up locally and must not call
   * `lockAll({ everywhere: true })`, so tabs do not echo the signal.
   */
  onExternalLock(listener: () => void): () => void {
    return this.onStorage((e) => {
      if (isLockSignal(e)) listener();
    });
  }

  private onStorage(handler: (e: StorageEvent) => void): () => void {
    const events = this.options.events;
    if (!events) return () => undefined;
    events.addEventListener('storage', handler);
    return () => events.removeEventListener('storage', handler);
  }

  // -- chain server

  async rpcGet(path: string): Promise<unknown> {
    return (await this.http.get(path)).value;
  }

  /**
   * Chain parameters, a synced height and the current balances of the
   * wallet's `addresses` (fresh), less the funds its broadcast transfers hold.
   */
  private async snapshot(walletId: WalletId, addresses: readonly string[]): Promise<ChainSnapshot> {
    // Inclusion is checked before balances are read: a transfer included in
    // between is then subtracted once more (safe) instead of not at all.
    const waiting = [...(this.reserved.get(walletId)?.values() ?? [])];
    const [paramsReply, nodeReply, included] = await Promise.all([
      this.http.get('/chain/info'),
      this.http.get('/node/info'),
      // A failed lookup keeps the reservation (as on desktop).
      mapLimit(waiting, FETCH_WORKERS, (r) => this.isIncluded(r.txid).catch(() => false)),
    ]);
    const params = parseChainParams(paramsReply.value);
    const node = parseNodeInfo(nodeReply.value);
    checkNode(node, params);
    this.releaseReservations(
      walletId,
      new Set(waiting.filter((_, i) => included[i]).map((r) => r.txid)),
      node.height,
    );
    const rows = await mapLimit(addresses, FETCH_WORKERS, async (address) => ({
      address,
      rows: parseAddressBalances((await this.http.get(`/address?id=${address}&limit=1000`)).value, params),
    }));
    const held = this.heldFunds(walletId);
    const balances: Array<{ address: string; contract: string; amount: string }> = [];
    const currencies = new Map<string, Currency>();
    currencies.set(NATIVE_CONTRACT, { symbol: 'MMX', decimals: params.decimals });
    for (const entry of rows) {
      for (const row of entry.rows) {
        const hold = held.get(fundsKey(entry.address, row.contract)) ?? 0n;
        const amount = row.amount > hold ? row.amount - hold : 0n;
        balances.push({ address: entry.address, contract: row.contract, amount: amount.toString() });
        if (row.native) continue;
        const known = currencies.get(row.contract);
        if (known && (known.decimals !== row.decimals || known.symbol !== row.symbol)) fail('rpc_response_invalid');
        currencies.set(row.contract, { symbol: row.symbol, decimals: row.decimals });
      }
    }
    return { paramsJson: paramsReply.text, params, height: node.height, balancesJson: JSON.stringify(balances), currencies };
  }

  /** Whether the chain server reports the transfer in a block (confirmed or failed). */
  private async isIncluded(txid: string): Promise<boolean> {
    const info = (await this.http.get(`/transaction?id=${txid}`)).value;
    return isRecord(info) && safeInteger(info.height) !== null;
  }

  /** Forgets reservations of transfers that were included or can no longer be (`expires < height`). */
  private releaseReservations(walletId: WalletId, included: ReadonlySet<string>, height: number): void {
    const list = this.reserved.get(walletId);
    if (!list) return;
    for (const [txid, r] of [...list]) if (included.has(txid) || r.expires < height) list.delete(txid);
    if (list.size === 0) this.reserved.delete(walletId);
  }

  /** Funds held by the wallet's broadcast transfers, per address and currency. */
  private heldFunds(walletId: WalletId): Map<string, bigint> {
    const held = new Map<string, bigint>();
    const add = (address: string, contract: string, amount: bigint) => {
      const key = fundsKey(address, contract);
      held.set(key, (held.get(key) ?? 0n) + amount);
    };
    for (const r of this.reserved.get(walletId)?.values() ?? []) {
      add(r.sender, NATIVE_CONTRACT, r.senderFee);
      for (const input of r.inputs) add(input.address, input.contract, input.amount);
    }
    return held;
  }

  private reserve(walletId: WalletId, r: Reservation): void {
    let list = this.reserved.get(walletId);
    if (!list) {
      list = new Map();
      this.reserved.set(walletId, list);
    }
    list.delete(r.txid);
    while (list.size >= MAX_RESERVATIONS) {
      const oldest = list.keys().next().value;
      if (oldest === undefined) break;
      list.delete(oldest);
    }
    list.set(r.txid, r);
  }

  private unreserve(walletId: WalletId, txid: string): void {
    const list = this.reserved.get(walletId);
    if (!list) return;
    list.delete(txid);
    if (list.size === 0) this.reserved.delete(walletId);
  }

  /** Throws `locked` when the wallet was locked (or re-opened) while a request was running. */
  private stillOpen(id: WalletId, vault: VaultHandle): void {
    if (this.vaults.get(id) !== vault) fail('locked');
  }

  private contractOf(contract: unknown, wasm: WasmApi): string {
    if (typeof contract !== 'string' || !this.call(() => wasm.validate_address(contract))) fail('invalid_argument');
    return contract;
  }

  async maxSendable(r: Omit<SendRequest, 'amount'>): Promise<string> {
    const addresses = await this.getAddresses(r.walletId);
    const memo = memoOf(r.memo);
    const feeLevel = feeLevelOf(r.feeLevel);
    const wasm = await this.wasm();
    const contract = this.contractOf(r.contract, wasm);
    const chain = await this.snapshot(r.walletId, addresses);
    return this.call(() => wasm.watch_max_amount(JSON.stringify(addresses), chain.balancesJson, chain.paramsJson, contract, memo ?? null, feeLevel));
  }

  async prepareSend(r: SendRequest): Promise<PreparedSend> {
    const walletId = r.walletId;
    const vault = this.vault(walletId);
    const wasm = await this.wasm();
    this.stillOpen(walletId, vault);
    const to = typeof r.to === 'string' ? normalizeAddress(r.to) : null;
    if (to === null || to === NATIVE_CONTRACT || !this.call(() => wasm.validate_address(to))) fail('invalid_address');
    const contract = this.contractOf(r.contract, wasm);
    const amount = atomicOf(r.amount);
    const memo = memoOf(r.memo);
    const feeLevel = feeLevelOf(r.feeLevel);

    const addresses = await this.getAddresses(walletId);
    this.stillOpen(walletId, vault);
    const derived = this.call(() => vault.addresses());
    if (derived.length !== addresses.length || derived.some((address, i) => addresses[i] !== address)) fail('wallet_mismatch');
    const chain = await this.snapshot(walletId, addresses);
    this.stillOpen(walletId, vault);
    const currency = chain.currencies.get(contract);
    if (!currency) fail('insufficient_funds');
    let txJson: string;
    try {
      txJson = this.call(() =>
        vault.sign_transfer(
          chain.balancesJson,
          chain.paramsJson,
          chain.height,
          to,
          contract,
          amount.toString(),
          memo ?? null,
          feeLevel,
          EXPIRE_DELTA,
        ),
      );
    } catch (e) {
      // Sending MMX: the amount and the fee come from the same balance (as on desktop).
      if (contract === NATIVE_CONTRACT && e instanceof BackendError && e.code === 'insufficient_fee') fail('insufficient_funds');
      throw e;
    }
    const tx = inspectTransfer(txJson, { to, contract, amount, memo, feeLevel, network: chain.params.network, height: chain.height });
    // Checked here, not by the chain server: nothing signed leaves the device before the user confirms.
    // sign_transfer already made sure the fee payer covers the fee (fee_payer_covered).
    try {
      this.call(() => wasm.verify_transaction(txJson, chain.paramsJson));
    } catch {
      fail('internal');
    }
    // For a plain transfer the static fee is the whole fee (the node's total_fee).
    const fee = tx.fee;
    const own = new Set(addresses);
    const reservation: Reservation = {
      txid: tx.txid,
      expires: tx.expires,
      sender: tx.sender,
      senderFee: tx.fee,
      inputs: tx.inputs.filter((input) => own.has(input.address)),
    };

    this.prunePrepared();
    while (this.prepared.size >= MAX_PREPARED) {
      const oldest = this.prepared.keys().next().value;
      if (oldest === undefined) break;
      this.prepared.delete(oldest);
    }
    this.prepared.set(tx.txid, { walletId, txJson, createdAt: this.now(), reservation });

    const prepared: PreparedSend = {
      txid: tx.txid,
      to,
      contract,
      symbol: currency.symbol,
      decimals: currency.decimals,
      amount: amount.toString(),
      fee: fee.toString(),
      expires: tx.expires,
      height: chain.height,
    };
    if (memo !== undefined) prepared.memo = memo;
    return prepared;
  }

  private prunePrepared(): void {
    const now = this.now();
    for (const [txid, p] of this.prepared) if (now - p.createdAt > PREPARED_TTL_MS) this.prepared.delete(txid);
  }

  async broadcast(walletId: WalletId, txid: string): Promise<void> {
    const key = typeof txid === 'string' ? txid.toUpperCase() : '';
    this.prunePrepared();
    const p = this.prepared.get(key);
    if (!p || p.walletId !== walletId || !this.store.has(walletId)) fail('prepared_not_found');
    // Removed before sending: a transfer is submitted at most once.
    this.prepared.delete(key);
    // Nothing is sent unless these checks pass (as on desktop), so a failure
    // here is never reported as a transfer that may have been accepted.
    const notSent = (e: unknown): never => {
      if (e instanceof BackendError && REFUSED.has(e.code)) throw e;
      fail('tx_not_sent');
    };
    // A cheap read first, so a transfer that can no longer be included is never sent.
    const height = await this.http
      .get('/node/info')
      .then((reply) => parseNodeInfo(reply.value).height)
      .catch(notSent);
    if (p.reservation.expires <= height) fail('tx_expired');
    // The node's broadcast accepts a transfer whose execution fails (it is
    // included and charged its fee), so its dry run must pass first, at no
    // more than the fee shown on review.
    const result = await this.http
      .validate(p.txJson)
      .then((reply) => reply.value)
      .catch(notSent);
    if (!isRecord(result) || typeof result.did_fail !== 'boolean') fail('tx_not_sent');
    if (result.did_fail) fail(failedExecutionCode(result.error));
    const totalFee = atomicField(result.total_fee);
    if (totalFee === null || totalFee > p.reservation.senderFee) fail('tx_not_sent');
    // Reserved before sending: when the reply is lost the node may still have accepted it.
    this.reserve(walletId, p.reservation);
    try {
      await this.http.broadcast(p.txJson);
    } catch (e) {
      if (e instanceof BackendError && REFUSED.has(e.code)) this.unreserve(walletId, p.reservation.txid);
      throw e;
    }
  }

  async discardPrepared(txid: string): Promise<void> {
    if (typeof txid === 'string') this.prepared.delete(txid.toUpperCase());
  }

  // -- platform services

  async copyText(text: string, sensitive?: boolean): Promise<void> {
    if (typeof text !== 'string') fail('invalid_argument');
    await this.clipboard.copy(text, sensitive === true);
  }

  async openExternal(url: string): Promise<void> {
    openExplorer(url);
  }

  async exportFile(suggestedName: string, contents: string): Promise<boolean> {
    if (typeof contents !== 'string') fail('invalid_argument');
    return downloadFile(typeof suggestedName === 'string' ? suggestedName : 'export.json', contents);
  }

  importFile(): Promise<string | null> {
    return pickTextFile();
  }
}

interface TransferCheck {
  to: string;
  contract: string;
  amount: bigint;
  memo: string | undefined;
  feeLevel: FeeLevel;
  network: string;
  height: number;
}

interface TransferInfo {
  txid: string;
  expires: number;
  /** static cost × fee ratio / 1024. */
  fee: bigint;
  /** Pays the fee. */
  sender: string;
  inputs: Array<{ address: string; contract: string; amount: bigint }>;
}

function inputOf(v: unknown): { address: string; contract: string; amount: bigint } {
  if (!isRecord(v) || typeof v.address !== 'string' || typeof v.contract !== 'string') fail('internal');
  if (typeof v.amount !== 'string' || !/^\d{1,39}$/u.test(v.amount)) fail('internal');
  return { address: v.address, contract: v.contract, amount: BigInt(v.amount) };
}

/**
 * Reads the signed transfer back and checks that it is exactly what was
 * requested. The JSON itself is broadcast unchanged (its nonce does not fit a
 * JavaScript number).
 */
export function inspectTransfer(txJson: string, want: TransferCheck): TransferInfo {
  let tx: unknown;
  try {
    tx = JSON.parse(txJson);
  } catch {
    fail('internal');
  }
  if (!isRecord(tx) || !Array.isArray(tx.outputs) || tx.outputs.length !== 1 || !Array.isArray(tx.inputs)) fail('internal');
  const out = tx.outputs[0] as unknown;
  const inputs = (tx.inputs as unknown[]).map(inputOf);
  const id = tx.id;
  const expires = safeInteger(tx.expires);
  const ratio = safeInteger(tx.fee_ratio);
  const cost = safeInteger(tx.static_cost);
  const maxFee = safeInteger(tx.max_fee_amount);
  if (
    typeof id !== 'string' ||
    !/^[0-9a-fA-F]{64}$/u.test(id) ||
    !isRecord(out) ||
    out.address !== want.to ||
    out.contract !== want.contract ||
    out.amount !== want.amount.toString() ||
    (out.memo ?? null) !== (want.memo ?? null) ||
    tx.network !== want.network ||
    expires !== want.height + EXPIRE_DELTA ||
    ratio !== want.feeLevel * 1024 ||
    cost === null ||
    maxFee === null ||
    typeof tx.sender !== 'string' ||
    inputs.length === 0
  ) {
    fail('internal');
  }
  return {
    txid: id.toUpperCase(),
    expires,
    fee: (BigInt(cost) * BigInt(ratio)) / 1024n,
    sender: tx.sender,
    inputs,
  };
}

function fundsKey(address: string, contract: string): string {
  return `${address}|${contract}`;
}

/** A `storage` event for the lock signal written by another tab's explicit Lock. */
function isLockSignal(e: StorageEvent): boolean {
  return e.key === LOCK_SIGNAL_KEY && e.newValue !== null;
}
