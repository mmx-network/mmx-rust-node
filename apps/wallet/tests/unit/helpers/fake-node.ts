/**
 * Minimal chain server for tests: implements /address/history exactly like
 * the C++ node (chunked scan, last block completed, sort, truncate to the
 * limit plus the rest of the cutoff block), plus /node/info and /chain/info.
 */
import { encodeAddress, NATIVE_CONTRACT } from '../../../src/lib/address';
import type { TxType } from '../../../src/chain/types';

export interface FakeEntry {
  height: number;
  type?: TxType;
  amount?: bigint;
  txid?: string;
  contract?: string;
  memo?: string;
  time?: number;
  /** An NFT row: the node renders `is_nft` and no currency metadata. */
  nft?: boolean;
}

interface Stored extends Required<Omit<FakeEntry, 'memo' | 'nft'>> {
  memo?: string;
  nft: boolean;
  address: string;
  counter: number;
}

export const CHAIN_INFO = {
  network: 'mainnet',
  decimals: 6,
  block_interval_ms: 10000,
  min_fee_ratio: [1, 2, 3, 5, 10],
  min_txfee: 20000,
  min_txfee_io: 10000,
  min_txfee_sign: 10000,
  min_txfee_memo: 5000,
};

export function testAddress(i: number): string {
  const b = new Uint8Array(32);
  b[0] = 0xa0;
  b[1] = i & 255;
  b[2] = (i >> 8) & 255;
  return encodeAddress(b);
}

export function testTxid(seed: string): string {
  let h = 0x811c9dc5;
  let out = '';
  for (let round = 0; out.length < 64; round++) {
    for (const ch of `${seed}|${round}`) h = Math.imul(h ^ ch.charCodeAt(0), 0x01000193) >>> 0;
    out += h.toString(16).padStart(8, '0');
  }
  return out.slice(0, 64).toUpperCase();
}

export class FakeNode {
  height = 1000;
  synced = true;
  network = 'mainnet';
  readonly calls: string[] = [];
  private counter = 0;
  private readonly entries = new Map<string, Stored[]>();
  failNext: Error | null = null;
  /** When set, /address/history answers like a lagging node at this height. */
  historyHeight: number | null = null;

  add(address: string, e: FakeEntry): void {
    const list = this.entries.get(address) ?? [];
    list.push({
      address,
      height: e.height,
      type: e.type ?? 'RECEIVE',
      amount: e.amount ?? 1_000_000n,
      txid: e.txid ?? testTxid(`${address}|${this.counter}`),
      contract: e.contract ?? NATIVE_CONTRACT,
      time: e.time ?? 1_700_000_000_000 + e.height * 10_000,
      nft: e.nft ?? false,
      counter: this.counter++,
      ...(e.memo !== undefined ? { memo: e.memo } : {}),
    });
    this.entries.set(address, list);
    if (e.height > this.height) this.height = e.height;
  }

  all(address: string): Stored[] {
    return this.entries.get(address) ?? [];
  }

  transport = async (path: string): Promise<unknown> => {
    this.calls.push(path);
    if (this.failNext) {
      const e = this.failNext;
      this.failNext = null;
      throw e;
    }
    const [route, query = ''] = path.split('?');
    const q = new URLSearchParams(query);
    if (route === '/chain/info') return { ...CHAIN_INFO, network: this.network };
    if (route === '/node/info') return { height: this.height, is_synced: this.synced, name: this.network, time_stamp: 1_700_000_000_000 + this.height * 10_000 };
    if (route === '/address/history') {
      const id = q.get('id')!;
      const limit = Number(q.get('limit') ?? 100);
      const since = Number(q.get('since') ?? 0);
      const until = Number(q.get('until') ?? 0xffffffff);
      const visible = Math.min(until, this.historyHeight ?? until);
      return this.history(id, limit, since, visible).map((e) => ({
        address: e.address,
        amount: e.amount.toString(),
        contract: e.contract,
        ...(e.nft ? { is_nft: true } : { decimals: 6, symbol: e.contract === NATIVE_CONTRACT ? 'MMX' : 'TOK' }),
        height: e.height,
        is_native: e.contract === NATIVE_CONTRACT,
        is_pending: false,
        is_validated: false,
        memo: e.memo ?? null,
        time: e.time / 1000,
        time_stamp: e.time,
        txid: e.txid,
        type: e.type,
      }));
    }
    throw new Error(`unexpected path ${path}`);
  };

  /** Node::get_history for one address. */
  private history(address: string, limit: number, since: number, until: number): Stored[] {
    const log = this.all(address)
      .filter((e) => e.height >= since && e.height <= until)
      .sort((a, b) => b.height - a.height || b.counter - a.counter);
    const chunk = 100;
    const res: Stored[] = [];
    let pos = 0;
    let count = 0;
    let last: Stored | undefined;
    for (;;) {
      const part = log.slice(pos, pos + chunk);
      pos += part.length;
      res.push(...part);
      count += part.length;
      if (part.length) last = part[part.length - 1];
      if (part.length < chunk || count >= limit) break;
    }
    if (last) {
      for (const e of log.slice(pos)) if (e.height === last.height) res.push(e);
    }
    const key = (e: Stored) => [e.height, e.txid, e.type, e.contract, e.address, e.memo ?? '', e.amount] as const;
    res.sort((a, b) => {
      const ka = key(a);
      const kb = key(b);
      for (let i = 0; i < ka.length; i++) {
        if (ka[i]! < kb[i]!) return 1;
        if (ka[i]! > kb[i]!) return -1;
      }
      return 0;
    });
    if (limit > 0 && res.length > limit) {
      let i = limit - 1;
      const cutoff = res[i]!.height;
      while (i < res.length && res[i]!.height === cutoff) i++;
      res.length = i;
    }
    return res;
  }
}
