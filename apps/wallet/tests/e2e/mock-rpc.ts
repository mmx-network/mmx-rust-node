/**
 * Stateful mock of the public MMX chain server for end-to-end and unit tests.
 *
 * Endpoints (as served by the C++ node's WebAPI, CORS *):
 *   GET  /node/info, /chain/info (real mainnet parameters), /address,
 *        /address/history (exact since/until/limit paging),
 *        /transaction
 *   POST /transaction/validate  checks the transfer's shape, cost, balances
 *        and fee payer; total_fee = static_cost × fee_ratio / 1024
 *   POST /transaction/broadcast records the transfer; it is included after
 *        `confirmBlocks` blocks (history rows, balances, /transaction)
 *
 * Test control (JSON bodies), see MockRpcControl in fixtures.ts:
 *   POST /__mock/reset {height?, network?, confirmBlocks?}
 *   POST /__mock/fund {address, amount, contract?, memo?, from?, type?, height?} -> {txid, height}
 *   POST /__mock/token {contract, symbol, decimals?}
 *   POST /__mock/advance {blocks?} -> {height}
 *   POST /__mock/node {synced?, network?, confirmBlocks?}
 *   POST /__mock/fail {path, method?, mode, status?, body?, after?, times?}
 *   GET  /__mock/state, /__mock/broadcasts, /__mock/requests
 *
 * Run standalone: node tests/e2e/mock-rpc.ts [--port 31700] [--block-ms 10000]
 * (no imports outside node: so Node can run this file directly).
 */
import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http';
import { pathToFileURL } from 'node:url';

// ---------------------------------------------------------------------------
// Addresses (Bech32m "mmx", 32 bytes little-endian; port of src/lib/address.ts)

const CHARSET = 'qpzry9x8gf2tvdw0s3jn54khce6mua7l';
const GENERATORS = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];
const HRP_EXPANDED = [3, 3, 3, 0, 13, 13, 24];
const BECH32M = 0x2bc830a3;
export const NATIVE = 'mmx1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqdgytev';

function polymod(values: number[]): number {
  let chk = 1;
  for (const v of values) {
    const top = chk >>> 25;
    chk = (((chk & 0x1ffffff) << 5) ^ v) >>> 0;
    for (let i = 0; i < 5; i++) if ((top >>> i) & 1) chk = (chk ^ GENERATORS[i]!) >>> 0;
  }
  return chk >>> 0;
}

export function encodeAddress(bytes: Uint8Array): string {
  const data: number[] = [];
  for (let group = 0; group < 52; group++) {
    let value = 0;
    for (let j = 0; j < 5; j++) {
      const bit = group * 5 + j;
      value = (value << 1) | (bit < 256 ? (bytes[31 - (bit >> 3)]! >> (7 - (bit & 7))) & 1 : 0);
    }
    data.push(value);
  }
  const checksum = (polymod([...HRP_EXPANDED, ...data, 0, 0, 0, 0, 0, 0]) ^ BECH32M) >>> 0;
  for (let i = 5; i >= 0; i--) data.push((checksum >>> (5 * i)) & 31);
  return 'mmx1' + data.map((v) => CHARSET[v]).join('');
}

/** True for a canonical (lowercase) address. */
export function isAddress(text: unknown): text is string {
  if (typeof text !== 'string' || text.length !== 62 || !text.startsWith('mmx1')) return false;
  const data: number[] = [];
  for (const ch of text.slice(4)) {
    const v = CHARSET.indexOf(ch);
    if (v < 0) return false;
    data.push(v);
  }
  return polymod([...HRP_EXPANDED, ...data]) === BECH32M && (data[51]! & 15) === 0;
}

/** Deterministic address for test data. */
export function testAddress(seed: string): string {
  const bytes = new Uint8Array(32);
  let h = 0x811c9dc5;
  for (let i = 0; i < 32; i++) {
    for (const ch of `${seed}|${i}`) h = Math.imul(h ^ ch.charCodeAt(0), 0x01000193) >>> 0;
    bytes[i] = h & 255;
  }
  return encodeAddress(bytes);
}

// ---------------------------------------------------------------------------
// Chain data

/** https://rpc.mmx.network/chain/info (mainnet). */
export const CHAIN_INFO: Readonly<Record<string, unknown>> = {
  __type: 'mmx.ChainParams',
  avg_proof_count: 4,
  block_interval_ms: 10000,
  challenge_delay: 6,
  challenge_interval: 256,
  commit_delay: 24,
  decimals: 6,
  escrow_binary: 'mmx1wlwl4tgfdjl5nmntd4sj77mcgfn47sapztnwaf2u2cqmzrcppvrqxhx0st',
  fixed_project_reward: 50000,
  hardfork1_height: 1000000,
  hardfork2_height: 5050000,
  infuse_delay: 1,
  initial_space_diff: 100,
  initial_time_diff: 600,
  initial_time_stamp: 1737119420000,
  max_block_cost: 1000000000,
  max_block_size: 250000000,
  max_diff_adjust: 10,
  max_ksize: 32,
  max_proof_count: 20,
  max_rcall_depth: 3,
  max_rcall_width: 5,
  max_space_fork_len: 8,
  max_tx_cost: 25000000,
  max_validators: 33,
  max_vdf_count: 100,
  min_fee_ratio: [1, 2, 3, 5, 10],
  min_ksize: 29,
  min_reward: 500000,
  min_txfee: 20000,
  min_txfee_byte: 100,
  min_txfee_depend: 10000,
  min_txfee_deploy: 20000,
  min_txfee_exec: 10000,
  min_txfee_io: 10000,
  min_txfee_memo: 5000,
  min_txfee_read: 2000,
  min_txfee_read_kbyte: 2000,
  min_txfee_sign: 10000,
  network: 'mainnet',
  nft_binary: 'mmx1hzz9tgs2dz9366t3p4ep8trmaejx7tk9al9ah3md2u37pkesa3qqfyepyw',
  offer_binary: 'mmx18rcdx8nhh56twmr2gq3h22kwj00slsn23ejan8qp00rqqw8yl4jq6ccysq',
  plot_filter: 4,
  plot_nft_binary: 'mmx1cw4nwrhceunjd82tdcauy8yeg8ccygsrx3r4kzr8v35prv482qvqdvz80z',
  port: 11337,
  post_filter: 10,
  project_ratio: { __type: 'mmx.uint_fraction_t', inverse: 100, value: 1 },
  relay_binary: 'mmx199hvcn9fzr5r9wx9jd4ppzgnerqtt0kfg6c3cgz674y60sw7kwnqznlqvp',
  reward_activation: 10000,
  reward_adjust_div: 100,
  reward_adjust_interval: 8640,
  reward_adjust_tick: 10000,
  space_diff_constant: 100000000000,
  swap_binary: 'mmx15qd3lrqm7d7jyqx3rrwqav09n8sq2xlwpehq9ydxfn8peyh367lsxfsfqd',
  target_mmx_gold_price: 2000,
  time_diff_constant: 1000000,
  time_diff_divider: 100,
  time_lock_binary: 'mmx1g9h53asl8ekmp38ejr2kpuumxz3mw5w2yth6cyvj6qjuaa4aaa3sqm5lc2',
  token_binary: 'mmx17pzl9cgmesyjur7rvvev70fx7f55rvyv7549cvrtf7chjml4qh4sryu9yl',
  transaction_activation: 20000,
  vdf_reward: 200000,
  vdf_reward_interval: 20,
  vdf_segment_size: 250000,
};

const MIN_TXFEE = 20000n;
const MIN_TXFEE_IO = 10000n;
const MIN_TXFEE_MEMO = 5000n;
const MIN_TXFEE_SIGN = 10000n;
const BLOCK_MS = 10_000;
export const DEFAULT_HEIGHT = 5_600_000;
const U32_MAX = 0xffff_ffff;

export type EntryType = 'SPEND' | 'RECEIVE' | 'TXFEE' | 'REWARD' | 'VDF_REWARD' | 'PROJECT_REWARD';
const ENTRY_TYPES: readonly EntryType[] = ['SPEND', 'RECEIVE', 'TXFEE', 'REWARD', 'VDF_REWARD', 'PROJECT_REWARD'];

interface Token {
  symbol: string;
  decimals: number;
}

interface Entry {
  height: number;
  txid: string;
  type: EntryType;
  address: string;
  contract: string;
  amount: bigint;
  memo?: string;
  counter: number;
}

interface Io {
  address: string;
  contract: string;
  amount: bigint;
  memo?: string;
}

interface TxRecord {
  id: string;
  height?: number;
  expires: number;
  didFail: boolean;
  fee: bigint;
  sender?: string;
  inputs: Io[];
  outputs: Io[];
}

/** A transfer received by /transaction/broadcast. */
export interface Broadcast {
  txid: string;
  /** Exact request body. */
  raw: string;
  status: 'pending' | 'confirmed' | 'expired';
  /** Height when it was received. */
  receivedAt: number;
  /** Height of inclusion once confirmed. */
  height?: number;
  /** Times the same transfer was submitted. */
  submissions: number;
  sender: string;
  fee: string;
  expires: number;
  inputs: Array<{ address: string; contract: string; amount: string }>;
  outputs: Array<{ address: string; contract: string; amount: string; memo?: string }>;
}

export interface FailRule {
  path: string;
  method?: string;
  /** status: reply with `status`/`body`; drop: close the connection; did_fail: validate reports a failed execution. */
  mode: 'status' | 'drop' | 'did_fail';
  status?: number;
  body?: string;
  /** Apply after the request was processed (e.g. a broadcast that was accepted but whose reply is lost). */
  after?: boolean;
  /** Number of matching requests to fail (default 1). */
  times?: number;
}

export interface MockReply {
  status: number;
  body: string;
  contentType?: string;
  /** Close the connection without a reply. */
  drop?: boolean;
}

export interface FundRequest {
  address: string;
  /** Atomic units (decimal string or safe integer). */
  amount: string | number;
  contract?: string;
  memo?: string;
  /** Sender shown for received transfers. */
  from?: string;
  type?: 'RECEIVE' | 'REWARD' | 'VDF_REWARD' | 'PROJECT_REWARD';
  /** Block height (default: current). */
  height?: number;
}

class HttpError extends Error {
  readonly status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

const bad = (message: string) => new HttpError(400, message);

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

function u32(v: unknown): number | null {
  return typeof v === 'number' && Number.isInteger(v) && v >= 0 && v <= U32_MAX ? v : null;
}

function atomic(v: unknown): bigint | null {
  if (typeof v === 'string' && /^\d{1,39}$/.test(v)) return BigInt(v);
  if (typeof v === 'number' && Number.isSafeInteger(v) && v >= 0) return BigInt(v);
  return null;
}

function hex64(seed: string): string {
  let out = '';
  let h = 0x811c9dc5;
  for (let round = 0; out.length < 64; round++) {
    for (const ch of `${seed}|${round}`) h = Math.imul(h ^ ch.charCodeAt(0), 0x01000193) >>> 0;
    out += h.toString(16).padStart(8, '0');
  }
  return out.slice(0, 64).toUpperCase();
}

function memoCost(memo: string | null | undefined): bigint {
  if (memo === null || memo === undefined) return 0n;
  const chunks = Math.max(1, Math.ceil(Buffer.byteLength(memo, 'utf8') / 32));
  return BigInt(chunks) * MIN_TXFEE_MEMO;
}

function valueOf(amount: bigint, decimals: number): number {
  return Number(amount) / 10 ** decimals;
}

/** Node order for history: (height, txid, type, contract, address, memo, amount) descending. */
function compareKey(a: Entry, b: Entry): number {
  const ka = [a.height, a.txid, a.type, a.contract, a.address, a.memo ?? '', a.amount] as const;
  const kb = [b.height, b.txid, b.type, b.contract, b.address, b.memo ?? '', b.amount] as const;
  for (let i = 0; i < ka.length; i++) {
    if (ka[i]! < kb[i]!) return 1;
    if (ka[i]! > kb[i]!) return -1;
  }
  return 0;
}

interface ParsedTx {
  id: string;
  expires: number;
  feeRatio: number;
  staticCost: number;
  maxFee: bigint;
  sender: string;
  inputs: Io[];
  outputs: Io[];
  solutions: number;
}

export interface MockOptions {
  height?: number;
  network?: string;
  confirmBlocks?: number;
  now?: () => number;
}

export class MockRpc {
  height = DEFAULT_HEIGHT;
  network = 'mainnet';
  synced = true;
  /** Blocks until a broadcast transfer is included. */
  confirmBlocks = 1;
  private now: () => number;
  private startHeight = DEFAULT_HEIGHT;
  private startTime = 0;
  private blockTimes = new Map<number, number>();
  private counter = 0;
  private tokens = new Map<string, Token>();
  private entries = new Map<string, Entry[]>();
  private balances = new Map<string, Map<string, bigint>>();
  private txs = new Map<string, TxRecord>();
  private pending = new Map<string, { tx: ParsedTx; broadcast: Broadcast }>();
  readonly broadcasts: Broadcast[] = [];
  private rules: Array<FailRule & { times: number }> = [];
  /** Chain requests: method, path with query, reply status and request body size in bytes. */
  readonly requests: Array<{ method: string; path: string; status: number; bytes: number }> = [];

  constructor(options: MockOptions = {}) {
    this.now = options.now ?? (() => Date.now());
    this.reset(options);
  }

  reset(options: MockOptions = {}): void {
    this.height = options.height ?? DEFAULT_HEIGHT;
    this.network = options.network ?? 'mainnet';
    this.confirmBlocks = options.confirmBlocks ?? 1;
    this.synced = true;
    this.startHeight = this.height;
    this.startTime = this.now();
    this.blockTimes.clear();
    this.counter = 0;
    this.tokens.clear();
    this.entries.clear();
    this.balances.clear();
    this.txs.clear();
    this.pending.clear();
    this.broadcasts.length = 0;
    this.rules = [];
    this.requests.length = 0;
  }

  /** Block time in ms: past blocks every 10 s before the start, new blocks when they were made. */
  timeOf(height: number): number {
    if (height <= this.startHeight) return this.startTime - (this.startHeight - height) * BLOCK_MS;
    return this.blockTimes.get(height) ?? this.startTime + (height - this.startHeight) * BLOCK_MS;
  }

  balance(address: string, contract = NATIVE): bigint {
    return this.balances.get(address)?.get(contract) ?? 0n;
  }

  private credit(address: string, contract: string, delta: bigint): void {
    let m = this.balances.get(address);
    if (!m) {
      m = new Map();
      this.balances.set(address, m);
    }
    const next = (m.get(contract) ?? 0n) + delta;
    if (next < 0n) throw new Error('mock balance below zero');
    if (next === 0n) m.delete(contract);
    else m.set(contract, next);
  }

  private record(e: Omit<Entry, 'counter'>): void {
    const list = this.entries.get(e.address) ?? [];
    list.push({ ...e, counter: this.counter++ });
    this.entries.set(e.address, list);
    const debit = e.type === 'SPEND' || e.type === 'TXFEE';
    this.credit(e.address, e.contract, debit ? -e.amount : e.amount);
  }

  private token(contract: string): Token {
    if (contract === NATIVE) return { symbol: 'MMX', decimals: 6 };
    const t = this.tokens.get(contract);
    if (!t) throw bad(`unknown currency ${contract}`);
    return t;
  }

  registerToken(contract: string, symbol: string, decimals = 6): void {
    if (!isAddress(contract) || contract === NATIVE) throw bad('invalid token contract');
    if (!Number.isInteger(decimals) || decimals < 0 || decimals > 18) throw bad('invalid decimals');
    this.tokens.set(contract, { symbol, decimals });
  }

  /** Credits an address (a received transfer or a reward) at the current or an earlier height. */
  fund(r: FundRequest): { txid: string; height: number } {
    if (!isAddress(r.address)) throw bad('invalid address');
    const amount = atomic(r.amount);
    if (amount === null || amount === 0n) throw bad('invalid amount');
    const contract = r.contract ?? NATIVE;
    this.token(contract);
    const type = r.type ?? 'RECEIVE';
    if (!ENTRY_TYPES.includes(type)) throw bad('invalid type');
    if (type !== 'RECEIVE' && contract !== NATIVE) throw bad('rewards are paid in MMX');
    const height = r.height ?? this.height;
    if (!Number.isInteger(height) || height < 1 || height > this.height) throw bad('invalid height');
    if (r.memo !== undefined && (typeof r.memo !== 'string' || Buffer.byteLength(r.memo, 'utf8') > 64)) {
      throw bad('invalid memo');
    }
    const txid = hex64(`fund|${this.counter}|${r.address}|${height}`);
    const memo = type === 'RECEIVE' ? r.memo : undefined;
    this.record({ height, txid, type, address: r.address, contract, amount, ...(memo ? { memo } : {}) });
    if (type === 'RECEIVE') {
      const from = r.from && isAddress(r.from) ? r.from : testAddress('mock-sender');
      const io = { address: from, contract, amount, ...(memo ? { memo } : {}) };
      this.txs.set(txid, {
        id: txid,
        height,
        expires: height + 100,
        didFail: false,
        fee: MIN_TXFEE + 2n * MIN_TXFEE_IO + MIN_TXFEE_SIGN + memoCost(memo),
        sender: from,
        inputs: [io],
        outputs: [{ ...io, address: r.address }],
      });
    }
    return { txid, height };
  }

  /** Mines `blocks` new blocks; due broadcasts are included. */
  advance(blocks = 1): number {
    if (!Number.isInteger(blocks) || blocks < 1 || blocks > 100_000) throw bad('invalid block count');
    const base = Math.max(this.now(), this.timeOf(this.height) + 1);
    for (let i = 1; i <= blocks; i++) {
      const h = this.height + 1;
      this.height = h;
      this.blockTimes.set(h, base + i);
      for (const [txid, p] of [...this.pending]) {
        if (p.broadcast.receivedAt + this.confirmBlocks <= h) this.include(txid, h);
        else if (p.tx.expires < h) {
          p.broadcast.status = 'expired';
          this.pending.delete(txid);
        }
      }
    }
    return this.height;
  }

  private include(txid: string, height: number): void {
    const p = this.pending.get(txid)!;
    this.pending.delete(txid);
    const { tx, broadcast } = p;
    const fee = BigInt(broadcast.fee);
    for (const i of tx.inputs) this.record({ height, txid, type: 'SPEND', address: i.address, contract: i.contract, amount: i.amount });
    for (const o of tx.outputs) {
      this.record({ height, txid, type: 'RECEIVE', address: o.address, contract: o.contract, amount: o.amount, ...(o.memo ? { memo: o.memo } : {}) });
    }
    this.record({ height, txid, type: 'TXFEE', address: tx.sender, contract: NATIVE, amount: fee });
    this.txs.set(txid, { id: txid, height, expires: tx.expires, didFail: false, fee, sender: tx.sender, inputs: tx.inputs, outputs: tx.outputs });
    broadcast.status = 'confirmed';
    broadcast.height = height;
  }

  /** Spendable after transfers waiting for inclusion. */
  private available(address: string, contract: string): bigint {
    let amount = this.balance(address, contract);
    for (const { tx, broadcast } of this.pending.values()) {
      for (const i of tx.inputs) if (i.address === address && i.contract === contract) amount -= i.amount;
      if (contract === NATIVE && tx.sender === address) amount -= BigInt(broadcast.fee);
    }
    return amount;
  }

  /** Node::get_history for one address (chunked scan, last block completed, sorted, truncated). */
  history(address: string, limit: number, since: number, until: number): Entry[] {
    const log = (this.entries.get(address) ?? [])
      .filter((e) => e.height >= since && e.height <= until)
      .sort((a, b) => b.height - a.height || b.counter - a.counter);
    const chunk = 100;
    const res: Entry[] = [];
    let pos = 0;
    let count = 0;
    let last: Entry | undefined;
    for (;;) {
      const part = log.slice(pos, pos + chunk);
      pos += part.length;
      res.push(...part);
      count += part.length;
      if (part.length) last = part[part.length - 1];
      if (part.length < chunk || count >= limit) break;
    }
    if (last) for (const e of log.slice(pos)) if (e.height === last.height) res.push(e);
    res.sort(compareKey);
    if (limit > 0 && res.length > limit) {
      let i = limit - 1;
      const cutoff = res[i]!.height;
      while (i < res.length && res[i]!.height === cutoff) i++;
      res.length = i;
    }
    return res;
  }

  // -- transfers

  private parseTx(body: string): ParsedTx {
    let tx: unknown;
    try {
      tx = JSON.parse(body);
    } catch {
      throw bad('invalid JSON');
    }
    if (!isRecord(tx)) throw bad('expected a transaction object');
    const fields = ['__type', 'id', 'version', 'expires', 'fee_ratio', 'static_cost', 'max_fee_amount', 'note', 'nonce', 'network', 'sender', 'inputs', 'outputs', 'execute', 'solutions', 'deploy', 'exec_result', 'content_hash'];
    for (const f of fields) if (!(f in tx)) throw bad(`missing field ${f}`);
    for (const f of Object.keys(tx)) if (!fields.includes(f)) throw bad(`unknown field ${f}`);
    if (tx.__type !== 'mmx.Transaction' || tx.version !== 1 || tx.note !== 'TRANSFER') throw bad('not a transfer');
    if (typeof tx.id !== 'string' || !/^[0-9a-f]{64}$/.test(tx.id)) throw bad('invalid id');
    if (typeof tx.content_hash !== 'string' || !/^[0-9a-f]{64}$/.test(tx.content_hash)) throw bad('invalid content hash');
    if (tx.network !== this.network) throw bad('wrong network');
    if (typeof tx.nonce !== 'number' || tx.nonce <= 0) throw bad('invalid nonce');
    const expires = u32(tx.expires);
    const feeRatio = u32(tx.fee_ratio);
    const staticCost = u32(tx.static_cost);
    const maxFee = u32(tx.max_fee_amount);
    if (expires === null || feeRatio === null || feeRatio < 1024 || staticCost === null || maxFee === null) throw bad('invalid fee fields');
    if (!isAddress(tx.sender)) throw bad('invalid sender');
    if (!Array.isArray(tx.execute) || tx.execute.length || tx.deploy !== null || tx.exec_result !== null) throw bad('not a simple transfer');
    if (!Array.isArray(tx.solutions) || tx.solutions.length === 0) throw bad('missing signatures');
    for (const s of tx.solutions) {
      if (!isRecord(s) || s.__type !== 'mmx.solution.PubKey' || typeof s.pubkey !== 'string' || !/^0[23][0-9a-f]{64}$/.test(s.pubkey) || typeof s.signature !== 'string' || !/^[0-9a-f]{128}$/.test(s.signature)) {
        throw bad('invalid signature');
      }
    }
    const io = (v: unknown, input: boolean): Io => {
      if (!isRecord(v) || !isAddress(v.address) || !isAddress(v.contract)) throw bad('invalid input/output');
      const amount = typeof v.amount === 'string' ? atomic(v.amount) : null;
      if (amount === null || amount === 0n) throw bad('invalid amount');
      if (v.memo !== null && (typeof v.memo !== 'string' || Buffer.byteLength(v.memo, 'utf8') > 64)) throw bad('invalid memo');
      if (input && (v.flags !== 0 || u32(v.solution) === null || (v.solution as number) >= (tx.solutions as unknown[]).length)) {
        throw bad('invalid input');
      }
      return { address: v.address, contract: v.contract, amount, ...(typeof v.memo === 'string' ? { memo: v.memo } : {}) };
    };
    if (!Array.isArray(tx.inputs) || tx.inputs.length === 0 || !Array.isArray(tx.outputs) || tx.outputs.length === 0) {
      throw bad('empty transfer');
    }
    const inputs = tx.inputs.map((i) => io(i, true));
    const outputs = tx.outputs.map((o) => io(o, false));
    let cost = MIN_TXFEE + BigInt(tx.solutions.length) * MIN_TXFEE_SIGN;
    for (const x of [...inputs, ...outputs]) cost += MIN_TXFEE_IO + memoCost(x.memo);
    if (cost !== BigInt(staticCost)) throw bad(`static cost mismatch: ${staticCost} != ${cost}`);
    const fee = (BigInt(staticCost) * BigInt(feeRatio)) / 1024n;
    if (fee > BigInt(maxFee)) throw bad('transaction fee > max_fee_amount');
    const sums = new Map<string, bigint>();
    for (const i of inputs) sums.set(i.contract, (sums.get(i.contract) ?? 0n) + i.amount);
    for (const o of outputs) sums.set(o.contract, (sums.get(o.contract) ?? 0n) - o.amount);
    for (const v of sums.values()) if (v !== 0n) throw bad('inputs do not match outputs');
    return { id: tx.id.toUpperCase(), expires, feeRatio, staticCost, maxFee: BigInt(maxFee), sender: tx.sender, inputs, outputs, solutions: tx.solutions.length };
  }

  /** The node's checks before execution: expiry, balances, fee payer. */
  private checkFunds(tx: ParsedTx): bigint {
    if (tx.expires < this.height) throw bad('transaction expired');
    const spend = new Map<string, bigint>();
    for (const i of tx.inputs) {
      const key = `${i.address}|${i.contract}`;
      spend.set(key, (spend.get(key) ?? 0n) + i.amount);
    }
    for (const [key, amount] of spend) {
      const [address, contract] = key.split('|') as [string, string];
      if (this.available(address, contract) < amount) throw bad(`insufficient funds for ${address}`);
    }
    const fee = (BigInt(tx.staticCost) * BigInt(tx.feeRatio)) / 1024n;
    const senderSpend = spend.get(`${tx.sender}|${NATIVE}`) ?? 0n;
    if (this.available(tx.sender, NATIVE) - senderSpend < fee) throw bad('insufficient funds for transaction fee');
    return fee;
  }

  private validate(body: string): Record<string, unknown> {
    const tx = this.parseTx(body);
    const fee = this.checkFunds(tx);
    return {
      __type: 'mmx.exec_result_t',
      did_fail: false,
      total_cost: tx.staticCost,
      total_fee: Number(fee),
      inputs: [],
      outputs: [],
      error: null,
      total_fee_value: valueOf(fee, 6),
    };
  }

  private submit(body: string): void {
    const tx = this.parseTx(body);
    const existing = this.broadcasts.find((b) => b.txid === tx.id);
    if (existing) {
      existing.submissions++;
      return;
    }
    const fee = this.checkFunds(tx);
    const broadcast: Broadcast = {
      txid: tx.id,
      raw: body,
      status: 'pending',
      receivedAt: this.height,
      submissions: 1,
      sender: tx.sender,
      fee: fee.toString(),
      expires: tx.expires,
      inputs: tx.inputs.map((i) => ({ address: i.address, contract: i.contract, amount: i.amount.toString() })),
      outputs: tx.outputs.map((o) => ({ address: o.address, contract: o.contract, amount: o.amount.toString(), ...(o.memo ? { memo: o.memo } : {}) })),
    };
    this.broadcasts.push(broadcast);
    this.pending.set(tx.id, { tx, broadcast });
  }

  // -- HTTP

  /** Handles one request; used by the HTTP server and directly by unit tests. */
  handle(method: string, url: string, body = ''): MockReply {
    const q = url.indexOf('?');
    const path = q >= 0 ? url.slice(0, q) : url;
    const params = new URLSearchParams(q >= 0 ? url.slice(q + 1) : '');
    let reply: MockReply;
    const rule = path.startsWith('/__mock/') ? undefined : this.takeRule(method, path);
    if (rule && !rule.after && rule.mode !== 'did_fail') {
      reply = rule.mode === 'drop' ? { status: 0, body: '', drop: true } : { status: rule.status ?? 500, body: rule.body ?? 'mock failure', contentType: 'text/plain' };
    } else {
      try {
        reply = this.route(method, path, params, body, rule?.mode === 'did_fail');
      } catch (e) {
        reply = e instanceof HttpError ? { status: e.status, body: e.message, contentType: 'text/plain' } : { status: 500, body: 'internal error', contentType: 'text/plain' };
      }
      if (rule?.after) {
        reply = rule.mode === 'drop' ? { status: 0, body: '', drop: true } : { status: rule.status ?? 500, body: rule.body ?? 'mock failure', contentType: 'text/plain' };
      }
    }
    if (!path.startsWith('/__mock/')) this.log(method, url, reply.drop ? 0 : reply.status, Buffer.byteLength(body));
    return reply;
  }

  /** Records a request (also CORS preflights, which never reach `handle`). */
  log(method: string, path: string, status: number, bytes = 0): void {
    this.requests.push({ method, path, status, bytes });
    if (this.requests.length > 2000) this.requests.splice(0, this.requests.length - 2000);
  }

  private takeRule(method: string, path: string): (FailRule & { times: number }) | undefined {
    const rule = this.rules.find((r) => r.path === path && (r.method === undefined || r.method === method));
    if (!rule) return undefined;
    rule.times--;
    if (rule.times <= 0) this.rules.splice(this.rules.indexOf(rule), 1);
    return rule;
  }

  private route(method: string, path: string, params: URLSearchParams, body: string, didFail: boolean): MockReply {
    const json = (v: unknown): MockReply => ({ status: 200, body: JSON.stringify(v), contentType: 'application/json' });
    if (path.startsWith('/__mock/')) return json(this.control(method, path, body));
    const uint = (name: string, fallback: number): number => {
      const v = params.get(name);
      if (v === null) return fallback;
      if (!/^\d{1,10}$/.test(v) || Number(v) > U32_MAX) throw bad(`invalid ${name}`);
      return Number(v);
    };
    const addressParam = (): string => {
      const a = params.get('id') ?? '';
      if (!isAddress(a)) throw bad('invalid address');
      return a;
    };
    if (method === 'POST') {
      if (path === '/transaction/validate') {
        const result = this.validate(body);
        return json(didFail ? { ...result, did_fail: true, error: { __type: 'mmx.exec_error_t', code: 1, address: 0, operation: 0, line: null, message: 'failed' } } : result);
      }
      if (path === '/transaction/broadcast') {
        this.submit(body);
        return { status: 200, body: '' };
      }
      throw new HttpError(404, 'not found');
    }
    if (method !== 'GET') throw new HttpError(405, 'method not allowed');
    switch (path) {
      case '/chain/info':
        return json({ ...CHAIN_INFO, network: this.network });
      case '/node/info': {
        const ts = this.timeOf(this.height);
        return json({ __type: 'mmx.NetworkInfo', height: this.height, is_synced: this.synced, name: this.network, synced_since: this.startHeight, time_stamp: ts, time: ts / 1000, vdf_height: this.height + 109_000, node_version: 'mock' });
      }
      case '/address': {
        const address = addressParam();
        if (uint('limit', 100) > 1000) throw bad('limit > 1000');
        const balances = [...(this.balances.get(address) ?? new Map<string, bigint>())].map(([contract, amount]) => {
          const t = this.token(contract);
          return { amount: amount.toString(), contract, decimals: t.decimals, is_native: contract === NATIVE, symbol: t.symbol, value: valueOf(amount, t.decimals) };
        });
        return json({ balances, contract: null });
      }
      case '/address/history': {
        const address = addressParam();
        const limit = uint('limit', 100);
        if (limit > 1000) throw bad('limit > 1000');
        const rows = this.history(address, limit, uint('since', 0), uint('until', U32_MAX));
        return json(
          rows.map((e) => {
            const t = this.token(e.contract);
            const ts = this.timeOf(e.height);
            return { __type: 'mmx.tx_entry_t', address: e.address, amount: e.amount.toString(), contract: e.contract, decimals: t.decimals, height: e.height, is_native: e.contract === NATIVE, is_pending: false, is_validated: false, memo: e.memo ?? null, symbol: t.symbol, time: ts / 1000, time_stamp: ts, txid: e.txid, type: e.type, value: valueOf(e.amount, t.decimals) };
          }),
        );
      }
      case '/transaction': {
        const id = params.get('id') ?? '';
        if (!/^[0-9A-Fa-f]{64}$/.test(id)) throw bad('transaction?id');
        const tx = this.txs.get(id.toUpperCase());
        return tx ? json(this.renderTx(tx)) : { status: 200, body: 'null', contentType: 'application/json' };
      }
    }
    throw new HttpError(404, 'not found');
  }

  private renderTx(tx: TxRecord): Record<string, unknown> {
    const io = (x: Io) => {
      const t = this.token(x.contract);
      return { address: x.address, amount: x.amount.toString(), contract: x.contract, decimals: t.decimals, is_native: x.contract === NATIVE, memo: x.memo ?? null, symbol: t.symbol, value: valueOf(x.amount, t.decimals) };
    };
    const out: Record<string, unknown> = {
      __type: 'mmx.tx_info_t',
      id: tx.id,
      expires: tx.expires,
      did_fail: tx.didFail,
      height: tx.height ?? null,
      block: tx.height !== undefined ? hex64(`block|${tx.height}`) : null,
      message: '',
      fee: { amount: tx.fee.toString(), value: valueOf(tx.fee, 6) },
      cost: { amount: tx.fee.toString(), value: valueOf(tx.fee, 6) },
      note: 'TRANSFER',
      sender: tx.sender ?? null,
      inputs: tx.inputs.map((i) => ({ __type: 'mmx.txin_t', ...io(i), solution: 0, flags: 0 })),
      outputs: tx.outputs.map((o) => ({ __type: 'mmx.txout_t', ...io(o) })),
      operations: [],
      deployed: null,
    };
    if (tx.height !== undefined) {
      const ts = this.timeOf(tx.height);
      out.time_stamp = ts;
      out.time = ts / 1000;
      out.confirm = this.height >= tx.height ? this.height - tx.height + 1 : 0;
    }
    return out;
  }

  private control(method: string, path: string, body: string): unknown {
    let input: Record<string, unknown> = {};
    if (body.trim()) {
      try {
        const v: unknown = JSON.parse(body);
        if (isRecord(v)) input = v;
      } catch {
        throw bad('invalid JSON');
      }
    }
    const get = method === 'GET';
    switch (path) {
      case '/__mock/state':
        return { height: this.height, network: this.network, synced: this.synced, confirmBlocks: this.confirmBlocks, pending: this.pending.size, broadcasts: this.broadcasts.length };
      case '/__mock/broadcasts':
        return this.broadcasts;
      case '/__mock/requests':
        return this.requests;
    }
    if (get) throw new HttpError(404, 'not found');
    switch (path) {
      case '/__mock/reset':
        this.reset({
          ...(typeof input.height === 'number' ? { height: input.height } : {}),
          ...(typeof input.network === 'string' ? { network: input.network } : {}),
          ...(typeof input.confirmBlocks === 'number' ? { confirmBlocks: input.confirmBlocks } : {}),
        });
        return { height: this.height };
      case '/__mock/fund':
        return this.fund(input as unknown as FundRequest);
      case '/__mock/token':
        this.registerToken(String(input.contract), String(input.symbol), typeof input.decimals === 'number' ? input.decimals : 6);
        return { ok: true };
      case '/__mock/advance':
        return { height: this.advance(typeof input.blocks === 'number' ? input.blocks : 1) };
      case '/__mock/node':
        if (typeof input.synced === 'boolean') this.synced = input.synced;
        if (typeof input.network === 'string') this.network = input.network;
        if (typeof input.confirmBlocks === 'number' && Number.isInteger(input.confirmBlocks) && input.confirmBlocks >= 1) this.confirmBlocks = input.confirmBlocks;
        return { ok: true };
      case '/__mock/fail': {
        const mode = input.mode;
        if (typeof input.path !== 'string' || (mode !== 'status' && mode !== 'drop' && mode !== 'did_fail')) throw bad('invalid rule');
        const rule: FailRule & { times: number } = {
          path: input.path,
          mode,
          times: typeof input.times === 'number' && input.times > 0 ? input.times : 1,
          ...(typeof input.method === 'string' ? { method: input.method } : {}),
          ...(typeof input.status === 'number' ? { status: input.status } : {}),
          ...(typeof input.body === 'string' ? { body: input.body } : {}),
          ...(input.after === true ? { after: true } : {}),
        };
        this.rules.push(rule);
        return { ok: true };
      }
    }
    throw new HttpError(404, 'not found');
  }
}

// ---------------------------------------------------------------------------
// HTTP server

const CORS: Record<string, string> = {
  'Access-Control-Allow-Origin': '*',
  'Access-Control-Allow-Methods': 'GET, POST, OPTIONS',
  'Access-Control-Allow-Headers': 'Content-Type',
  'Access-Control-Max-Age': '86400',
};

function readBody(req: IncomingMessage, limit: number): Promise<string | null> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    let size = 0;
    req.on('data', (chunk: Buffer) => {
      size += chunk.length;
      if (size > limit) {
        resolve(null);
        req.destroy();
        return;
      }
      chunks.push(chunk);
    });
    req.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')));
    req.on('error', reject);
  });
}

export interface MockServer {
  url: string;
  rpc: MockRpc;
  close(): Promise<void>;
}

export async function startMockRpcServer(options: { port?: number; host?: string; blockMs?: number } & MockOptions = {}): Promise<MockServer> {
  const rpc = new MockRpc(options);
  const host = options.host ?? '127.0.0.1';
  const server: Server = createServer((req: IncomingMessage, res: ServerResponse) => {
    void (async () => {
      for (const [k, v] of Object.entries(CORS)) res.setHeader(k, v);
      if (req.method === 'OPTIONS') {
        rpc.log('OPTIONS', req.url ?? '/', 204);
        res.writeHead(204).end();
        return;
      }
      const body = await readBody(req, 1024 * 1024).catch(() => null);
      if (body === null) {
        res.writeHead(413, { 'Content-Type': 'text/plain' }).end('payload too large');
        return;
      }
      const reply = rpc.handle(req.method ?? 'GET', req.url ?? '/', body);
      if (reply.drop) {
        req.socket.destroy();
        return;
      }
      res.writeHead(reply.status, reply.contentType ? { 'Content-Type': reply.contentType, 'Cache-Control': 'no-store' } : { 'Cache-Control': 'no-store' });
      res.end(reply.body);
    })();
  });
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(options.port ?? 0, host, () => resolve());
  });
  const address = server.address();
  const port = typeof address === 'object' && address ? address.port : options.port;
  const timer = options.blockMs ? setInterval(() => rpc.advance(1), options.blockMs) : null;
  return {
    url: `http://${host}:${port}`,
    rpc,
    close: () =>
      new Promise<void>((resolve) => {
        if (timer) clearInterval(timer);
        server.closeAllConnections();
        server.close(() => resolve());
      }),
  };
}

function argValue(name: string): string | undefined {
  const i = process.argv.indexOf(name);
  return i >= 0 ? process.argv[i + 1] : undefined;
}

const entry = process.argv[1];
if (entry && import.meta.url === pathToFileURL(entry).href) {
  const port = Number(argValue('--port') ?? process.env.E2E_RPC_PORT ?? 31700);
  const blockMs = Number(argValue('--block-ms') ?? 0);
  const server = await startMockRpcServer({ port, ...(blockMs > 0 ? { blockMs } : {}) });
  process.stdout.write(`mock chain server on ${server.url}\n`);
  const stop = () => void server.close().then(() => process.exit(0));
  process.on('SIGINT', stop);
  process.on('SIGTERM', stop);
}
