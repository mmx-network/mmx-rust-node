/**
 * In-memory backend for development, screenshots and tests. Emulates the
 * wallet service and the public chain server (/node/info, /chain/info,
 * /address, /balance, /address/history, /transaction) with deterministic
 * fake data. No network access. Never part of web or desktop builds.
 */
import { encodeAddress, NATIVE_CONTRACT, normalizeAddress } from '../lib/address';
import { isExplorerUrl } from '../lib/explorer';
import { utf8Length } from '../lib/memo';
import { hasUnsafeCharacters } from '../lib/text';
import { WORDS, isWord } from '../lib/wordlist';
import {
  BackendError,
  type AppState,
  type Backend,
  type CreateWalletRequest,
  type ImportWalletRequest,
  type Platform,
  type PreparedSend,
  type SendRequest,
  type UnlockRequest,
  type WalletId,
  type WalletInfo,
} from './types';

/** Captured from https://rpc.mmx.network/chain/info (mainnet). */
const CHAIN_INFO: Readonly<Record<string, unknown>> = {
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

const DECIMALS = 6;
const BLOCK_MS = 10_000;
/** Mainnet height observed at BASE_TIME. */
const BASE_HEIGHT = 5_571_706;
const BASE_TIME = 1_792_835_101_709;
const HISTORY_DAYS = 120;
const EXPIRE_DELTA = 100;
const PREPARED_TTL_MS = 10 * 60_000;

const MIN_TXFEE = 20000n;
const MIN_TXFEE_IO = 10000n;
const MIN_TXFEE_SIGN = 10000n;
const MIN_TXFEE_MEMO = 5000n;

// ---------------------------------------------------------------------------
// Deterministic helpers

function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function hash32(text: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  h ^= h >>> 16;
  h = Math.imul(h, 0x85ebca6b);
  h ^= h >>> 13;
  h = Math.imul(h, 0xc2b2ae35);
  h ^= h >>> 16;
  return h >>> 0;
}

function seededBytes(seedText: string, n = 32): Uint8Array {
  const r = mulberry32(hash32(seedText));
  const out = new Uint8Array(n);
  for (let i = 0; i < n; i++) out[i] = Math.floor(r() * 256);
  return out;
}

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, '0'))
    .join('')
    .toUpperCase();
}

function seededAddress(seedText: string): string {
  return encodeAddress(seededBytes(seedText));
}

function seededHash(seedText: string): string {
  return toHex(seededBytes(seedText));
}

function randomHash(): string {
  const bytes = new Uint8Array(32);
  crypto.getRandomValues(bytes);
  return toHex(bytes);
}

function fingerprintOf(mnemonic: string, passphrase: string | null): string {
  return String(hash32(`fingerprint|${mnemonic}|${passphrase === null ? '-' : `+${passphrase}`}`));
}

function deriveAddresses(mnemonic: string, passphrase: string | null, count: number): string[] {
  const out: string[] = [];
  for (let i = 0; i < count; i++) out.push(seededAddress(`address|${mnemonic}|${passphrase ?? ''}|${i}`));
  return out;
}

function seededMnemonic(seedText: string): string {
  const r = mulberry32(hash32(seedText));
  return Array.from({ length: 24 }, () => WORDS[Math.floor(r() * WORDS.length)]!).join(' ');
}

function randomMnemonic(): string {
  const buf = new Uint32Array(24);
  crypto.getRandomValues(buf);
  return Array.from(buf, (v) => WORDS[v % WORDS.length]!).join(' ');
}

function normalizeMnemonic(text: string): string {
  const words = text.trim().toLowerCase().split(/\s+/u);
  if (words.length !== 24 || !words.every(isWord)) throw new BackendError('invalid_mnemonic');
  return words.join(' ');
}

function heightAt(time: number): number {
  return BASE_HEIGHT + Math.floor((time - BASE_TIME) / BLOCK_MS);
}

function timeOf(height: number): number {
  return BASE_TIME + (height - BASE_HEIGHT) * BLOCK_MS + (hash32(`t${height}`) % 2000);
}

function memoCost(memo: string | undefined): bigint {
  return memo === undefined ? 0n : BigInt(Math.max(1, Math.ceil(utf8Length(memo) / 32))) * MIN_TXFEE_MEMO;
}

/** Static cost with k inputs (no memo), one output and the given signature count. */
function staticFee(inputs: number, memo: string | undefined, signatures: number, level: number): bigint {
  const cost = MIN_TXFEE + BigInt(inputs + 1) * MIN_TXFEE_IO + memoCost(memo) + BigInt(signatures) * MIN_TXFEE_SIGN;
  return cost * BigInt(level);
}

function toValue(amount: bigint, decimals: number): number {
  return Number(amount) / 10 ** decimals;
}

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

// ---------------------------------------------------------------------------
// Chain emulation

type EntryType = 'SPEND' | 'RECEIVE' | 'TXFEE' | 'REWARD' | 'VDF_REWARD' | 'PROJECT_REWARD';

interface Entry {
  height: number;
  time: number;
  txid: string;
  type: EntryType;
  address: string;
  contract: string;
  amount: bigint;
  memo?: string;
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
  time?: number;
  expires: number;
  didFail: boolean;
  fee: bigint;
  sender: string;
  inputs: Io[];
  outputs: Io[];
}

interface Token {
  symbol: string;
  decimals: number;
  validated: boolean;
}

interface MempoolTx {
  record: TxRecord;
  rows: Array<Omit<Entry, 'height' | 'time'>>;
  includeAt: number;
}

const USDM = seededAddress('token|USDM');
const FAKE_MMX = seededAddress('token|fake-mmx');

const TOKENS = new Map<string, Token>([
  [NATIVE_CONTRACT, { symbol: 'MMX', decimals: DECIMALS, validated: false }],
  [USDM, { symbol: 'USDM', decimals: 6, validated: true }],
  [FAKE_MMX, { symbol: 'MMX', decimals: 6, validated: false }],
]);

const FOREIGN = Array.from({ length: 16 }, (_, i) => seededAddress(`foreign|${i}`));

const MEMOS = [
  'Invoice 2024-117',
  'Thanks!',
  'Rent October',
  'Pool payout',
  'Coffee \u{2615}',
  'Order #88412',
  'Refund',
  'Gift for Max',
  'Payment for plots',
  '\u{202e}evil.exe\u{202c} totally safe',
];

class MockChain {
  readonly entries = new Map<string, Entry[]>();
  readonly balances = new Map<string, Map<string, bigint>>();
  readonly txs = new Map<string, TxRecord>();
  readonly mempool: MempoolTx[] = [];
  private processed: number;

  constructor(
    private readonly now: () => number,
    private readonly liveRewardAddress: string | null,
  ) {
    this.processed = heightAt(now());
  }

  height(): number {
    return heightAt(this.now());
  }

  balance(address: string, contract: string): bigint {
    return this.balances.get(address)?.get(contract) ?? 0n;
  }

  private add(address: string, contract: string, delta: bigint): void {
    let m = this.balances.get(address);
    if (!m) {
      m = new Map();
      this.balances.set(address, m);
    }
    const next = (m.get(contract) ?? 0n) + delta;
    if (next < 0n) throw new Error('negative mock balance');
    if (next === 0n) m.delete(contract);
    else m.set(contract, next);
  }

  record(entry: Entry): void {
    let list = this.entries.get(entry.address);
    if (!list) {
      list = [];
      this.entries.set(entry.address, list);
    }
    list.push(entry);
    const credit = entry.type !== 'SPEND' && entry.type !== 'TXFEE';
    this.add(entry.address, entry.contract, credit ? entry.amount : -entry.amount);
  }

  /** Advances to the current height: includes due transactions and live rewards. */
  tick(): void {
    const height = this.height();
    if (height <= this.processed) return;
    const from = Math.max(this.processed + 1, height - 50_000);
    for (let h = from; h <= height; h++) {
      if (this.liveRewardAddress && hash32(`live|${h}`) % 45 === 0) {
        this.record({
          height: h,
          time: timeOf(h),
          txid: seededHash(`block|${h}`),
          type: 'REWARD',
          address: this.liveRewardAddress,
          contract: NATIVE_CONTRACT,
          amount: 500000n + BigInt(hash32(`fees|${h}`) % 60000),
        });
      }
      for (const tx of this.mempool.filter((m) => m.includeAt === h)) this.include(tx, h);
    }
    this.processed = height;
  }

  private include(tx: MempoolTx, height: number): void {
    const index = this.mempool.indexOf(tx);
    if (index >= 0) this.mempool.splice(index, 1);
    const time = timeOf(height);
    tx.record.height = height;
    tx.record.time = time;
    for (const row of tx.rows) this.record({ ...row, height, time });
  }

  submit(tx: MempoolTx): void {
    this.txs.set(tx.record.id, tx.record);
    this.mempool.push(tx);
  }

  history(address: string, limit: number, since: number, until: number): Entry[] {
    const rows = (this.entries.get(address) ?? []).filter((e) => e.height >= since && e.height <= until);
    rows.sort(compareEntries);
    if (rows.length <= limit) return rows;
    const cutoff = rows[limit - 1]!.height;
    let end = limit;
    while (end < rows.length && rows[end]!.height === cutoff) end++;
    return rows.slice(0, end);
  }
}

/** Node order: (height, txid, type, contract, address, memo, amount) descending. */
function compareEntries(a: Entry, b: Entry): number {
  const s = (x: string, y: string) => (x < y ? 1 : x > y ? -1 : 0);
  return (
    b.height - a.height ||
    s(a.txid, b.txid) ||
    s(a.type, b.type) ||
    s(a.contract, b.contract) ||
    s(a.address, b.address) ||
    s(a.memo ?? '', b.memo ?? '') ||
    (a.amount < b.amount ? 1 : a.amount > b.amount ? -1 : 0)
  );
}

interface HistoryPlan {
  rewards: number;
  vdfRewards: number;
  projectRewards: number;
  receives: number;
  sends: number;
  moves: number;
  tokenReceives: number;
  tokenSends: number;
  fakeTokens: number;
  /** Addresses that receive funds (index < this). */
  activeAddresses: number;
  /** Extra address index holding funds (exercises "hidden funds" warnings). */
  remoteIndex?: number;
}

/** Builds ~300 history entries over HISTORY_DAYS for a wallet's addresses. */
function generateHistory(chain: MockChain, addresses: string[], plan: HistoryPlan, seed: string, now: number): void {
  const r = mulberry32(hash32(seed));
  const pick = <T,>(list: readonly T[]): T => list[Math.floor(r() * list.length)]!;
  type Kind = 'reward' | 'vdf' | 'project' | 'receive' | 'send' | 'move' | 'tokenReceive' | 'tokenSend' | 'fake';
  const kinds: Kind[] = [];
  const push = (k: Kind, n: number) => {
    for (let i = 0; i < n; i++) kinds.push(k);
  };
  push('reward', plan.rewards);
  push('vdf', plan.vdfRewards);
  push('project', plan.projectRewards);
  push('receive', plan.receives);
  push('send', plan.sends);
  push('move', plan.moves);
  push('tokenReceive', plan.tokenReceives);
  push('tokenSend', plan.tokenSends);
  push('fake', plan.fakeTokens);
  const start = now - HISTORY_DAYS * 86_400_000;
  const events = kinds.map((kind) => ({ kind, time: start + r() * (now - start - 3_600_000) }));
  // Funds arrive first so early spends are possible.
  events.sort((a, b) => a.time - b.time);
  const firstReceive = events.findIndex((e) => e.kind === 'receive');
  if (firstReceive > 0) {
    const [ev] = events.splice(firstReceive, 1);
    events.unshift({ kind: ev!.kind, time: start + 3_600_000 });
  }
  const receivers = addresses.slice(0, Math.max(1, Math.min(plan.activeAddresses, addresses.length)));
  if (plan.remoteIndex !== undefined && addresses[plan.remoteIndex]) receivers.push(addresses[plan.remoteIndex]!);

  const natives = () =>
    addresses
      .map((a) => ({ address: a, amount: chain.balance(a, NATIVE_CONTRACT) }))
      .filter((x) => x.amount > 0n)
      .sort((x, y) => (y.amount > x.amount ? 1 : y.amount < x.amount ? -1 : 0));

  for (const ev of events) {
    const height = heightAt(ev.time);
    const time = timeOf(height);
    const base = { height, time };
    switch (ev.kind) {
      case 'reward':
      case 'vdf':
      case 'project': {
        const type: EntryType = ev.kind === 'reward' ? 'REWARD' : ev.kind === 'vdf' ? 'VDF_REWARD' : 'PROJECT_REWARD';
        const amount =
          ev.kind === 'reward' ? 500000n + BigInt(Math.floor(r() * 60000)) : ev.kind === 'vdf' ? 200000n : 50000n;
        chain.record({
          ...base,
          txid: seededHash(`block|${height}`),
          type,
          address: r() < 0.85 ? addresses[0]! : addresses[Math.min(1, addresses.length - 1)]!,
          contract: NATIVE_CONTRACT,
          amount,
        });
        break;
      }
      case 'receive':
      case 'tokenReceive':
      case 'fake': {
        const contract = ev.kind === 'receive' ? NATIVE_CONTRACT : ev.kind === 'tokenReceive' ? USDM : FAKE_MMX;
        const to = ev.kind === 'fake' ? addresses[0]! : pick(receivers);
        const from = pick(FOREIGN);
        const amount =
          ev.kind === 'fake'
            ? 1_000_000_000_000n
            : BigInt(Math.floor(r() * (ev.kind === 'receive' ? 400 : 2500)) + 1) * 1_000_000n + BigInt(Math.floor(r() * 1_000_000));
        const memo = r() < 0.4 ? pick(MEMOS) : undefined;
        const txid = seededHash(`tx|${seed}|${ev.time}`);
        chain.record({ ...base, txid, type: 'RECEIVE', address: to, contract, amount, ...(memo ? { memo } : {}) });
        chain.txs.set(txid, {
          id: txid,
          height,
          time,
          expires: height + EXPIRE_DELTA,
          didFail: false,
          fee: staticFee(1, memo, 1, 1) + (memo ? memoCost(memo) : 0n),
          sender: from,
          inputs: [{ address: from, contract, amount, ...(memo ? { memo } : {}) }],
          outputs: [{ address: to, contract, amount, ...(memo ? { memo } : {}) }],
        });
        break;
      }
      case 'send':
      case 'tokenSend':
      case 'move': {
        const contract = ev.kind === 'tokenSend' ? USDM : NATIVE_CONTRACT;
        const holders = addresses
          .map((a) => ({ address: a, amount: chain.balance(a, contract) }))
          .filter((x) => x.amount > 0n)
          .sort((x, y) => (y.amount > x.amount ? 1 : y.amount < x.amount ? -1 : 0));
        if (holders.length === 0) break;
        const total = holders.reduce((s, h) => s + h.amount, 0n);
        const share = ev.kind === 'move' ? 0.4 : 0.25;
        let amount = (total * BigInt(Math.floor(r() * share * 1000) + 1)) / 1000n;
        amount -= amount % 1000n;
        if (amount <= 0n) break;
        const targets = receivers.filter((a) => a !== holders[0]!.address);
        if (ev.kind === 'move' && targets.length === 0) break;
        const to = ev.kind === 'move' ? pick(targets) : pick(FOREIGN);
        const memo = ev.kind !== 'move' && r() < 0.35 ? pick(MEMOS.slice(0, 9)) : undefined;
        // Inputs: largest balances first (as the wallet core does).
        const inputs: Io[] = [];
        let left = amount;
        for (const h of holders) {
          if (left === 0n) break;
          const take = left < h.amount ? left : h.amount;
          inputs.push({ address: h.address, contract, amount: take });
          left -= take;
        }
        if (left !== 0n) break;
        const spent = new Map(inputs.map((i) => [i.address, i.amount]));
        const remaining = natives()
          .map((x) => ({ ...x, amount: contract === NATIVE_CONTRACT ? x.amount - (spent.get(x.address) ?? 0n) : x.amount }))
          .sort((x, y) => (y.amount > x.amount ? 1 : y.amount < x.amount ? -1 : 0));
        const sender = remaining[0];
        if (!sender) break;
        const owners = new Set([sender.address, ...inputs.map((i) => i.address)]);
        const fee = staticFee(inputs.length, memo, owners.size, 1);
        if (sender.amount < fee) break;
        const txid = seededHash(`tx|${seed}|${ev.time}`);
        for (const i of inputs) chain.record({ ...base, txid, type: 'SPEND', address: i.address, contract, amount: i.amount });
        if (ev.kind === 'move') {
          chain.record({ ...base, txid, type: 'RECEIVE', address: to, contract, amount });
        }
        chain.record({ ...base, txid, type: 'TXFEE', address: sender.address, contract: NATIVE_CONTRACT, amount: fee });
        chain.txs.set(txid, {
          id: txid,
          height,
          time,
          expires: height + EXPIRE_DELTA,
          didFail: false,
          fee,
          sender: sender.address,
          inputs,
          outputs: [{ address: to, contract, amount, ...(memo ? { memo } : {}) }],
        });
        break;
      }
    }
  }
}

// ---------------------------------------------------------------------------
// Wallets

interface MockWallet {
  id: WalletId;
  name: string;
  mnemonic: string;
  /** null = created without a passphrase. */
  passphrase: string | null;
  fingerprint: string;
  addressCount: number;
  locked: boolean;
  verified: boolean;
}

interface Prepared {
  walletId: WalletId;
  request: SendRequest;
  inputs: Io[];
  sender: string;
  fee: bigint;
  expires: number;
  createdAt: number;
  symbol: string;
  decimals: number;
}

/** Fixed wallets of the mock (also used by end-to-end tests). */
export const MOCK_WALLETS = [
  { name: 'Main', mnemonic: seededMnemonic('wallet|main'), passphrase: null, addressCount: 10 },
  { name: 'Savings', mnemonic: seededMnemonic('wallet|savings'), passphrase: 'correct horse', addressCount: 10 },
  { name: 'Trading', mnemonic: seededMnemonic('wallet|trading'), passphrase: null, addressCount: 12 },
] as const;

export const MOCK_PASSPHRASE = 'correct horse';

const PLANS: Record<string, HistoryPlan> = {
  Main: {
    rewards: 210,
    vdfRewards: 8,
    projectRewards: 2,
    receives: 24,
    sends: 18,
    moves: 8,
    tokenReceives: 0,
    tokenSends: 0,
    fakeTokens: 1,
    activeAddresses: 3,
  },
  Savings: {
    rewards: 0,
    vdfRewards: 0,
    projectRewards: 0,
    receives: 90,
    sends: 45,
    moves: 12,
    tokenReceives: 14,
    tokenSends: 5,
    fakeTokens: 0,
    activeAddresses: 4,
  },
  Trading: {
    rewards: 0,
    vdfRewards: 0,
    projectRewards: 0,
    receives: 80,
    sends: 50,
    moves: 10,
    tokenReceives: 25,
    tokenSends: 15,
    fakeTokens: 2,
    activeAddresses: 5,
    remoteIndex: 11,
  },
};

export interface MockOptions {
  platform?: Platform;
  /** Start without wallets (onboarding). */
  empty?: boolean;
  /** Simulated network latency in ms (0 = none). */
  latencyMs?: number;
  /** Clock override for tests. */
  now?: () => number;
}

export class MockBackend implements Backend {
  readonly platform: Platform;
  readonly chain: MockChain;
  private readonly now: () => number;
  private readonly latencyMs: number;
  private wallets: MockWallet[] = [];
  private activeWallet: WalletId | undefined;
  private appState: AppState;
  private readonly prepared = new Map<string, Prepared>();
  /** Test hooks. */
  clipboard = '';
  lastOpened: string | null = null;
  exported: { name: string; contents: string } | null = null;
  nextImport: string | null = null;
  /** When set, the next broadcast fails with this error after reaching the server. */
  failNextBroadcast: BackendError | null = null;

  constructor(options: MockOptions = {}) {
    this.platform = options.platform ?? 'desktop';
    this.now = options.now ?? (() => Date.now());
    this.latencyMs = options.latencyMs ?? 0;
    const main = MOCK_WALLETS[0];
    const mainAddress = deriveAddresses(main.mnemonic, main.passphrase, 1)[0]!;
    this.chain = new MockChain(this.now, options.empty ? null : mainAddress);
    this.appState = { labels: {}, contacts: [], settings: { theme: 'system', autoLockMinutes: 15, feeLevel: 1 } };
    if (!options.empty) this.seed();
  }

  private seed(): void {
    const now = this.now();
    for (const def of MOCK_WALLETS) {
      const wallet = this.makeWallet(def.name, def.mnemonic, def.passphrase, def.addressCount);
      wallet.verified = true;
      this.wallets.push(wallet);
      const addresses = deriveAddresses(def.mnemonic, def.passphrase, 100);
      generateHistory(this.chain, addresses, PLANS[def.name]!, `history|${def.name}`, now);
    }
    if (this.platform === 'desktop') this.wallets[0]!.id = 'wallet.dat';
    this.activeWallet = this.wallets[0]!.id;
    this.appState = {
      activeWallet: this.activeWallet,
      labels: { [this.wallets[0]!.id]: { 0: 'Farming', 1: 'Spending' }, [this.wallets[2]!.id]: { 11: 'Cold storage' } },
      contacts: [
        { id: 'c-alice', name: 'Alice', address: FOREIGN[0]!, note: 'Plot hosting' },
        { id: 'c-pool', name: 'Mining pool', address: FOREIGN[1]! },
        { id: 'c-exchange', name: 'Exchange deposit', address: FOREIGN[2]!, note: 'Use memo 88412' },
      ],
      settings: { theme: 'system', autoLockMinutes: 15, feeLevel: 1 },
    };
  }

  private makeWallet(name: string, mnemonic: string, passphrase: string | null, addressCount: number): MockWallet {
    const fingerprint = fingerprintOf(mnemonic, passphrase);
    return {
      id: this.platform === 'web' ? fingerprint : `wallet_${fingerprint}.dat`,
      name,
      mnemonic,
      passphrase,
      fingerprint,
      addressCount,
      // Desktop opens wallets without a passphrase directly; the web keeps no seed between sessions.
      locked: this.platform === 'web' || (passphrase !== null && passphrase !== ''),
      verified: true,
    };
  }

  private info(w: MockWallet): WalletInfo {
    const info: WalletInfo = {
      id: w.id,
      name: w.name,
      fingerprint: w.fingerprint,
      withPassphrase: w.passphrase !== null && w.passphrase !== '',
      locked: w.locked,
      addressCount: w.addressCount,
    };
    if (this.platform === 'web') info.verified = w.verified;
    return info;
  }

  private find(id: WalletId): MockWallet {
    const w = this.wallets.find((x) => x.id === id);
    if (!w) throw new BackendError('wallet_not_found');
    return w;
  }

  private unlocked(id: WalletId): MockWallet {
    const w = this.find(id);
    if (w.locked) throw new BackendError('locked');
    return w;
  }

  private async delay(): Promise<void> {
    if (this.latencyMs > 0) {
      await new Promise((resolve) => setTimeout(resolve, this.latencyMs * (0.5 + Math.random())));
    }
  }

  private addressesOf(w: MockWallet): string[] {
    return deriveAddresses(w.mnemonic, w.passphrase, w.addressCount);
  }

  // -- app state

  async loadAppState(): Promise<AppState> {
    await this.delay();
    return clone({ ...this.appState, activeWallet: this.activeWallet });
  }

  async saveAppState(s: AppState): Promise<void> {
    await this.delay();
    const url = s.settings.rpcUrl;
    if (this.platform === 'desktop' && url !== undefined && url !== this.appState.settings.rpcUrl) {
      if (!/^https?:\/\/[^\s/]+/.test(url)) throw new BackendError('invalid_argument');
      if (/offline|invalid|unreachable/.test(url)) throw new BackendError('rpc_unreachable');
    }
    this.appState = clone(s);
    if (s.activeWallet && this.wallets.some((w) => w.id === s.activeWallet)) this.activeWallet = s.activeWallet;
  }

  // -- wallets

  async listWallets(): Promise<WalletInfo[]> {
    await this.delay();
    return this.wallets.map((w) => this.info(w));
  }

  private checkName(name: string): string {
    const trimmed = name.trim();
    if (!trimmed || Array.from(trimmed).length > 64 || hasUnsafeCharacters(trimmed)) throw new BackendError('invalid_argument');
    return trimmed;
  }

  private checkCount(n: number): void {
    if (!Number.isInteger(n) || n < 1 || n > 100) throw new BackendError('invalid_argument');
  }

  async createWallet(r: CreateWalletRequest): Promise<{ wallet: WalletInfo; mnemonic: string }> {
    await this.delay();
    const name = this.checkName(r.name);
    this.checkCount(r.addressCount);
    const mnemonic = randomMnemonic();
    const wallet = this.makeWallet(name, mnemonic, r.passphrase ? r.passphrase : null, r.addressCount);
    if (this.platform === 'web') {
      wallet.locked = true;
      wallet.verified = false;
    } else {
      wallet.locked = false;
    }
    if (this.wallets.some((w) => w.id === wallet.id)) throw new BackendError('wallet_exists');
    this.wallets.push(wallet);
    return { wallet: this.info(wallet), mnemonic };
  }

  /** §6.3 detection; returns the passphrase that opens the wallet (null = none). */
  private detect(mnemonic: string, expected: string, passphrase: string | undefined, withPassphrase: boolean): string | null {
    if (fingerprintOf(mnemonic, null) === expected) return null;
    if (fingerprintOf(mnemonic, '') === expected) return '';
    if (!withPassphrase) throw new BackendError('wallet_mismatch');
    if (passphrase === undefined || passphrase === '') throw new BackendError('passphrase_required');
    if (fingerprintOf(mnemonic, passphrase) === expected) return passphrase;
    throw new BackendError('invalid_passphrase');
  }

  async importWallet(r: ImportWalletRequest): Promise<WalletInfo> {
    await this.delay();
    const name = this.checkName(r.name);
    this.checkCount(r.addressCount);
    const mnemonic = normalizeMnemonic(r.mnemonic);
    let passphrase: string | null = r.passphrase ? r.passphrase : null;
    if (r.expectedFingerprint !== undefined) {
      if (!/^\d{1,10}$/.test(r.expectedFingerprint)) throw new BackendError('invalid_argument');
      passphrase = this.detect(mnemonic, r.expectedFingerprint, r.passphrase, true);
    }
    const wallet = this.makeWallet(name, mnemonic, passphrase, r.addressCount);
    if (this.wallets.some((w) => w.fingerprint === wallet.fingerprint)) throw new BackendError('wallet_exists');
    wallet.locked = false;
    wallet.verified = true;
    this.wallets.push(wallet);
    return this.info(wallet);
  }

  async unlockWallet(id: WalletId, r: UnlockRequest): Promise<WalletInfo> {
    await this.delay();
    const w = this.find(id);
    if (this.platform === 'web') {
      if (r.mnemonic === undefined) {
        if (w.locked) throw new BackendError('locked');
        if (r.passphrase !== undefined && r.passphrase !== w.passphrase) throw new BackendError('invalid_passphrase');
        return this.info(w);
      }
      const mnemonic = normalizeMnemonic(r.mnemonic);
      const withPassphrase = w.passphrase !== null && w.passphrase !== '';
      this.detect(mnemonic, w.fingerprint, r.passphrase, withPassphrase);
      w.locked = false;
      w.verified = true;
      return this.info(w);
    }
    if (w.passphrase === null || w.passphrase === '') {
      w.locked = false;
      return this.info(w);
    }
    if (r.passphrase === undefined || r.passphrase === '') throw new BackendError('passphrase_required');
    if (fingerprintOf(w.mnemonic, r.passphrase) !== w.fingerprint) throw new BackendError('invalid_passphrase');
    w.locked = false;
    return this.info(w);
  }

  async lockWallet(id: WalletId): Promise<void> {
    await this.delay();
    const w = this.find(id);
    if (this.platform === 'web' || (w.passphrase !== null && w.passphrase !== '')) w.locked = true;
  }

  async lockAll(): Promise<void> {
    await this.delay();
    for (const w of this.wallets) {
      if (this.platform === 'web' || (w.passphrase !== null && w.passphrase !== '')) w.locked = true;
    }
    for (const [txid, p] of this.prepared) if (this.find(p.walletId).locked) this.prepared.delete(txid);
  }

  async renameWallet(id: WalletId, name: string): Promise<void> {
    await this.delay();
    this.find(id).name = this.checkName(name);
  }

  async setAddressCount(id: WalletId, n: number): Promise<string[]> {
    await this.delay();
    this.checkCount(n);
    const w = this.unlocked(id);
    w.addressCount = n;
    return this.addressesOf(w);
  }

  async removeWallet(id: WalletId): Promise<void> {
    await this.delay();
    this.find(id);
    this.wallets = this.wallets.filter((w) => w.id !== id);
    if (this.activeWallet === id) this.activeWallet = this.wallets[0]?.id;
    for (const [txid, p] of this.prepared) if (p.walletId === id) this.prepared.delete(txid);
  }

  async revealMnemonic(id: WalletId): Promise<string> {
    await this.delay();
    return this.unlocked(id).mnemonic;
  }

  async getAddresses(id: WalletId): Promise<string[]> {
    await this.delay();
    return this.addressesOf(this.unlocked(id));
  }

  async setActiveWallet(id: WalletId): Promise<void> {
    await this.delay();
    this.find(id);
    this.activeWallet = id;
  }

  // -- chain server

  async rpcGet(path: string): Promise<unknown> {
    await this.delay();
    return this.handle(path);
  }

  private handle(path: string): unknown {
    const q = path.indexOf('?');
    const route = q >= 0 ? path.slice(0, q) : path;
    const query = q >= 0 ? path.slice(q + 1) : '';
    const allowed = ['/node/info', '/chain/info', '/address', '/balance', '/address/history', '/transaction'];
    if (!allowed.includes(route) || !/^[A-Za-z0-9=&_-]*$/.test(query)) throw new BackendError('invalid_argument');
    const params = new URLSearchParams(query);
    this.chain.tick();
    const height = this.chain.height();
    const fail = () => new BackendError('rpc_http_error', 'HTTP 500');
    const uint = (name: string, fallback: number): number => {
      const v = params.get(name);
      if (v === null) return fallback;
      if (!/^\d{1,10}$/.test(v) || Number(v) > 0xffffffff) throw fail();
      return Number(v);
    };
    const addressParam = (): string => {
      const a = normalizeAddress(params.get('id') ?? '');
      if (!a) throw fail();
      return a;
    };
    switch (route) {
      case '/chain/info':
        return clone(CHAIN_INFO);
      case '/node/info': {
        const ts = timeOf(height);
        return {
          __type: 'mmx.NetworkInfo',
          height,
          is_synced: true,
          name: 'mainnet',
          synced_since: height - 1200,
          time_stamp: ts,
          time: ts / 1000,
          vdf_height: height + 109_000,
          node_version: 'v1.5.3',
        };
      }
      case '/address': {
        const address = addressParam();
        if (uint('limit', 100) > 1000) throw fail();
        const balances = [...(this.chain.balances.get(address) ?? new Map<string, bigint>())].map(([contract, amount]) => {
          const t = TOKENS.get(contract)!;
          return {
            amount: amount.toString(),
            contract,
            decimals: t.decimals,
            is_native: contract === NATIVE_CONTRACT,
            symbol: t.symbol,
            value: toValue(amount, t.decimals),
          };
        });
        return { balances, contract: null };
      }
      case '/balance': {
        const address = addressParam();
        const balances = [...(this.chain.balances.get(address) ?? new Map<string, bigint>())].map(([contract, amount]) => {
          const t = TOKENS.get(contract)!;
          const v = toValue(amount, t.decimals);
          return {
            contract,
            decimals: t.decimals,
            ...(contract === NATIVE_CONTRACT ? { is_native: true } : {}),
            is_validated: t.validated,
            locked: 0,
            reserved: 0,
            spendable: v,
            symbol: t.symbol,
            total: v,
          };
        });
        return { balances, nfts: [] };
      }
      case '/address/history': {
        const address = addressParam();
        const limit = uint('limit', 100);
        if (limit > 1000) throw fail();
        const rows = this.chain.history(address, limit, uint('since', 0), uint('until', 0xffffffff));
        return rows.map((e) => {
          const t = TOKENS.get(e.contract)!;
          return {
            __type: 'mmx.tx_entry_t',
            address: e.address,
            amount: e.amount.toString(),
            contract: e.contract,
            decimals: t.decimals,
            height: e.height,
            is_native: e.contract === NATIVE_CONTRACT,
            is_pending: false,
            is_validated: t.validated,
            memo: e.memo ?? null,
            symbol: t.symbol,
            time: e.time / 1000,
            time_stamp: e.time,
            txid: e.txid,
            type: e.type,
            value: toValue(e.amount, t.decimals),
          };
        });
      }
      case '/transaction': {
        const id = params.get('id') ?? '';
        if (!/^[0-9A-Fa-f]{64}$/.test(id)) throw fail();
        const tx = this.chain.txs.get(id.toUpperCase());
        return tx ? renderTx(tx, height) : null;
      }
    }
    throw fail();
  }

  // -- sending

  private checkRequest(r: Omit<SendRequest, 'amount'>, needRecipient: boolean): { w: MockWallet; token: Token } {
    const w = this.unlocked(r.walletId);
    if (needRecipient) {
      const to = normalizeAddress(r.to);
      if (!to || to === NATIVE_CONTRACT) throw new BackendError('invalid_address');
    }
    const token = TOKENS.get(r.contract);
    if (!token) throw new BackendError('invalid_argument');
    if (r.memo !== undefined && (utf8Length(r.memo) > 64 || r.memo === '')) throw new BackendError('invalid_memo');
    if (![1, 2, 3, 5, 10].includes(r.feeLevel)) throw new BackendError('invalid_argument');
    return { w, token };
  }

  private nativeBalances(addresses: string[]): Array<{ address: string; amount: bigint }> {
    return addresses
      .map((address) => ({ address, amount: this.chain.balance(address, NATIVE_CONTRACT) }))
      .sort((a, b) => (b.amount > a.amount ? 1 : b.amount < a.amount ? -1 : 0));
  }

  async maxSendable(r: Omit<SendRequest, 'amount'>): Promise<string> {
    await this.delay();
    this.chain.tick();
    const { w } = this.checkRequest(r, false);
    const addresses = this.addressesOf(w);
    if (r.contract === NATIVE_CONTRACT) {
      const balances = this.nativeBalances(addresses).filter((b) => b.amount > 0n);
      let k = balances.length;
      while (k > 0) {
        const fee = staticFee(k, r.memo, k, r.feeLevel);
        const count = balances.filter((b) => b.amount >= fee).length;
        if (count >= k) {
          const sum = balances.slice(0, k).reduce((s, b) => s + b.amount, 0n);
          return sum > fee ? (sum - fee).toString() : '0';
        }
        k = count;
      }
      return '0';
    }
    const holders = addresses.filter((a) => this.chain.balance(a, r.contract) > 0n);
    const total = holders.reduce((s, a) => s + this.chain.balance(a, r.contract), 0n);
    const fee = staticFee(holders.length, r.memo, holders.length + 1, r.feeLevel);
    if (!this.nativeBalances(addresses).some((b) => b.amount >= fee)) throw new BackendError('insufficient_fee');
    return total.toString();
  }

  async prepareSend(r: SendRequest): Promise<PreparedSend> {
    await this.delay();
    this.chain.tick();
    const { w, token } = this.checkRequest(r, true);
    if (!/^\d{1,40}$/.test(r.amount)) throw new BackendError('invalid_amount');
    const amount = BigInt(r.amount);
    if (amount <= 0n) throw new BackendError('invalid_amount');
    const addresses = this.addressesOf(w);
    const holders = addresses
      .map((address) => ({ address, amount: this.chain.balance(address, r.contract) }))
      .filter((h) => h.amount > 0n)
      .sort((a, b) => (b.amount > a.amount ? 1 : b.amount < a.amount ? -1 : 0));
    const inputs: Io[] = [];
    let left = amount;
    for (const h of holders) {
      if (left === 0n) break;
      const take = left < h.amount ? left : h.amount;
      inputs.push({ address: h.address, contract: r.contract, amount: take });
      left -= take;
    }
    if (left !== 0n) throw new BackendError('insufficient_funds');
    const spent = new Map(inputs.map((i) => [i.address, i.amount]));
    const remaining = this.nativeBalances(addresses)
      .map((b) => ({ ...b, amount: r.contract === NATIVE_CONTRACT ? b.amount - (spent.get(b.address) ?? 0n) : b.amount }))
      .sort((a, b) => (b.amount > a.amount ? 1 : b.amount < a.amount ? -1 : 0));
    const sender = remaining[0]!;
    const owners = new Set([sender.address, ...inputs.map((i) => i.address)]);
    const fee = staticFee(inputs.length, r.memo, owners.size, r.feeLevel);
    if (sender.amount < fee) throw new BackendError(r.contract === NATIVE_CONTRACT ? 'insufficient_funds' : 'insufficient_fee');
    const height = this.chain.height();
    const txid = randomHash();
    const prepared: Prepared = {
      walletId: w.id,
      request: { ...r, to: normalizeAddress(r.to)! },
      inputs,
      sender: sender.address,
      fee,
      expires: height + EXPIRE_DELTA,
      createdAt: this.now(),
      symbol: token.symbol,
      decimals: token.decimals,
    };
    this.prepared.set(txid, prepared);
    const result: PreparedSend = {
      txid,
      to: prepared.request.to,
      contract: r.contract,
      symbol: token.symbol,
      decimals: token.decimals,
      amount: amount.toString(),
      fee: fee.toString(),
      expires: prepared.expires,
      height,
    };
    if (r.memo !== undefined) result.memo = r.memo;
    return result;
  }

  async broadcast(walletId: WalletId, txid: string): Promise<void> {
    await this.delay();
    this.chain.tick();
    const p = this.prepared.get(txid);
    if (!p || p.walletId !== walletId || this.now() - p.createdAt > PREPARED_TTL_MS) {
      this.prepared.delete(txid);
      throw new BackendError('prepared_not_found');
    }
    this.prepared.delete(txid);
    const height = this.chain.height();
    if (height > p.expires) throw new BackendError('tx_expired');
    const { request } = p;
    const memo = request.memo;
    const amount = BigInt(request.amount);
    const rows: MempoolTx['rows'] = p.inputs.map((i) => ({
      txid,
      type: 'SPEND' as const,
      address: i.address,
      contract: i.contract,
      amount: i.amount,
    }));
    // The chain records the output for any recipient, own or foreign.
    rows.push({ txid, type: 'RECEIVE', address: request.to, contract: request.contract, amount, ...(memo ? { memo } : {}) });
    rows.push({ txid, type: 'TXFEE', address: p.sender, contract: NATIVE_CONTRACT, amount: p.fee });
    const record: TxRecord = {
      id: txid,
      expires: p.expires,
      didFail: false,
      fee: p.fee,
      sender: p.sender,
      inputs: p.inputs,
      outputs: [{ address: request.to, contract: request.contract, amount, ...(memo ? { memo } : {}) }],
    };
    this.chain.submit({ record, rows, includeAt: height + 2 });
    if (this.failNextBroadcast) {
      const e = this.failNextBroadcast;
      this.failNextBroadcast = null;
      throw e;
    }
  }

  async discardPrepared(txid: string): Promise<void> {
    await this.delay();
    this.prepared.delete(txid);
  }

  // -- platform services

  async copyText(text: string, sensitive?: boolean): Promise<void> {
    this.clipboard = text;
    if (typeof navigator !== 'undefined' && navigator.clipboard) {
      await navigator.clipboard.writeText(text).catch(() => undefined);
    }
    if (sensitive) {
      setTimeout(() => {
        if (this.clipboard !== text) return;
        this.clipboard = '';
        if (typeof navigator !== 'undefined' && navigator.clipboard) void navigator.clipboard.writeText('').catch(() => undefined);
      }, 60_000);
    }
  }

  async openExternal(url: string): Promise<void> {
    if (!isExplorerUrl(url)) throw new BackendError('invalid_argument');
    this.lastOpened = url;
    if (typeof window !== 'undefined') window.open(url, '_blank', 'noopener,noreferrer');
  }

  async exportFile(suggestedName: string, contents: string): Promise<boolean> {
    this.exported = { name: suggestedName, contents };
    if (typeof document === 'undefined') return true;
    const url = URL.createObjectURL(new Blob([contents], { type: 'application/json' }));
    const a = document.createElement('a');
    a.href = url;
    a.download = suggestedName;
    a.rel = 'noopener';
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
    return true;
  }

  async importFile(): Promise<string | null> {
    if (typeof document === 'undefined') {
      const text = this.nextImport;
      this.nextImport = null;
      return text;
    }
    return new Promise((resolve, reject) => {
      const input = document.createElement('input');
      input.type = 'file';
      input.accept = 'application/json,.json';
      input.addEventListener('cancel', () => resolve(null));
      input.addEventListener('change', () => {
        const file = input.files?.[0];
        if (!file) return resolve(null);
        if (file.size > 1024 * 1024) return reject(new BackendError('invalid_argument'));
        file.text().then(resolve, () => reject(new BackendError('io_error')));
      });
      input.click();
    });
  }

  // -- test helpers

  /** Prepared transactions currently held (for tests). */
  preparedCount(): number {
    return this.prepared.size;
  }
}

function renderIo(io: Io): Record<string, unknown> {
  const t = TOKENS.get(io.contract)!;
  return {
    address: io.address,
    amount: io.amount.toString(),
    contract: io.contract,
    decimals: t.decimals,
    is_native: io.contract === NATIVE_CONTRACT,
    memo: io.memo ?? null,
    symbol: t.symbol,
    value: toValue(io.amount, t.decimals),
  };
}

function renderTx(tx: TxRecord, height: number): Record<string, unknown> {
  const out: Record<string, unknown> = {
    __type: 'mmx.tx_info_t',
    id: tx.id,
    expires: tx.expires,
    did_fail: tx.didFail,
    height: tx.height ?? null,
    block: tx.height !== undefined ? seededHash(`block|${tx.height}`) : null,
    message: '',
    fee: { amount: tx.fee.toString(), value: toValue(tx.fee, DECIMALS) },
    cost: { amount: tx.fee.toString(), value: toValue(tx.fee, DECIMALS) },
    note: 'TRANSFER',
    sender: tx.sender,
    inputs: tx.inputs.map((i) => ({ __type: 'mmx.txin_t', ...renderIo(i), solution: 0, flags: 0 })),
    outputs: tx.outputs.map((o) => ({ __type: 'mmx.txout_t', ...renderIo(o) })),
    operations: [],
    deployed: null,
  };
  if (tx.height !== undefined && tx.time !== undefined) {
    out.time_stamp = tx.time;
    out.time = tx.time / 1000;
    out.confirm = height >= tx.height ? height - tx.height + 1 : 0;
  }
  return out;
}

export function createMockBackend(options: MockOptions = {}): MockBackend {
  return new MockBackend(options);
}

/** Entry point used by getBackend() in mock builds: `?platform=web`, `?mock=empty`. */
export async function createBackend(): Promise<Backend> {
  const params = typeof location !== 'undefined' ? new URLSearchParams(location.search) : new URLSearchParams();
  return new MockBackend({
    platform: params.get('platform') === 'web' ? 'web' : 'desktop',
    empty: params.get('mock') === 'empty',
    latencyMs: 120,
  });
}

