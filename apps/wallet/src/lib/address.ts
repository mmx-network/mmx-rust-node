/**
 * MMX addresses: Bech32m with HRP "mmx", 32 bytes encoded as a little-endian
 * integer (byte 31 first). Port of crates/mmx-wallet/src/address.rs.
 */

const CHARSET = 'qpzry9x8gf2tvdw0s3jn54khce6mua7l';
const GENERATORS = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3] as const;
const BECH32M_CONST = 0x2bc830a3;
const HRP_EXPANDED = [3, 3, 3, 0, 13, 13, 24]; // "mmx" >> 5, 0, "mmx" & 31

const CHAR_INDEX: Record<string, number> = Object.create(null);
for (let i = 0; i < CHARSET.length; i++) CHAR_INDEX[CHARSET[i]!] = i;

/** Length of an encoded address: "mmx1" + 52 data + 6 checksum characters. */
export const ADDRESS_LENGTH = 62;

/** The native currency (MMX) contract: the all-zero address. */
export const NATIVE_CONTRACT = 'mmx1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqdgytev';

function polymod(values: Iterable<number>): number {
  let chk = 1;
  for (const v of values) {
    const top = chk >>> 25;
    chk = (((chk & 0x1ffffff) << 5) ^ v) >>> 0;
    for (let i = 0; i < 5; i++) {
      if ((top >>> i) & 1) chk = (chk ^ GENERATORS[i]!) >>> 0;
    }
  }
  return chk >>> 0;
}

/** Encodes 32 address bytes. */
export function encodeAddress(bytes: Uint8Array): string {
  if (bytes.length !== 32) throw new RangeError('address must be 32 bytes');
  const data: number[] = [];
  for (let group = 0; group < 52; group++) {
    let value = 0;
    for (let j = 0; j < 5; j++) {
      const bit = group * 5 + j;
      const b = bit < 256 ? (bytes[31 - (bit >> 3)]! >> (7 - (bit & 7))) & 1 : 0;
      value = (value << 1) | b;
    }
    data.push(value);
  }
  const checksum = (polymod([...HRP_EXPANDED, ...data, 0, 0, 0, 0, 0, 0]) ^ BECH32M_CONST) >>> 0;
  for (let i = 5; i >= 0; i--) data.push((checksum >>> (5 * i)) & 31);
  let out = 'mmx1';
  for (const v of data) out += CHARSET[v];
  return out;
}

/**
 * Decodes an address to its 32 bytes, or null when it is not a valid MMX
 * address. Accepts all-lowercase or all-uppercase input (Bech32 rules).
 */
export function decodeAddress(input: string): Uint8Array | null {
  if (typeof input !== 'string' || input.length !== ADDRESS_LENGTH) return null;
  const lower = input.toLowerCase();
  if (lower !== input && input.toUpperCase() !== input) return null;
  if (!lower.startsWith('mmx1')) return null;
  const data: number[] = [];
  for (let i = 4; i < lower.length; i++) {
    const v = CHAR_INDEX[lower[i]!];
    if (v === undefined) return null;
    data.push(v);
  }
  if (polymod([...HRP_EXPANDED, ...data]) !== BECH32M_CONST || (data[51]! & 15) !== 0) return null;
  const bytes = new Uint8Array(32);
  for (let bit = 0; bit < 256; bit++) {
    const b = (data[Math.floor(bit / 5)]! >> (4 - (bit % 5))) & 1;
    bytes[31 - (bit >> 3)]! |= b << (7 - (bit & 7));
  }
  return bytes;
}

/** Canonical lowercase form of a valid address, or null. Surrounding whitespace is ignored. */
export function normalizeAddress(input: string): string | null {
  if (typeof input !== 'string') return null;
  const trimmed = input.trim();
  return decodeAddress(trimmed) ? trimmed.toLowerCase() : null;
}
