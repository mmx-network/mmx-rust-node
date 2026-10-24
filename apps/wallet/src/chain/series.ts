/**
 * Balance history as step points: start from the current confirmed balance
 * and walk confirmed rows newest → oldest (RECEIVE and rewards add, SPEND and
 * TXFEE subtract).
 */
import type { HistoryRow } from './types';

export type SeriesRange = '1W' | '1M' | '3M' | '1Y' | 'ALL';

export const SERIES_RANGES: readonly SeriesRange[] = ['1W', '1M', '3M', '1Y', 'ALL'];

const DAY = 86_400_000;

export const RANGE_MS: Record<Exclude<SeriesRange, 'ALL'>, number> = {
  '1W': 7 * DAY,
  '1M': 30 * DAY,
  '3M': 91 * DAY,
  '1Y': 365 * DAY,
};

/** Span shown for "All" when there is no history at all. */
const EMPTY_ALL_SPAN = 7 * DAY;

export interface SeriesPoint {
  /** ms timestamp */
  t: number;
  v: bigint;
}

export interface BalanceSeries {
  contract: string;
  /** Ascending by time; the first point is at `start`, the last at `end`. */
  points: SeriesPoint[];
  start: number;
  end: number;
  /** False when older history was not available (the series starts later than the range). */
  complete: boolean;
  min: bigint;
  max: bigint;
}

export function rowDelta(row: HistoryRow): bigint {
  switch (row.type) {
    case 'SPEND':
    case 'TXFEE':
      return -row.amount;
    default:
      return row.amount;
  }
}

/** Start of a range, or null for "All". */
export function rangeStart(range: SeriesRange, now: number): number | null {
  return range === 'ALL' ? null : now - RANGE_MS[range];
}

/** Estimated timestamp of a block height from the current head. */
export function estimateTime(height: number, head: number, headTimeMs: number, blockIntervalMs: number): number {
  return headTimeMs - Math.max(0, head - height) * blockIntervalMs;
}

export interface SeriesInput {
  /** Rows complete for the wallet (height ≥ frontier); any order. */
  rows: readonly HistoryRow[];
  contract: string;
  /** Current confirmed balance of the contract. */
  current: bigint;
  now: number;
  range: SeriesRange;
  /** All history down to genesis is loaded. */
  historyComplete: boolean;
  /** Time of the frontier (oldest complete height) when history is not complete. */
  frontierTimeMs?: number;
}

interface Block {
  height: number;
  t: number;
  delta: bigint;
}

export function buildSeries(input: SeriesInput): BalanceSeries {
  const { contract, now, range } = input;
  const blocks = new Map<number, Block>();
  for (const row of input.rows) {
    if (row.contract !== contract) continue;
    let b = blocks.get(row.height);
    if (!b) {
      b = { height: row.height, t: row.timeMs, delta: 0n };
      blocks.set(row.height, b);
    }
    b.delta += rowDelta(row);
    if (row.timeMs > b.t) b.t = row.timeMs;
  }
  const ordered = [...blocks.values()].sort((a, b) => b.height - a.height);

  const wanted = rangeStart(range, now);
  const clamp = (v: bigint) => (v < 0n ? 0n : v);
  let balance = clamp(input.current);
  const desc: SeriesPoint[] = [{ t: now, v: balance }];
  let reachedStart = false;
  let earliest: number | undefined;
  for (const b of ordered) {
    if (wanted !== null && b.t < wanted) {
      reachedStart = true;
      break;
    }
    const t = Math.min(b.t, now);
    desc.push({ t, v: balance });
    balance = clamp(balance - b.delta);
    earliest = t;
  }

  let start: number;
  let complete: boolean;
  if (wanted !== null) {
    const covered =
      reachedStart || input.historyComplete || (input.frontierTimeMs !== undefined && input.frontierTimeMs <= wanted);
    if (covered) {
      start = wanted;
      complete = true;
    } else {
      start = Math.max(wanted, Math.min(input.frontierTimeMs ?? earliest ?? now, earliest ?? now));
      complete = false;
    }
  } else if (input.historyComplete) {
    start = earliest ?? now - EMPTY_ALL_SPAN;
    complete = true;
  } else {
    start = Math.min(input.frontierTimeMs ?? earliest ?? now, earliest ?? now);
    complete = false;
  }
  desc.push({ t: start, v: balance });

  const points = desc.reverse();
  let min = points[0]!.v;
  let max = points[0]!.v;
  for (const p of points) {
    if (p.v < min) min = p.v;
    if (p.v > max) max = p.v;
  }
  return { contract, points, start, end: now, complete, min, max };
}

/** Time the history must reach for a range to be complete (null = everything). */
export function coverageTarget(range: SeriesRange, now: number): number | null {
  return rangeStart(range, now);
}
