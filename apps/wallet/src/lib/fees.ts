/** Network fee priority levels (fee_ratio = level × 1024). */
import type { FeeLevel } from '../backend/types';

export interface FeeOption {
  level: FeeLevel;
  label: string;
  /** "1×", "2×", … */
  multiplier: string;
}

export const FEE_LEVELS: readonly FeeLevel[] = [1, 2, 3, 5, 10];

export const FEE_OPTIONS: readonly FeeOption[] = [
  { level: 1, label: 'Standard', multiplier: '1\u{d7}' },
  { level: 2, label: 'Faster', multiplier: '2\u{d7}' },
  { level: 3, label: 'Priority', multiplier: '3\u{d7}' },
  { level: 5, label: 'High priority', multiplier: '5\u{d7}' },
  { level: 10, label: 'Fastest', multiplier: '10\u{d7}' },
];

export const DEFAULT_FEE_LEVEL: FeeLevel = 1;

export function isFeeLevel(value: unknown): value is FeeLevel {
  return typeof value === 'number' && (FEE_LEVELS as readonly number[]).includes(value);
}

export function feeOption(level: FeeLevel): FeeOption {
  return FEE_OPTIONS.find((o) => o.level === level) ?? FEE_OPTIONS[0]!;
}

/**
 * Fee levels the chain accepts: the intersection of FEE_LEVELS with the
 * chain's `min_fee_ratio` list (all levels when the list is unavailable).
 */
export function availableFeeLevels(minFeeRatio: readonly number[] | undefined): FeeLevel[] {
  if (!minFeeRatio || minFeeRatio.length === 0) return [...FEE_LEVELS];
  const allowed = FEE_LEVELS.filter((l) => minFeeRatio.includes(l));
  return allowed.length > 0 ? allowed : [...FEE_LEVELS];
}
