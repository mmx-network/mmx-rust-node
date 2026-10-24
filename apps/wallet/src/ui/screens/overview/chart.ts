/**
 * Geometry for the balance chart: value scale, ticks, step paths and lookups.
 * Values are converted to numbers for plotting only; readouts use the exact
 * bigint amounts.
 */
import { getLocale } from '../../../lib/locale';

/** Atomic units → display number (plotting only). */
export function toDisplayNumber(atomic: bigint, decimals: number): number {
  const scale = 10n ** BigInt(Math.max(0, Math.min(18, Math.trunc(decimals))));
  const int = atomic / scale;
  const frac = atomic % scale;
  return Number(int) + Number(frac) / Number(scale);
}

/** A "nice" step (1, 2, 2.5, 5 × 10^n) splitting `range` into about `count` parts. */
export function niceStep(range: number, count: number): number {
  if (!(range > 0) || !Number.isFinite(range)) return 1;
  const raw = range / Math.max(1, count);
  const mag = 10 ** Math.floor(Math.log10(raw));
  const norm = raw / mag;
  const nice = norm <= 1 ? 1 : norm <= 2 ? 2 : norm <= 2.5 ? 2.5 : norm <= 5 ? 5 : 10;
  return nice * mag;
}

export interface ValueScale {
  lo: number;
  hi: number;
  step: number;
  ticks: number[];
}

function ticksFor(lo: number, hi: number, step: number): number[] {
  const out: number[] = [];
  for (let i = 0; i <= 12; i++) {
    const v = lo + i * step;
    if (v > hi + step / 1000) break;
    out.push(Number(v.toPrecision(12)));
  }
  return out;
}

/**
 * Value axis for balances (never below zero). Starts at zero when the values
 * are close to it relative to their spread; otherwise fits the data so small
 * changes of a large balance stay visible. A flat series sits at about two
 * thirds of the height.
 */
export function valueScale(min: number, max: number, count = 3): ValueScale {
  const lo0 = Math.max(0, Math.min(min, max));
  const hi0 = Math.max(0, max, min);
  if (!(hi0 > lo0)) {
    if (!(hi0 > 0)) return { lo: 0, hi: 1, step: 1, ticks: [0] };
    const step = niceStep(hi0 * 1.5, count);
    const hi = Math.max(step, Math.ceil((hi0 * 1.5) / step) * step);
    return { lo: 0, hi, step, ticks: ticksFor(0, hi, step) };
  }
  const span = hi0 - lo0;
  if (lo0 <= span * 0.6) {
    const step = niceStep(hi0, count);
    const hi = Math.max(step, Math.ceil(hi0 / step) * step);
    return { lo: 0, hi, step, ticks: ticksFor(0, hi, step) };
  }
  const pad = span * 0.12;
  const step = niceStep(span + 2 * pad, count);
  const lo = Math.max(0, Math.floor((lo0 - pad) / step) * step);
  const hi = Math.ceil((hi0 + pad) / step) * step;
  return { lo, hi: hi > lo ? hi : lo + step, step, ticks: ticksFor(lo, hi > lo ? hi : lo + step, step) };
}

const tickFormats = new Map<string, Intl.NumberFormat>();

function tickFormat(locale: string, compact: boolean, digits: number): Intl.NumberFormat {
  const key = `${locale}|${compact ? 1 : 0}|${digits}`;
  let f = tickFormats.get(key);
  if (!f) {
    f = new Intl.NumberFormat(locale, {
      numberingSystem: 'latn',
      notation: compact ? 'compact' : 'standard',
      maximumFractionDigits: digits,
      minimumFractionDigits: 0,
    });
    tickFormats.set(key, f);
  }
  return f;
}

/** Short axis label that still tells neighbouring ticks apart ("12.5K", "0.25"). */
export function formatTick(value: number, step: number, locale = getLocale()): string {
  if (value === 0) return tickFormat(locale, false, 0).format(0);
  const abs = Math.abs(value);
  if (abs >= 10_000) {
    const unit = abs >= 1e12 ? 1e12 : abs >= 1e9 ? 1e9 : abs >= 1e6 ? 1e6 : 1e3;
    const digits = Math.max(0, Math.min(3, Math.ceil(Math.log10(unit / step) - 1e-9)));
    return tickFormat(locale, true, digits).format(value);
  }
  const digits = step >= 1 ? 0 : Math.max(0, Math.min(6, Math.ceil(-Math.log10(step) - 1e-9)));
  return tickFormat(locale, false, digits).format(value);
}

export type TickUnit = 'hour' | 'day' | 'month';

export interface TimeTicks {
  unit: TickUnit;
  ticks: number[];
}

const HOUR_MS = 3_600_000;
const DAY_MS = 86_400_000;
const HOUR_STEPS = [1, 2, 3, 6, 12];
const DAY_STEPS = [1, 2, 3, 7, 14];
const MONTH_STEPS = [1, 2, 3, 6, 12, 24, 60, 120];

function collect(start: number, end: number, first: Date, advance: (d: Date) => void): number[] {
  const out: number[] = [];
  const d = new Date(first.getTime());
  for (let guard = 0; guard < 400 && d.getTime() <= end; guard++) {
    if (d.getTime() >= start) out.push(d.getTime());
    advance(d);
  }
  return out;
}

/**
 * Time axis ticks on calendar boundaries in local time: hours for spans up
 * to two days, days up to about ten weeks, months beyond. Picks the smallest
 * step that keeps at most `maxCount` ticks.
 */
export function timeTicks(start: number, end: number, maxCount: number): TimeTicks {
  const max = Math.max(1, Math.floor(maxCount));
  const span = end - start;
  if (!(span > 0)) return { unit: 'day', ticks: [] };
  if (span <= 2 * DAY_MS) {
    for (const h of HOUR_STEPS) {
      if (span / (h * HOUR_MS) > max + 1) continue;
      const first = new Date(start);
      first.setMinutes(0, 0, 0);
      while (first.getTime() < start || first.getHours() % h !== 0) first.setHours(first.getHours() + 1);
      const ticks = collect(start, end, first, (d) => d.setHours(d.getHours() + h));
      if (ticks.length <= max) return { unit: 'hour', ticks };
    }
  }
  if (span <= 75 * DAY_MS) {
    for (const k of DAY_STEPS) {
      if (span / (k * DAY_MS) > max + 1) continue;
      const first = new Date(start);
      first.setHours(0, 0, 0, 0);
      if (first.getTime() < start) first.setDate(first.getDate() + 1);
      const ticks = collect(start, end, first, (d) => d.setDate(d.getDate() + k));
      if (ticks.length <= max) return { unit: 'day', ticks };
    }
  }
  for (const k of MONTH_STEPS) {
    if (span / (k * 30 * DAY_MS) > max + 1) continue;
    const first = new Date(start);
    first.setHours(0, 0, 0, 0);
    first.setDate(1);
    if (first.getTime() < start) first.setMonth(first.getMonth() + 1);
    while ((first.getFullYear() * 12 + first.getMonth()) % k !== 0) first.setMonth(first.getMonth() + 1);
    const ticks = collect(start, end, first, (d) => d.setMonth(d.getMonth() + k));
    if (ticks.length <= max) return { unit: 'month', ticks };
  }
  return { unit: 'month', ticks: [] };
}

const dateFormats = new Map<string, Intl.DateTimeFormat>();

function dateFormat(locale: string, options: Intl.DateTimeFormatOptions): Intl.DateTimeFormat {
  const key = `${locale}|${JSON.stringify(options)}`;
  let f = dateFormats.get(key);
  if (!f) {
    f = new Intl.DateTimeFormat(locale, options);
    dateFormats.set(key, f);
  }
  return f;
}

/** Label of a time tick: "14:00", "Oct 18", "Oct", or the year for January. */
export function formatTimeTick(t: number, unit: TickUnit, locale = getLocale()): string {
  if (unit === 'hour') return dateFormat(locale, { hour: 'numeric', minute: '2-digit' }).format(t);
  if (unit === 'day') return dateFormat(locale, { month: 'short', day: 'numeric' }).format(t);
  const d = new Date(t);
  if (d.getMonth() === 0) return String(d.getFullYear());
  return dateFormat(locale, { month: 'short' }).format(t);
}

/** Index of the last point at or before `t` (step semantics); 0 when `t` precedes all points. */
export function indexAt(points: readonly { t: number }[], t: number): number {
  let lo = 0;
  let hi = points.length - 1;
  if (hi < 0) return -1;
  if (t < points[0]!.t) return 0;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (points[mid]!.t <= t) lo = mid;
    else hi = mid - 1;
  }
  return lo;
}

function num(n: number): string {
  const r = Math.round(n * 10) / 10;
  return Object.is(r, -0) ? '0' : String(r);
}

/**
 * Step line through pixel points (the value holds until the next point),
 * extended horizontally to `endX`. Points falling into the same half-pixel
 * column are collapsed to their vertical extent, so long histories stay light.
 */
export function stepPath(xs: readonly number[], ys: readonly number[], endX: number): string {
  const n = Math.min(xs.length, ys.length);
  if (n === 0) return '';
  const col = (x: number) => Math.round(x * 2) / 2;
  let d = `M${num(col(xs[0]!))} ${num(ys[0]!)}`;
  let i = 1;
  let lastX = col(xs[0]!);
  let lastY = ys[0]!;
  while (i < n) {
    const x = col(xs[i]!);
    let lo = Math.min(lastY, ys[i]!);
    let hi = Math.max(lastY, ys[i]!);
    let y = ys[i]!;
    i++;
    while (i < n && col(xs[i]!) === x) {
      y = ys[i]!;
      if (y < lo) lo = y;
      if (y > hi) hi = y;
      i++;
    }
    if (x !== lastX) d += `H${num(x)}`;
    // Walk from the previous value over the column's full extent and end at its last value.
    if (lo === Math.min(lastY, y) && hi === Math.max(lastY, y)) {
      if (y !== lastY) d += `V${num(y)}`;
    } else if (y === hi) {
      d += `V${num(lo)}V${num(hi)}`;
    } else if (y === lo) {
      d += `V${num(hi)}V${num(lo)}`;
    } else {
      d += `V${num(lo)}V${num(hi)}V${num(y)}`;
    }
    lastX = x;
    lastY = y;
  }
  const ex = col(endX);
  if (ex > lastX) d += `H${num(ex)}`;
  return d;
}
