/**
 * Web wallet backend (docs/wallet-app.md §6.2, §6.3, §6.6). Recovery words come
 * from the user's password manager and are opened into a WASM `Vault` per
 * unlocked wallet; nothing secret is stored or sent anywhere. Metadata lives
 * in localStorage, chain data comes from the public chain server.
 */
import {
  BackendError,
  isErrorCode,
  type AppState,
  type Backend,
  type CreateWalletRequest,
  type FeeLevel,
  type ImportWalletRequest,
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
import { isFingerprint, MetadataStore, STORAGE_KEY, type StorageLike, type WalletRecord } from './store';
import type { VaultHandle, WasmApi } from './wasm';

/** Blocks a transfer stays valid for (same as the desktop wallet). */
export const EXPIRE_DELTA = 100;
/** Prepared transfers are discarded after this time. */
export const PREPARED_TTL_MS = 10 * 60_000;
const MAX_PREPARED = 16;
/** Parallel balance requests while preparing a transfer (same as the desktop service). */
const FETCH_WORKERS = 4;

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

interface Prepared {
  walletId: WalletId;
  txJson: string;
  createdAt: number;
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
      addressCount: record.addressCount,
      verified: record.verified,
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

  async createWallet(r: CreateWalletRequest): Promise<{ wallet: WalletInfo; mnemonic: string }> {
    const name = checkName(r.name);
    const count = checkCount(r.addressCount);
    const passphrase = passphraseOf(r.passphrase);
    const wasm = await this.wasm();
    const mnemonic = this.call(() => wasm.generate_mnemonic());
    // Derives the wallet ID; the wallet stays locked until it is opened with the saved credential.
    const vault = this.call(() => wasm.Vault.open(mnemonic, passphrase, null, 1));
    let id: string;
    let withPassphrase: boolean;
    try {
      id = this.call(() => vault.fingerprint());
      withPassphrase = this.call(() => vault.with_passphrase());
    } finally {
      this.freeVault(vault);
    }
    if (this.store.has(id)) fail('wallet_exists');
    const record: WalletRecord = { name, withPassphrase, addressCount: count, verified: false, added: this.now() };
    this.store.putWallet(id, record);
    return { wallet: this.info(id, record), mnemonic };
  }

  async importWallet(r: ImportWalletRequest): Promise<WalletInfo> {
    const name = checkName(r.name);
    const count = checkCount(r.addressCount);
    const passphrase = passphraseOf(r.passphrase);
    if (typeof r.mnemonic !== 'string') fail('invalid_mnemonic');
    let expected: string | null = null;
    if (r.expectedFingerprint !== undefined) {
      if (!isFingerprint(r.expectedFingerprint)) fail('invalid_argument');
      expected = r.expectedFingerprint;
    }
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
      this.store.putWallet(id, record);
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

  async unlockWallet(id: WalletId, r: UnlockRequest): Promise<WalletInfo> {
    const record = this.record(id);
    const passphrase = passphraseOf(r.passphrase);
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
    this.setVault(id, vault);
    let updated: WalletRecord = { ...record, withPassphrase, verified: true };
    try {
      updated = this.store.patchWallet(id, { withPassphrase, verified: true });
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
    if (typeof id !== 'string') fail('wallet_not_found');
    const vault = this.vaults.get(id);
    this.vaults.delete(id);
    this.dropPrepared(id);
    if (vault) this.freeVault(vault);
    void preventSilentAccess();
    if (!vault && !this.store.has(id)) fail('wallet_not_found');
  }

  async lockAll(): Promise<void> {
    this.freeAll();
    void preventSilentAccess();
  }

  async renameWallet(id: WalletId, name: string): Promise<void> {
    this.record(id);
    this.store.patchWallet(id, { name: checkName(name) });
  }

  async setAddressCount(id: WalletId, n: number): Promise<string[]> {
    const count = checkCount(n);
    const vault = this.vault(id);
    this.call(() => vault.set_count(count));
    this.store.patchWallet(id, { addressCount: count });
    return this.call(() => vault.addresses());
  }

  async removeWallet(id: WalletId): Promise<void> {
    this.record(id);
    const vault = this.vaults.get(id);
    this.vaults.delete(id);
    this.dropPrepared(id);
    if (vault) this.freeVault(vault);
    this.store.removeWallet(id);
  }

  async revealMnemonic(id: WalletId): Promise<string> {
    const vault = this.vault(id);
    return this.call(() => vault.mnemonic());
  }

  async getAddresses(id: WalletId): Promise<string[]> {
    const vault = this.vault(id);
    return this.call(() => vault.addresses());
  }

  async setActiveWallet(id: WalletId): Promise<void> {
    this.record(id);
    this.store.setActiveWallet(id);
  }

  onExternalChange(listener: () => void): () => void {
    const events = this.options.events;
    if (!events) return () => undefined;
    const handler = (e: StorageEvent) => {
      if (e.key !== null && e.key !== STORAGE_KEY) return;
      // A wallet removed in another tab is closed here too.
      const known = new Set(this.store.list().map((w) => w.id));
      for (const [id, vault] of [...this.vaults]) {
        if (known.has(id)) continue;
        this.vaults.delete(id);
        this.dropPrepared(id);
        this.freeVault(vault);
      }
      listener();
    };
    events.addEventListener('storage', handler);
    return () => events.removeEventListener('storage', handler);
  }

  // -- chain server

  async rpcGet(path: string): Promise<unknown> {
    return (await this.http.get(path)).value;
  }

  /** Chain parameters, a synced height and the current balances of `addresses` (fresh). */
  private async snapshot(addresses: readonly string[]): Promise<ChainSnapshot> {
    const [paramsReply, nodeReply] = await Promise.all([this.http.get('/chain/info'), this.http.get('/node/info')]);
    const params = parseChainParams(paramsReply.value);
    const node = parseNodeInfo(nodeReply.value);
    checkNode(node, params);
    const rows = await mapLimit(addresses, FETCH_WORKERS, async (address) => ({
      address,
      rows: parseAddressBalances((await this.http.get(`/address?id=${address}&limit=1000`)).value, params),
    }));
    const balances: Array<{ address: string; contract: string; amount: string }> = [];
    const currencies = new Map<string, Currency>();
    currencies.set(NATIVE_CONTRACT, { symbol: 'MMX', decimals: params.decimals });
    for (const entry of rows) {
      for (const row of entry.rows) {
        balances.push({ address: entry.address, contract: row.contract, amount: row.amount.toString() });
        if (row.native) continue;
        const known = currencies.get(row.contract);
        if (known && (known.decimals !== row.decimals || known.symbol !== row.symbol)) fail('rpc_response_invalid');
        currencies.set(row.contract, { symbol: row.symbol, decimals: row.decimals });
      }
    }
    return { paramsJson: paramsReply.text, params, height: node.height, balancesJson: JSON.stringify(balances), currencies };
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
    const vault = this.vault(r.walletId);
    const memo = memoOf(r.memo);
    const feeLevel = feeLevelOf(r.feeLevel);
    const wasm = await this.wasm();
    const contract = this.contractOf(r.contract, wasm);
    const chain = await this.snapshot(this.call(() => vault.addresses()));
    this.stillOpen(r.walletId, vault);
    return this.call(() => vault.max_amount(chain.balancesJson, chain.paramsJson, contract, memo ?? null, feeLevel));
  }

  async prepareSend(r: SendRequest): Promise<PreparedSend> {
    const walletId = r.walletId;
    const vault = this.vault(walletId);
    const wasm = await this.wasm();
    const to = typeof r.to === 'string' ? normalizeAddress(r.to) : null;
    if (to === null || to === NATIVE_CONTRACT || !this.call(() => wasm.validate_address(to))) fail('invalid_address');
    const contract = this.contractOf(r.contract, wasm);
    const amount = atomicOf(r.amount);
    const memo = memoOf(r.memo);
    const feeLevel = feeLevelOf(r.feeLevel);

    const chain = await this.snapshot(this.call(() => vault.addresses()));
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

    const result = (await this.http.validate(txJson)).value;
    if (!isRecord(result) || typeof result.did_fail !== 'boolean') fail('rpc_response_invalid');
    if (result.did_fail) fail('tx_rejected');
    const totalFee = atomicField(result.total_fee);
    if (totalFee === null || totalFee > tx.maxFee) fail('rpc_response_invalid');
    // Never show less than the fee the transfer pays by the chain's own rules.
    const fee = totalFee > tx.fee ? totalFee : tx.fee;

    this.stillOpen(walletId, vault);
    this.prunePrepared();
    while (this.prepared.size >= MAX_PREPARED) {
      const oldest = this.prepared.keys().next().value;
      if (oldest === undefined) break;
      this.prepared.delete(oldest);
    }
    this.prepared.set(tx.txid, { walletId, txJson, createdAt: this.now() });

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
    if (!p || p.walletId !== walletId || !this.vaults.has(walletId)) fail('prepared_not_found');
    // Removed before sending: a transfer is submitted at most once.
    this.prepared.delete(key);
    await this.http.broadcast(p.txJson);
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

function atomicField(v: unknown): bigint | null {
  if (typeof v === 'string' && /^\d{1,39}$/u.test(v)) return BigInt(v);
  const n = safeInteger(v);
  return n === null ? null : BigInt(n);
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
  maxFee: bigint;
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
  if (!isRecord(tx) || !Array.isArray(tx.outputs) || tx.outputs.length !== 1) fail('internal');
  const out = tx.outputs[0] as unknown;
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
    maxFee === null
  ) {
    fail('internal');
  }
  return {
    txid: id.toUpperCase(),
    expires,
    fee: (BigInt(cost) * BigInt(ratio)) / 1024n,
    maxFee: BigInt(maxFee),
  };
}

