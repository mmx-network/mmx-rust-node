import { afterEach, describe, expect, it } from 'vitest';
import { formatAmount, MAX_ATOMIC, parseAmount, parseAtomic, splitAtomic, toInputString } from '../../src/lib/amount';
import { getSeparators, setLocale } from '../../src/lib/locale';

afterEach(() => setLocale(null));

const NBSP_NARROW = '\u{202f}';
const MINUS = '\u{2212}';

describe('formatAmount', () => {
  it('formats with grouping and trims trailing zeros', () => {
    expect(formatAmount(1_234_500_000n, 6, { locale: 'en-US' })).toBe('1,234.5');
    expect(formatAmount(1_000_000n, 6, { locale: 'en-US' })).toBe('1');
    expect(formatAmount(1n, 6, { locale: 'en-US' })).toBe('0.000001');
    expect(formatAmount(0n, 6, { locale: 'en-US' })).toBe('0');
    expect(formatAmount(123n, 0, { locale: 'en-US' })).toBe('123');
    expect(formatAmount(MAX_ATOMIC, 18, { locale: 'en-US' })).toBe('340,282,366,920,938,463,463.374607431768211455');
  });

  it('uses the locale separators', () => {
    expect(formatAmount(1_234_567_890_000n, 6, { locale: 'de-DE' })).toBe('1.234.567,89');
    expect(formatAmount(1_234_567_890_000n, 6, { locale: 'fr-FR' })).toBe(`1${NBSP_NARROW}234${NBSP_NARROW}567,89`);
    expect(formatAmount(1_234_567_890_000n, 6, { locale: 'en-IN' })).toBe('12,34,567.89');
    expect(formatAmount(1_234_567_890_000n, 6, { locale: 'de-CH' })).toMatch(/^1['\u{2019}]234['\u{2019}]567\.89$/u);
  });

  it('always uses Latin digits', () => {
    expect(formatAmount(1_234_500_000n, 6, { locale: 'ar-EG' })).toMatch(/^1.234.5$/u);
  });

  it('cuts extra fraction digits without rounding up', () => {
    expect(formatAmount(1_999_999n, 6, { locale: 'en-US', maxFractionDigits: 2 })).toBe('1.99');
    expect(formatAmount(1_000_001n, 6, { locale: 'en-US', maxFractionDigits: 2 })).toBe('1');
    expect(formatAmount(999n, 6, { locale: 'en-US', maxFractionDigits: 2 })).toBe('0');
  });

  it('pads to a minimum number of fraction digits', () => {
    expect(formatAmount(1_500_000n, 6, { locale: 'en-US', minFractionDigits: 2 })).toBe('1.50');
    expect(formatAmount(5n, 0, { locale: 'en-US', minFractionDigits: 2 })).toBe('5');
  });

  it('handles signs', () => {
    expect(formatAmount(-1_500_000n, 6, { locale: 'en-US' })).toBe(`${MINUS}1.5`);
    expect(formatAmount(1_500_000n, 6, { locale: 'en-US', sign: 'always' })).toBe('+1.5');
    expect(formatAmount(0n, 6, { locale: 'en-US', sign: 'always' })).toBe('0');
    expect(formatAmount(-1_500_000n, 6, { locale: 'en-US', sign: 'never' })).toBe('1.5');
    expect(formatAmount(-1n, 6, { locale: 'en-US', maxFractionDigits: 2 })).toBe('0');
  });

  it('rejects invalid decimals', () => {
    expect(() => formatAmount(1n, 19)).toThrow();
    expect(() => formatAmount(1n, -1)).toThrow();
    expect(() => formatAmount(1n, 1.5)).toThrow();
  });

  it('splits atomic amounts', () => {
    expect(splitAtomic(1_230_000n, 6)).toEqual({ negative: false, int: 1n, frac: '23' });
    expect(splitAtomic(-5n, 2)).toEqual({ negative: true, int: 0n, frac: '05' });
  });
});

describe('parseAmount', () => {
  const ok = (text: string, decimals = 6, locale = 'en-US') => {
    const r = parseAmount(text, decimals, locale);
    if (!r.ok) throw new Error(`expected ok for ${text}: ${r.error}`);
    return r.value;
  };
  const err = (text: string, decimals = 6, locale = 'en-US') => {
    const r = parseAmount(text, decimals, locale);
    if (r.ok) throw new Error(`expected error for ${text}`);
    return r.error;
  };

  it('parses exact values', () => {
    expect(ok('1')).toBe(1_000_000n);
    expect(ok('1.5')).toBe(1_500_000n);
    expect(ok('0.000001')).toBe(1n);
    expect(ok('.5')).toBe(500_000n);
    expect(ok('5.')).toBe(5_000_000n);
    expect(ok('  12.25  ')).toBe(12_250_000n);
    expect(ok('007')).toBe(7_000_000n);
    expect(ok('0')).toBe(0n);
    expect(ok('123', 0)).toBe(123n);
    expect(ok('340282366920938463463.374607431768211455', 18)).toBe(MAX_ATOMIC);
  });

  it('uses the locale decimal separator only', () => {
    expect(ok('1,5', 6, 'de-DE')).toBe(1_500_000n);
    expect(err('1.5', 6, 'de-DE')).toBe('separator');
    expect(err('1,5', 6, 'en-US')).toBe('separator');
    expect(err('1,000', 6, 'en-US')).toBe('separator');
    expect(err('1.000,5', 6, 'de-DE')).toBe('separator');
    expect(err('1 000', 6, 'fr-FR')).toBe('separator');
    expect(err(`1${NBSP_NARROW}000`, 6, 'fr-FR')).toBe('separator');
    expect(err('1’000', 6, 'de-CH')).toBe('separator');
    expect(err("1'000", 6, 'en-US')).toBe('separator');
    expect(err('1.2.3')).toBe('separator');
  });

  it('accepts "." where the locale separator is neither "." nor ","', () => {
    const { decimal } = getSeparators('ar-EG');
    if (decimal !== '.' && decimal !== ',') {
      expect(ok('1.5', 6, 'ar-EG')).toBe(1_500_000n);
      expect(ok(`1${decimal}5`, 6, 'ar-EG')).toBe(1_500_000n);
    }
  });

  it('rejects scientific notation, signs and other characters', () => {
    expect(err('1e6')).toBe('invalid');
    expect(err('1E-6')).toBe('invalid');
    expect(err('-1')).toBe('invalid');
    expect(err('+1')).toBe('invalid');
    expect(err('0x10')).toBe('invalid');
    expect(err('1 MMX')).toBe('invalid');
    expect(err('١٢')).toBe('invalid');
    expect(err('Infinity')).toBe('invalid');
    expect(err('.')).toBe('invalid');
  });

  it('reports empty input, precision and overflow', () => {
    expect(err('')).toBe('empty');
    expect(err('   ')).toBe('empty');
    expect(err('0.0000001')).toBe('precision');
    expect(err('1.5', 0)).toBe('precision');
    expect(err('1.0000000')).toBe('precision');
    expect(err('340282366920938463463.374607431768211456', 18)).toBe('too_large');
    expect(err('9'.repeat(60))).toBe('too_large');
  });

  it('round-trips with toInputString', () => {
    for (const locale of ['en-US', 'de-DE', 'fr-FR', 'en-IN']) {
      for (const v of [0n, 1n, 10n, 1_000_000n, 123_456_789_012n, MAX_ATOMIC]) {
        const text = toInputString(v, 18, locale);
        expect(parseAmount(text, 18, locale)).toEqual({ ok: true, value: v });
      }
    }
    expect(toInputString(1_234_500_000n, 6, 'de-DE')).toBe('1234,5');
    expect(() => toInputString(-1n, 6)).toThrow();
  });

  it('uses the configured locale by default', () => {
    setLocale('de-DE');
    expect(parseAmount('2,5', 6)).toEqual({ ok: true, value: 2_500_000n });
    expect(formatAmount(2_500_000n, 6)).toBe('2,5');
  });
});

describe('parseAtomic', () => {
  it('accepts exact decimal strings only', () => {
    expect(parseAtomic('0')).toBe(0n);
    expect(parseAtomic('123')).toBe(123n);
    expect(parseAtomic(MAX_ATOMIC.toString())).toBe(MAX_ATOMIC);
    expect(parseAtomic((MAX_ATOMIC + 1n).toString())).toBeNull();
    expect(parseAtomic('-1')).toBeNull();
    expect(parseAtomic('1.0')).toBeNull();
    expect(parseAtomic('')).toBeNull();
    expect(parseAtomic(' 1')).toBeNull();
    expect(parseAtomic(1 as unknown as string)).toBeNull();
  });
});
