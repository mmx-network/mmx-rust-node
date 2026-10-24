/**
 * Chain server client over `backend.rpcGet`. Mirrors the validation of
 * crates/mmxwallet/src/rpc.rs: synced node on the expected network, exact
 * string amounts, decimals ≤ 18, native currency identified only by the zero
 * contract. Adds a concurrency limit, in-flight de-duplication of identical
 * GETs and a timeout.
 */
import { BackendError, isBackendError } from '../backend/types';
import { decodeAddress, NATIVE_CONTRACT } from '../lib/address';
import { MAX_ATOMIC, MAX_DECIMALS } from '../lib/amount';
import { isTxid } from '../lib/explorer';
import { sanitizeMemo, sanitizeSymbol } from '../lib/text';
import type { BalanceRow, ChainParams, HistoryRow, NodeInfo, TxInfo, TxIo, TxType } from './types';
import { TX_TYPES } from './types';

export type Transport = (path: string) => Promise<unknown>;

export interface RpcOptions {
  concurrency?: number;
  timeoutMs?: number;
}

const U32_MAX = 0xffff_ffff;

function invalid(field: string): BackendError {
  return new BackendError('rpc_response_invalid', `invalid field: ${field}`);
}

type Obj = Record<string, unknown>;

export function asObject(v: unknown, what = 'response'): Obj {
  if (typeof v !== 'object' || v === null || Array.isArray(v)) throw invalid(what);
  return v as Obj;
}

export function asArray(v: unknown, what = 'response'): unknown[] {
  if (!Array.isArray(v)) throw invalid(what);
  return v;
}

export function text(o: Obj, field: string): string {
  const v = o[field];
  if (typeof v !== 'string') throw invalid(field);
  return v;
}

export function u32(o: Obj, field: string): number {
  const v = o[field];
  if (typeof v !== 'number' || !Number.isInteger(v) || v < 0 || v > U32_MAX) throw invalid(field);
  return v;
}

export function boolean(o: Obj, field: string): boolean {
  const v = o[field];
  if (typeof v !== 'boolean') throw invalid(field);
  return v;
}

/** Exact 128-bit amount from a decimal string or a safe integer. */
export function atomic(v: unknown, field = 'amount'): bigint {
  let value: bigint | null = null;
  if (typeof v === 'string' && /^\d{1,40}$/.test(v)) value = BigInt(v);
  else if (typeof v === 'number' && Number.isSafeInteger(v) && v >= 0) value = BigInt(v);
  if (value === null || value > MAX_ATOMIC) throw invalid(field);
  return value;
}

/** Canonical (lowercase) address from a field. */
export function address(o: Obj, field: string): string {
  const v = text(o, field);
  if (!decodeAddress(v)) throw invalid(field);
  return v.toLowerCase();
}

export function decimals(o: Obj, contract: string, params: ChainParams): number {
  const d = u32(o, 'decimals');
  if (d > MAX_DECIMALS || (contract === NATIVE_CONTRACT && d !== params.decimals)) throw invalid('decimals');
  return d;
}

function txid(o: Obj, field: string): string {
  const v = text(o, field);
  if (!isTxid(v)) throw invalid(field);
  return v.toUpperCase();
}

function optionalMemo(o: Obj): string | undefined {
  const v = o.memo;
  if (v === null || v === undefined) return undefined;
  if (typeof v !== 'string') throw invalid('memo');
  return sanitizeMemo(v) || undefined;
}

/** Time in ms from `time_stamp` (ms) or `time` (seconds). */
function timeMs(o: Obj): number | undefined {
  const ts = o.time_stamp;
  if (typeof ts === 'number' && Number.isFinite(ts) && ts > 0) return Math.round(ts);
  const t = o.time;
  if (typeof t === 'number' && Number.isFinite(t) && t > 0) return Math.round(t * 1000);
  return undefined;
}

/** Native symbol is fixed; tokens use the sanitized server symbol. */
function symbolFor(o: Obj, contract: string): string {
  if (contract === NATIVE_CONTRACT) return 'MMX';
  const s = sanitizeSymbol(text(o, 'symbol'));
  if (!s) throw invalid('symbol');
  return s;
}

export function parseChainParams(v: unknown): ChainParams {
  const o = asObject(v, 'chain parameters');
  const network = text(o, 'network');
  if (!network || network.length > 64) throw invalid('network');
  const d = u32(o, 'decimals');
  if (d > MAX_DECIMALS) throw invalid('decimals');
  const ratios = asArray(o.min_fee_ratio ?? [1, 2, 3, 5, 10], 'min_fee_ratio');
  const minFeeRatio = ratios.map((r) => {
    if (typeof r !== 'number' || !Number.isInteger(r) || r < 1 || r > 1000) throw invalid('min_fee_ratio');
    return r;
  });
  const interval = o.block_interval_ms;
  return {
    network,
    decimals: d,
    blockIntervalMs: typeof interval === 'number' && interval >= 1000 && interval <= 600_000 ? interval : 10_000,
    minFeeRatio,
    minTxfee: u32(o, 'min_txfee'),
    minTxfeeIo: u32(o, 'min_txfee_io'),
    minTxfeeSign: u32(o, 'min_txfee_sign'),
    minTxfeeMemo: u32(o, 'min_txfee_memo'),
  };
}

export function parseNodeInfo(v: unknown): NodeInfo {
  const o = asObject(v, 'node info');
  return {
    height: u32(o, 'height'),
    synced: boolean(o, 'is_synced'),
    network: text(o, 'name'),
    timeMs: timeMs(o),
  };
}

/** `/address?id=` balances; NFTs are skipped, duplicates rejected. */
export function parseAddressBalances(v: unknown, params: ChainParams): BalanceRow[] {
  const o = asObject(v, 'balances');
  const rows: BalanceRow[] = [];
  const seen = new Set<string>();
  for (const item of asArray(o.balances, 'balances')) {
    const row = asObject(item, 'balance');
    const contract = address(row, 'contract');
    if (row.is_nft === true) continue;
    if (seen.has(contract)) throw invalid('duplicate balance');
    seen.add(contract);
    rows.push({
      contract,
      amount: atomic(row.amount),
      decimals: decimals(row, contract, params),
      symbol: symbolFor(row, contract),
      native: contract === NATIVE_CONTRACT,
    });
  }
  return rows;
}

/** `/balance?id=` → contract → whitelisted. */
export function parseTokenValidation(v: unknown): Map<string, boolean> {
  const out = new Map<string, boolean>();
  if (v === null) return out;
  const o = asObject(v, 'balance');
  for (const item of asArray(o.balances, 'balances')) {
    const row = asObject(item, 'balance');
    out.set(address(row, 'contract'), row.is_validated === true);
  }
  return out;
}

function txType(o: Obj): TxType {
  const t = text(o, 'type');
  if (!(TX_TYPES as readonly string[]).includes(t)) throw invalid('type');
  return t as TxType;
}

/**
 * `/address/history` rows. Pending entries (no height) are skipped — they are
 * tracked locally — as are NFT rows, which carry no currency metadata.
 */
export function parseHistory(v: unknown, params: ChainParams, expectedAddress?: string): HistoryRow[] {
  const rows: HistoryRow[] = [];
  for (const item of asArray(v, 'history')) {
    const o = asObject(item, 'history entry');
    if (o.is_pending === true || o.height === null || o.height === U32_MAX) continue;
    if (o.is_nft === true) continue;
    const contract = address(o, 'contract');
    const addr = address(o, 'address');
    if (expectedAddress && addr !== expectedAddress) throw invalid('address');
    const ms = timeMs(o);
    if (ms === undefined) throw invalid('time_stamp');
    const memo = optionalMemo(o);
    rows.push({
      txid: txid(o, 'txid'),
      height: u32(o, 'height'),
      timeMs: ms,
      type: txType(o),
      address: addr,
      contract,
      amount: atomic(o.amount),
      decimals: decimals(o, contract, params),
      symbol: symbolFor(o, contract),
      native: contract === NATIVE_CONTRACT,
      validated: o.is_validated === true,
      ...(memo ? { memo } : {}),
    });
  }
  return rows;
}

function parseIo(v: unknown, params: ChainParams): TxIo {
  const o = asObject(v, 'tx io');
  const contract = address(o, 'contract');
  const io: TxIo = { address: address(o, 'address'), contract, amount: atomic(o.amount) };
  const memo = optionalMemo(o);
  if (memo) io.memo = memo;
  if (typeof o.decimals === 'number' && o.is_nft !== true) {
    io.decimals = decimals(o, contract, params);
    io.symbol = symbolFor(o, contract);
  }
  return io;
}

/** `/transaction?id=`; null when the server does not know the transaction. */
export function parseTransaction(v: unknown, params: ChainParams): TxInfo | null {
  if (v === null || v === undefined || v === '') return null;
  const o = asObject(v, 'transaction');
  const fee = asObject(o.fee, 'fee');
  const info: TxInfo = {
    id: txid(o, 'id'),
    expires: u32(o, 'expires'),
    didFail: boolean(o, 'did_fail'),
    fee: atomic(fee.amount, 'fee'),
    inputs: asArray(o.inputs ?? [], 'inputs').map((i) => parseIo(i, params)),
    outputs: asArray(o.outputs ?? [], 'outputs').map((i) => parseIo(i, params)),
  };
  if (o.height !== null && o.height !== undefined) info.height = u32(o, 'height');
  const ms = timeMs(o);
  if (ms !== undefined) info.timeMs = ms;
  if (typeof o.message === 'string' && o.message) info.message = sanitizeMemo(o.message);
  if (typeof o.sender === 'string') info.sender = address(o, 'sender');
  return info;
}

/** Exponential backoff for polling an unreachable server. */
export class Backoff {
  private failures = 0;
  constructor(
    private readonly baseMs: number,
    private readonly maxMs: number,
  ) {}
  /** Records a failure and returns the delay before the next attempt. */
  fail(): number {
    this.failures = Math.min(this.failures + 1, 16);
    return Math.min(this.maxMs, this.baseMs * 2 ** (this.failures - 1));
  }
  reset(): void {
    this.failures = 0;
  }
  get failing(): boolean {
    return this.failures > 0;
  }
}


export class RpcClient {
  private readonly concurrency: number;
  private readonly timeoutMs: number;
  private active = 0;
  private readonly queue: Array<() => void> = [];
  private readonly inflight = new Map<string, Promise<unknown>>();
  private paramsPromise: Promise<ChainParams> | null = null;

  constructor(
    private readonly transport: Transport,
    options: RpcOptions = {},
  ) {
    this.concurrency = options.concurrency ?? 4;
    this.timeoutMs = options.timeoutMs ?? 20_000;
  }

  /** GET with de-duplication of identical in-flight paths. */
  get(path: string): Promise<unknown> {
    const existing = this.inflight.get(path);
    if (existing) return existing;
    const p = this.schedule(path).finally(() => this.inflight.delete(path));
    this.inflight.set(path, p);
    return p;
  }

  private async schedule(path: string): Promise<unknown> {
    await this.acquire();
    try {
      return await this.withTimeout(path);
    } finally {
      this.release();
    }
  }

  private async acquire(): Promise<void> {
    if (this.active < this.concurrency) {
      this.active++;
      return;
    }
    // The releasing request hands its slot over directly; `active` is unchanged.
    await new Promise<void>((resolve) => this.queue.push(resolve));
  }

  private release(): void {
    const next = this.queue.shift();
    if (next) next();
    else this.active--;
  }

  private withTimeout(path: string): Promise<unknown> {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new BackendError('rpc_timeout')), this.timeoutMs);
      let call: Promise<unknown>;
      try {
        call = Promise.resolve(this.transport(path));
      } catch (e) {
        call = Promise.reject(e);
      }
      call.then(
        (v) => {
          clearTimeout(timer);
          resolve(v);
        },
        (e: unknown) => {
          clearTimeout(timer);
          reject(normalizeTransportError(e));
        },
      );
    });
  }

  /** Chain parameters, fetched once per client. */
  params(): Promise<ChainParams> {
    if (!this.paramsPromise) {
      this.paramsPromise = this.get('/chain/info').then(parseChainParams);
      this.paramsPromise.catch(() => {
        this.paramsPromise = null;
      });
    }
    return this.paramsPromise;
  }

  async nodeInfo(): Promise<NodeInfo> {
    return parseNodeInfo(await this.get('/node/info'));
  }

  /** Current height of a synced node on the expected network. */
  async height(params: ChainParams): Promise<number> {
    const info = await this.nodeInfo();
    checkNode(info, params);
    return info.height;
  }

  async addressBalances(addr: string, params: ChainParams): Promise<BalanceRow[]> {
    return parseAddressBalances(await this.get(`/address?id=${addr}&limit=1000`), params);
  }

  async tokenValidation(addr: string): Promise<Map<string, boolean>> {
    return parseTokenValidation(await this.get(`/balance?id=${addr}`));
  }

  async history(
    addr: string,
    query: { limit: number; since?: number; until?: number },
    params: ChainParams,
  ): Promise<HistoryRow[]> {
    let path = `/address/history?id=${addr}&limit=${query.limit}`;
    if (query.since !== undefined) path += `&since=${query.since}`;
    if (query.until !== undefined) path += `&until=${query.until}`;
    return parseHistory(await this.get(path), params, addr);
  }

  /** True when the address has any history. */
  async isUsed(addr: string): Promise<boolean> {
    return asArray(await this.get(`/address/history?id=${addr}&limit=1`), 'history').length > 0;
  }

  async transaction(id: string, params: ChainParams): Promise<TxInfo | null> {
    if (!isTxid(id)) throw new BackendError('invalid_argument');
    const info = parseTransaction(await this.get(`/transaction?id=${id.toUpperCase()}`), params);
    if (info && info.id !== id.toUpperCase()) throw invalid('id');
    return info;
  }
}

export function checkNode(info: NodeInfo, params: ChainParams): void {
  if (!info.synced) throw new BackendError('rpc_not_synced');
  if (info.network !== params.network) throw new BackendError('rpc_network_mismatch');
}

function normalizeTransportError(e: unknown): BackendError {
  if (isBackendError(e)) return e;
  return new BackendError('rpc_unreachable');
}
