import { cx } from './internal/cx';

export interface SkeletonProps {
  /** CSS width, e.g. "60%" or "8rem" (default 100%). */
  width?: string;
  /** CSS height (default: one line of text). */
  height?: string;
  /** Number of text lines; the last one is shorter. */
  lines?: number;
  shape?: 'text' | 'rect' | 'circle';
  class?: string;
}

/** Loading placeholder. Mark the loading container with aria-busy="true". */
export function Skeleton({ width, height, lines = 1, shape = 'text', class: className }: SkeletonProps) {
  if (lines > 1) {
    return (
      <span class={cx('skeleton-lines', className)} aria-hidden="true">
        {Array.from({ length: lines }, (_, i) => (
          <span key={i} class="skeleton skeleton--text" style={{ width: i === lines - 1 ? '62%' : width ?? '100%' }} />
        ))}
      </span>
    );
  }
  return (
    <span
      class={cx('skeleton', `skeleton--${shape}`, className)}
      style={{ width: width ?? (shape === 'circle' ? height ?? '2rem' : '100%'), height }}
      aria-hidden="true"
    />
  );
}
