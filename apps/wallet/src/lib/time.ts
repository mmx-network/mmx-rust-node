/**
 * Date and time formatting. Absolute dates use the user's locale; relative
 * phrases are English to match the UI copy.
 */
import { getLocale } from './locale';

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

const formatterCache = new Map<string, Intl.DateTimeFormat>();

function dateFormatter(locale: string, options: Intl.DateTimeFormatOptions): Intl.DateTimeFormat {
  const key = `${locale}|${JSON.stringify(options)}`;
  let f = formatterCache.get(key);
  if (!f) {
    f = new Intl.DateTimeFormat(locale, options);
    formatterCache.set(key, f);
  }
  return f;
}

function startOfDay(ms: number): number {
  const d = new Date(ms);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** Whole calendar days between two timestamps in local time (b − a). */
export function calendarDayDiff(a: number, b: number): number {
  return Math.round((startOfDay(b) - startOfDay(a)) / DAY);
}

/** "Just now", "5 min ago", "3 h ago", "Yesterday", "Mar 4", "Mar 4, 2025". */
export function formatRelative(ms: number, now = Date.now(), locale = getLocale()): string {
  const diff = now - ms;
  if (diff < MINUTE) return 'Just now';
  if (diff < HOUR) return `${Math.floor(diff / MINUTE)} min ago`;
  const days = calendarDayDiff(ms, now);
  if (days <= 0) return `${Math.floor(diff / HOUR)} h ago`;
  if (days === 1) return 'Yesterday';
  return formatDate(ms, now, locale);
}

/** Date without time; the year is shown only when it differs from the current year. */
export function formatDate(ms: number, now = Date.now(), locale = getLocale()): string {
  const sameYear = new Date(ms).getFullYear() === new Date(now).getFullYear();
  return dateFormatter(
    locale,
    sameYear ? { month: 'short', day: 'numeric' } : { year: 'numeric', month: 'short', day: 'numeric' },
  ).format(ms);
}

/** Full date and time, e.g. "Mar 4, 2025, 14:05". */
export function formatDateTime(ms: number, locale = getLocale()): string {
  return dateFormatter(locale, { dateStyle: 'medium', timeStyle: 'short' }).format(ms);
}

/** Time of day, e.g. "14:05" or "2:05 PM". */
export function formatTime(ms: number, locale = getLocale()): string {
  return dateFormatter(locale, { timeStyle: 'short' }).format(ms);
}

/** Heading for a day group in a list: "Today", "Yesterday", "Monday" (within a week) or a date. */
export function formatDayHeading(ms: number, now = Date.now(), locale = getLocale()): string {
  const days = calendarDayDiff(ms, now);
  if (days <= 0) return 'Today';
  if (days === 1) return 'Yesterday';
  if (days < 7) return dateFormatter(locale, { weekday: 'long' }).format(ms);
  return formatDate(ms, now, locale);
}

/** Stable key for grouping timestamps by local calendar day. */
export function dayKey(ms: number): string {
  const d = new Date(ms);
  return `${d.getFullYear()}-${d.getMonth() + 1}-${d.getDate()}`;
}

export const TIME = { MINUTE, HOUR, DAY } as const;
