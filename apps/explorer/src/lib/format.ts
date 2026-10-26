import type { Amount, Timed, Transfer } from './types';

const numbers = new Intl.NumberFormat('en-US', { maximumFractionDigits: 12 });
const spaceNumbers = new Intl.NumberFormat('en-US', { minimumFractionDigits: 1, maximumFractionDigits: 1 });
export const number = (value: number | string | null | undefined): string =>
  value == null ? '—' : numbers.format(Number(value));
export const short = (value: string, size = 10): string =>
  value.length > 2 * size + 1 ? `${value.slice(0, size)}…${value.slice(-size)}` : value;
export const percent = (value: number | undefined): string => (value == null ? '—' : `${(value * 100).toFixed(1)}%`);

/** Format integer atoms without first rounding uint128 values through Number. */
export function atoms(value: string | number | undefined, decimals = 6): string | null {
  if (value == null || !Number.isInteger(decimals) || decimals < 0 || decimals > 255) return null;
  if (typeof value === 'number' && !Number.isSafeInteger(value)) return null;
  const raw = String(value);
  if (!/^-?\d+$/.test(raw) || raw.length > 80) return null;
  const negative = raw.startsWith('-');
  const digits = (negative ? raw.slice(1) : raw).replace(/^0+(?=\d)/, '').padStart(decimals + 1, '0');
  const integer = decimals ? digits.slice(0, -decimals) : digits;
  const fraction = decimals ? digits.slice(-decimals).replace(/0+$/, '') : '';
  return `${negative ? '-' : ''}${integer.replace(/\B(?=(\d{3})+(?!\d))/g, ',')}${fraction ? `.${fraction}` : ''}`;
}

export function amount(value: Amount | undefined): string {
  return atoms(value?.amount) ?? (value?.value == null ? '—' : number(value.value));
}
export function transferAmount(value: Transfer): string {
  return (
    atoms(value.amount, value.decimals ?? (value.is_native ? 6 : -1)) ??
    (value.value == null ? '—' : number(value.value))
  );
}
export function timestamp(value: Timed): number | null {
  const stamp = value.time_stamp ?? (value.time == null ? null : value.time * 1000);
  return stamp == null || !Number.isFinite(stamp) ? null : stamp;
}
export function date(value: Timed): string {
  const stamp = timestamp(value);
  return stamp === null ? '—' : new Date(stamp).toLocaleString();
}
export function networkSpace(total: number | undefined): string {
  if (total == null || !Number.isFinite(total) || total < 0) return '—';
  // NetworkInfo.total_space is in GB, as used by the C++ explorer.
  if (total >= 1e9) return `${spaceNumbers.format(total / 1e9)} EB`;
  if (total >= 1e6) return `${spaceNumbers.format(total / 1e6)} PB`;
  if (total >= 1e3) return `${spaceNumbers.format(total / 1e3)} TB`;
  return `${spaceNumbers.format(total)} GB`;
}
