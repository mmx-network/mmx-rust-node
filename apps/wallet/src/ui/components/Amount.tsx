import { formatAmount, MAX_DECIMALS, splitAtomic } from '../../lib/amount';
import { getSeparators } from '../../lib/locale';
import { cx } from './internal/cx';
import { safeLabel } from './internal/text';

export interface AmountProps {
  /** Atomic units. */
  value: bigint;
  decimals: number;
  symbol?: string;
  /**
   * auto: "−" for negative values only; always: also "+" for positive values;
   * never: absolute value.
   */
  sign?: 'auto' | 'always' | 'never';
  size?: 'sm' | 'md' | 'lg' | 'xl' | 'display';
  /** auto: positive tone for signed positive values, otherwise neutral. */
  tone?: 'auto' | 'neutral' | 'muted' | 'positive' | 'negative';
  /** Defaults to the app's locale (src/lib/locale.ts). */
  locale?: string;
  class?: string;
}

const MINUS = '−';

/** The parts of an absolute amount as displayed; invalid decimals count as 0. */
export function amountDisplay(abs: bigint, decimals: number, locale?: string): { integer: string; decimal: string; fraction: string } {
  const places = Number.isInteger(decimals) && decimals >= 0 && decimals <= MAX_DECIMALS ? decimals : 0;
  const value = abs < 0n ? -abs : abs;
  const fraction = splitAtomic(value, places).frac;
  const text = formatAmount(value, places, locale ? { locale, sign: 'never' } : { sign: 'never' });
  const decimal = fraction ? getSeparators(locale).decimal : '';
  return { integer: fraction ? text.slice(0, text.length - fraction.length - decimal.length) : text, decimal, fraction };
}

/**
 * Exact amount display (src/lib/amount.ts formatting: locale grouping and
 * decimal separator, trailing zeros trimmed) with tabular numerals and a
 * quieter fractional part.
 */
export function Amount({
  value,
  decimals,
  symbol,
  sign = 'auto',
  size = 'md',
  tone = 'auto',
  locale,
  class: className,
}: AmountProps) {
  const negative = value < 0n;
  const abs = negative ? -value : value;
  const { integer, decimal, fraction } = amountDisplay(abs, decimals, locale);
  const prefix =
    sign === 'never' || value === 0n ? '' : negative ? MINUS : sign === 'always' ? '+' : '';
  const resolvedTone = tone === 'auto' ? (sign === 'always' && value > 0n ? 'positive' : 'neutral') : tone;
  const sym = symbol ? safeLabel(symbol, 16) : '';
  return (
    <span class={cx('amount', `amount--${size}`, `amount--${resolvedTone}`, className)}>
      <span class="amount__number">
        {prefix ? <span class="amount__sign">{prefix}</span> : null}
        <span class="amount__int">{integer}</span>
        {fraction ? (
          <span class="amount__frac">
            {decimal}
            {fraction}
          </span>
        ) : null}
      </span>
      {sym ? <span class="amount__symbol">{sym}</span> : null}
    </span>
  );
}
