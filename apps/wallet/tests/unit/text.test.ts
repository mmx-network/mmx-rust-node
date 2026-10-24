import { describe, expect, it } from 'vitest';
import { hasUnsafeCharacters, sanitizeMemo, sanitizeSymbol, sanitizeText, shortenHash, validateName } from '../../src/lib/text';

describe('sanitizeText', () => {
  it('removes bidi overrides, isolates and marks', () => {
    const evil = 'pay\u{202e}exe.txt\u{202c}\u{2066}x\u{2069}\u{200e}\u{200f}\u{61c}';
    expect(sanitizeText(evil)).toBe('payexe.txtx');
  });

  it('removes C0/C1 controls and DEL, turns line breaks into spaces', () => {
    expect(sanitizeText('a\u{0}b\u{7}c\u{7f}d\u{9b}e')).toBe('abcde');
    expect(sanitizeText('line1\nline2\r\n\tx\u{2028}y\u{85}z')).toBe('line1 line2 x y z');
  });

  it('removes zero-width and invisible formatting characters', () => {
    expect(sanitizeText('M\u{200b}M\u{feff}X\u{2060}\u{ad}')).toBe('MMX');
  });

  it('keeps normal text, emoji and joiners', () => {
    expect(sanitizeText('Café ☕ 日本 👩‍💻')).toBe('Café ☕ 日本 👩‍💻');
  });

  it('caps by code points and ignores non-strings', () => {
    expect(sanitizeText('😀'.repeat(10), 3)).toBe('😀😀😀');
    expect(sanitizeText(42)).toBe('');
    expect(sanitizeText(null)).toBe('');
    expect(sanitizeMemo('x'.repeat(100))).toHaveLength(64);
    expect(sanitizeSymbol('  USDM\u{202e}  ')).toBe('USDM');
    expect(sanitizeSymbol('A'.repeat(40))).toHaveLength(24);
  });

  it('detects unsafe characters', () => {
    expect(hasUnsafeCharacters('hello')).toBe(false);
    expect(hasUnsafeCharacters('a\nb')).toBe(true);
    expect(hasUnsafeCharacters('a\u{202e}b')).toBe(true);
    expect(hasUnsafeCharacters('a\u{0}')).toBe(true);
    expect(hasUnsafeCharacters('a\u{200b}')).toBe(true);
  });
});

describe('formatting helpers', () => {
  it('shortens hashes', () => {
    expect(shortenHash('A'.repeat(64))).toBe('AAAAAA\u{2026}AAAAAA');
    expect(shortenHash('ABC')).toBe('ABC');
  });

  it('validates names', () => {
    expect(validateName('  Savings  ')).toBeNull();
    expect(validateName('')).toBe('empty');
    expect(validateName('   ')).toBe('empty');
    expect(validateName('x'.repeat(65))).toBe('too_long');
    expect(validateName('😀'.repeat(64))).toBeNull();
    expect(validateName('a\u{202e}b')).toBe('invalid');
    expect(validateName('a\nb')).toBe('invalid');
    expect(validateName('abc', 2)).toBe('too_long');
  });
});
