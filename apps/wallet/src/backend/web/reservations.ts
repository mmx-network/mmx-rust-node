/** Public pending funds only; no signed transactions or signing material. */
import { BackendError } from '../types';
import { normalizeAddress } from '../../lib/address';
import type { StorageLike } from './store';

const MAX_RESERVATIONS = 256;
const MAX_BYTES = 2_000_000;
export interface Reservation {
  network: string;
  txid: string;
  expires: number;
  sender: string;
  /** Static fee for a transfer, maximum fee for a contract transaction. */
  senderFee: bigint;
  inputs: Array<{ address: string; contract: string; amount: bigint }>;
}
function record(v: unknown): v is Record<string, unknown> {
  return !!v && typeof v === 'object' && !Array.isArray(v);
}
function address(v: unknown): v is string {
  return typeof v === 'string' && normalizeAddress(v) === v;
}
function amount(v: unknown): v is string {
  return typeof v === 'string' && /^\d{1,39}$/u.test(v) && BigInt(v) < 1n << 128n;
}
/** One bounded value per wallet and RPC endpoint. Every mutation reads fresh
 * storage so a later popup retains reservations made by an earlier signer. */
export class ReservationStore {
  constructor(private readonly storage: StorageLike, private readonly rpcUrl: string) {}
  private key(walletId: string) {
    return `mmx-wallet/pending/v1/${encodeURIComponent(this.rpcUrl)}/${walletId}`;
  }
  list(walletId: string): Reservation[] {
    try {
      const raw = this.storage.getItem(this.key(walletId));
      if (raw === null) return [];
      if (raw.length > MAX_BYTES) throw new Error();
      const rows: unknown = JSON.parse(raw);
      if (!Array.isArray(rows) || rows.length > MAX_RESERVATIONS) throw new Error();
      const seen = new Set<string>();
      return rows.map((r: unknown) => {
        if (!record(r) || typeof r.network !== 'string' || !r.network.length || r.network.length > 256 ||
          typeof r.txid !== 'string' || !/^[A-F0-9]{64}$/u.test(r.txid) || seen.has(r.txid) ||
          typeof r.expires !== 'number' || !Number.isInteger(r.expires) || r.expires < 0 || r.expires > 0xffff_ffff ||
          !address(r.sender) || !amount(r.senderFee) || !Array.isArray(r.inputs) || r.inputs.length > 1024) throw new Error();
        seen.add(r.txid);
        return { network: r.network, txid: r.txid, expires: r.expires, sender: r.sender, senderFee: BigInt(r.senderFee),
          inputs: r.inputs.map((i: unknown) => {
            if (!record(i) || !address(i.address) || !address(i.contract) || !amount(i.amount)) throw new Error();
            return {address:i.address,contract:i.contract,amount:BigInt(i.amount)};
          }) };
      });
    } catch { throw new BackendError('io_error'); }
  }
  private save(walletId: string, rows: Reservation[]) {
    try {
      const raw = JSON.stringify(rows, (_, v: unknown) => typeof v === 'bigint' ? v.toString() : v);
      if (rows.length > MAX_RESERVATIONS || raw.length > MAX_BYTES) throw new Error();
      if (rows.length) this.storage.setItem(this.key(walletId), raw);
      else this.storage.removeItem(this.key(walletId));
    } catch { throw new BackendError('io_error'); }
  }
  put(walletId: string, reservation: Reservation) {
    this.save(walletId, [...this.list(walletId).filter((r) => r.txid !== reservation.txid), reservation]);
  }
  remove(walletId: string, txid: string) {
    this.save(walletId, this.list(walletId).filter((r) => r.txid !== txid));
  }
  release(walletId: string, network: string, included: ReadonlySet<string>, height: number) {
    const before = this.list(walletId);
    const after = before.filter((r) => r.network !== network || (!included.has(r.txid) && r.expires >= height));
    if (before.length !== after.length) this.save(walletId, after);
  }
  removeWallet(walletId: string) {
    try { this.storage.removeItem(this.key(walletId)); }
    catch { throw new BackendError('io_error'); }
  }
}
