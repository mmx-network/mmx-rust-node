import type { FeeLevel, WalletInfo } from '../backend/types';
import { validMessage } from './message';
export { websiteOrigin as origin } from '../lib/websites';

export const PROTOCOL = 'mmx-wallet';
export const VERSION = 2;
export const CHANNEL = 'mmx-wallet-connect/v2';
export const REQUEST_TIMEOUT = 120_000;
export const MAX_JSON = 65_536;
export interface Account {
  walletId: string;
  address: string;
  addresses: string[];
  network: string;
  decimals?: number;
}
export interface TransactionRequest {
  network: string;
  transaction: Record<string, unknown>;
  feeLevel?: FeeLevel;
  gasLimit?: number;
}
export interface PreparedTransaction {
  txid: string;
  fee: string;
  maxFee: string;
  expires: number;
  height: number;
  network: string;
  decimals: number;
  /** Canonical signed transaction with all signing solutions removed. */
  transaction: Record<string, unknown>;
  currencies: Array<{ contract: string; symbol: string; decimals: number }>;
}
export interface Offer {
  host: string;
  kind: 'wallet' | 'approval';
  lock: number;
  wallet: Pick<WalletInfo, 'id' | 'name' | 'addressCount' | 'locked' | 'canView'>;
}
export interface Envelope {
  protocol: 'mmx-wallet';
  version: 2;
  channel: string;
  type: string;
  id?: string;
  [key: string]: unknown;
}
export class ConnectionError extends Error {
  constructor(
    readonly code: string,
    message = code,
  ) {
    super(message);
  }
}
export function object(v: unknown): v is Record<string, unknown> {
  return !!v && typeof v === 'object' && !Array.isArray(v);
}
export function token(v: unknown): v is string {
  return typeof v === 'string' && /^[a-f0-9]{32}$/u.test(v);
}
export function randomToken(): string {
  return Array.from(crypto.getRandomValues(new Uint8Array(16)), (b) => b.toString(16).padStart(2, '0')).join('');
}
/** Reject cyclic/non-JSON messages, large trees and lossy JavaScript integers. */
export function boundedJson(value: unknown, max = MAX_JSON): boolean {
  const seen = new Set<object>();
  let left = 4096;
  const visit = (v: unknown, depth: number): boolean => {
    if (depth > 32 || --left < 0) return false;
    if (v === null || typeof v === 'boolean') return true;
    if (typeof v === 'string') return v.length <= max;
    if (typeof v === 'number') return Number.isFinite(v) && Math.abs(v) <= Number.MAX_SAFE_INTEGER;
    if (typeof v !== 'object' || seen.has(v)) return false;
    seen.add(v);
    const valid = Array.isArray(v)
      ? v.every((x) => visit(x, depth + 1))
      : Object.getPrototypeOf(v) === Object.prototype && Object.values(v).every((x) => visit(x, depth + 1));
    seen.delete(v);
    return valid;
  };
  if (!visit(value, 0)) return false;
  try {
    return new TextEncoder().encode(JSON.stringify(value)).length <= max;
  } catch {
    return false;
  }
}
export function envelope(v: unknown, channel: string): v is Envelope {
  return (
    object(v) &&
    v.protocol === PROTOCOL &&
    v.version === VERSION &&
    v.channel === channel &&
    typeof v.type === 'string' &&
    boundedJson(v, MAX_JSON + 4096)
  );
}
export function transactionRequest(v: unknown): v is TransactionRequest {
  return (
    object(v) &&
    typeof v.network === 'string' &&
    v.network.length > 0 &&
    v.network.length <= 256 &&
    object(v.transaction) &&
    boundedJson(v.transaction) &&
    (v.feeLevel === undefined || [1, 2, 3, 5, 10].includes(v.feeLevel as number)) &&
    (v.gasLimit === undefined || (Number.isInteger(v.gasLimit) && (v.gasLimit as number) >= 0 && (v.gasLimit as number) <= 0xffff_ffff)) &&
    Object.keys(v).every((k) => ['network', 'transaction', 'feeLevel', 'gasLimit'].includes(k))
  );
}

export interface MessageRequest { network: string; message: string }
export interface MessageProof extends MessageRequest { origin: string; address: string; public_key: string; signature: string }
export function messageRequest(v: unknown): v is MessageRequest {
  return object(v) && typeof v.network === 'string' && v.network.length > 0 && v.network.length <= 64
    && validMessage(v.message)
    && Object.keys(v).every(k => ['network', 'message'].includes(k));
}
