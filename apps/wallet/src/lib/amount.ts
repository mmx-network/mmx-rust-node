/**
 * Exact amounts. Atomic units are always bigint; floats are never used.
 * Display follows the user's locale (grouping and decimal separator) with
 * trailing zeros trimmed. Input accepts digits and the locale's decimal
 * separator only (docs/wallet-app.md §6.5).
 */
import { getLocale, getSeparators } from './locale';

export const MAX_DECIMALS = 18;
/** Largest value representable in a 128-bit unsigned amount. */
export const MAX_ATOMIC = (1n << 128n) - 1n;

const integerFormatters = new Map<string, Intl.NumberFormat>();

function integerFormatter(locale: string, grouping: boolean): Intl.NumberFormat {
  const key = `${locale}|${grouping ? 1 : 0}`;
  let f = integerFormatters.get(key);
  if (!f) {
    f = new Intl.NumberFormat(locale, {
      numberingSystem: 'latn',
      useGrouping: grouping,
      maximumFractionDigits: 0,
    });
    integerFormatters.set(key, f);
  }
  return f;
}

function checkDecimals(decimals: number): void {
  if (!Number.isInteger(decimals) || decimals < 0 || decimals > MAX_DECIMALS) {
    throw new RangeError('invalid decimals');
  }
}

export interface FormatOptions {
  locale?: string;
  /**
   * Show at most this many fraction digits. Extra digits are cut (never
   * rounded up, so a balance is never overstated).
   */
  maxFractionDigits?: number;
  /** Pad the fraction with zeros to at least this many digits. */
  minFractionDigits?: number;
  /** 'always' prefixes positive values with "+". Default 'auto'. */
  sign?: 'auto' | 'always' | 'never';
  /** Default true. */
  grouping?: boolean;
}

const MINUS = '\u{2212}';

/** Splits an atomic amount into integer and fraction digit strings (fraction without trailing zeros). */
export function splitAtomic(atomic: bigint, decimals: number): { negative: boolean; int: bigint; frac: string } {
  checkDecimals(decimals);
  const negative = atomic < 0n;
  const abs = negative ? -atomic : atomic;
  const scale = 10n ** BigInt(decimals);
  const int = abs / scale;
  const frac = decimals === 0 ? '' : (abs % scale).toString().padStart(decimals, '0').replace(/0+$/, '');
  return { negative, int, frac };
}

/** Formats atomic units for display, e.g. 1234500000n (6 decimals) → "1,234.5". */
export function formatAmount(atomic: bigint, decimals: number, options: FormatOptions = {}): string {
  const locale = options.locale ?? getLocale();
  let { negative, int, frac } = splitAtomic(atomic, decimals);
  if (options.maxFractionDigits !== undefined && frac.length > options.maxFractionDigits) {
    frac = frac.slice(0, Math.max(0, options.maxFractionDigits)).replace(/0+$/, '');
  }
  if (options.minFractionDigits !== undefined && frac.length < options.minFractionDigits) {
    frac = frac.padEnd(Math.min(options.minFractionDigits, decimals), '0');
  }
  const isZero = int === 0n && /^0*$/.test(frac);
  let text = integerFormatter(locale, options.grouping ?? true).format(int);
  if (frac) text += getSeparators(locale).decimal + frac;
  const sign = options.sign ?? 'auto';
  if (sign !== 'never' && !isZero) {
    if (negative) text = MINUS + text;
    else if (sign === 'always') text = '+' + text;
  }
  return text;
}

/** Plain text for an amount input: no grouping, locale decimal separator. */
export function toInputString(atomic: bigint, decimals: number, locale = getLocale()): string {
  if (atomic < 0n) throw new RangeError('negative amount');
  return formatAmount(atomic, decimals, { locale, grouping: false, sign: 'never' });
}

export type AmountError =
  /** Nothing entered. */
  | 'empty'
  /** Characters other than digits and the decimal separator. */
  | 'invalid'
  /** A grouping character or the other decimal separator. */
  | 'separator'
  /** More fraction digits than the currency supports. */
  | 'precision'
  /** Larger than a 128-bit amount. */
  | 'too_large';

export type ParseResult = { ok: true; value: bigint } | { ok: false; error: AmountError };

/** Characters that commonly group digits in any locale. */
const GROUPING_CHARS = new Set([',', '.', "'", '\u{2019}', ' ', '\u{a0}', '\u{202f}', '\u{2009}', '_', '\u{66c}']);

/**
 * Parses user input into atomic units. Accepts digits and the locale's
 * decimal separator only; scientific notation, signs, grouping characters and
 * excess precision are rejected.
 */
export function parseAmount(text: string, decimals: number, locale = getLocale()): ParseResult {
  checkDecimals(decimals);
  const input = text.trim();
  if (!input) return { ok: false, error: 'empty' };
  const { decimal, group } = getSeparators(locale);
  const decimalChars = new Set([decimal]);
  if (decimal !== '.' && decimal !== ',') decimalChars.add('.');

  let intDigits = '';
  let fracDigits = '';
  let seenDecimal = false;
  let separatorError = false;
  for (const ch of input) {
    if (ch >= '0' && ch <= '9') {
      if (seenDecimal) fracDigits += ch;
      else intDigits += ch;
    } else if (decimalChars.has(ch)) {
      if (seenDecimal) separatorError = true;
      seenDecimal = true;
    } else if (ch === group || GROUPING_CHARS.has(ch)) {
      separatorError = true;
    } else {
      return { ok: false, error: 'invalid' };
    }
  }
  if (separatorError) return { ok: false, error: 'separator' };
  if (!intDigits && !fracDigits) return { ok: false, error: 'invalid' };
  if (fracDigits.length > decimals) return { ok: false, error: 'precision' };
  const digits = (intDigits + fracDigits.padEnd(decimals, '0')).replace(/^0+(?=\d)/, '');
  if (digits.length > 40) return { ok: false, error: 'too_large' };
  const value = BigInt(digits);
  if (value > MAX_ATOMIC) return { ok: false, error: 'too_large' };
  return { ok: true, value };
}

/** Parses an exact atomic decimal string ("123"), as used by the backend and RPC. */
export function parseAtomic(text: string): bigint | null {
  if (typeof text !== 'string' || !/^\d{1,40}$/.test(text)) return null;
  const value = BigInt(text);
  return value <= MAX_ATOMIC ? value : null;
}
