import { describe, expect, it } from 'vitest';
import { decodeAddress, encodeAddress, NATIVE_CONTRACT, normalizeAddress } from '../../src/lib/address';

const hex = (h: string) => Uint8Array.from(h.match(/../g)!.map((b) => parseInt(b, 16)));
const isValid = (s: string) => decodeAddress(s) !== null;
const toHex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');

// Cross-checked against crates/mmx-wallet/tests/fixtures/reference.json (byte arrays ↔ strings).
const VECTORS: Array<[string, string]> = [
  ['0000000000000000000000000000000000000000000000000000000000000000', NATIVE_CONTRACT],
  ['d11112c1ce883369c9a32e941f2a3d6cf7a77fad0165d9165bbb6029afb426ce', 'mmx1ecntftefvza4k9kev5q66la87akr62sljsh28jtfxwyvasgjz8gswl046q'],
  ['5f2fb506d49085443d55b6e034ef8174d6df079cc03b731449120b7226305e68', 'mmx1dp0rqfnjpvfyj9rn80qfcp7l6e6grme5uzm9202yskgdgp449a0snelu0z'],
  ['c2b4808a178783fe3959c0b4e9adcdb77ff02e96e1f7749f096a0a894901a1c2', 'mmx1c2sszjvfpf4qn8m57lsevths07mumt0fknq9jw07swr30z5qknpqp29vvx'],
];

const CHARSET = 'qpzry9x8gf2tvdw0s3jn54khce6mua7l';

describe('address', () => {
  it.each(VECTORS)('encodes %s', (bytes, address) => {
    expect(encodeAddress(hex(bytes))).toBe(address);
  });

  it.each(VECTORS)('decodes %s', (bytes, address) => {
    expect(toHex(decodeAddress(address)!)).toBe(bytes);
    expect(toHex(decodeAddress(address.toUpperCase())!)).toBe(bytes);
  });

  it('round-trips random bytes', () => {
    for (let i = 0; i < 200; i++) {
      const b = new Uint8Array(32);
      crypto.getRandomValues(b);
      const a = encodeAddress(b);
      expect(a).toHaveLength(62);
      expect(toHex(decodeAddress(a)!)).toBe(toHex(b));
    }
  });

  it('detects every single-character substitution', () => {
    const a = VECTORS[1]![1];
    for (let i = 4; i < a.length; i++) {
      for (const c of CHARSET) {
        if (c === a[i]) continue;
        const mutated = a.slice(0, i) + c + a.slice(i + 1);
        expect(isValid(mutated)).toBe(false);
      }
    }
  });

  it('rejects malformed input', () => {
    const a = VECTORS[2]![1];
    expect(isValid('')).toBe(false);
    expect(isValid(a.slice(0, -1))).toBe(false);
    expect(isValid(a + 'q')).toBe(false);
    expect(isValid('MMX')).toBe(false);
    expect(isValid('btc1' + a.slice(4))).toBe(false);
    expect(isValid('mmx2' + a.slice(4))).toBe(false);
    expect(isValid(a.slice(0, 10) + a.slice(10).toUpperCase())).toBe(false);
    expect(isValid(a.replace(/q/, 'b'))).toBe(false); // 'b' is not in the charset
    expect(isValid(` ${a}`)).toBe(false);
    expect(isValid(undefined as unknown as string)).toBe(false);
  });

  it('rejects non-zero padding bits', () => {
    // Character 51 of the data part carries 1 data bit + 4 padding bits.
    const bytes = new Uint8Array(32);
    const valid = encodeAddress(bytes);
    expect(isValid(valid)).toBe(true);
    // Re-encode with padding bits set and a recomputed checksum must still fail.
    const data = valid.slice(4, 56);
    const tampered = data.slice(0, 51) + CHARSET[1];
    const chk = checksum(tampered);
    expect(isValid('mmx1' + tampered + chk)).toBe(false);
  });

  it('normalizes and compares', () => {
    const a = VECTORS[3]![1];
    expect(normalizeAddress(`  ${a.toUpperCase()}\n`)).toBe(a);
    expect(normalizeAddress('nope')).toBeNull();
    expect(normalizeAddress(a.toUpperCase())).toBe(normalizeAddress(a));
  });
});

// Independent Bech32m checksum for the padding test.
function checksum(data: string): string {
  const values = [3, 3, 3, 0, 13, 13, 24, ...Array.from(data, (c) => CHARSET.indexOf(c)), 0, 0, 0, 0, 0, 0];
  const G = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];
  let chk = 1;
  for (const v of values) {
    const top = chk >>> 25;
    chk = (((chk & 0x1ffffff) << 5) ^ v) >>> 0;
    for (let i = 0; i < 5; i++) if ((top >>> i) & 1) chk = (chk ^ G[i]!) >>> 0;
  }
  chk = (chk ^ 0x2bc830a3) >>> 0;
  let out = '';
  for (let i = 5; i >= 0; i--) out += CHARSET[(chk >>> (5 * i)) & 31];
  return out;
}
