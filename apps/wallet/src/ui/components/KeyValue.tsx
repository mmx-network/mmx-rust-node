import type { ComponentChildren } from 'preact';
import { cx } from './internal/cx';

export interface KeyValueListProps {
  /** Card-like rows with dividers (default) or compact rows without. */
  variant?: 'rows' | 'compact';
  class?: string;
  children?: ComponentChildren;
}

/** Label/value rows for review and detail screens (<dl>). */
export function KeyValueList({ variant = 'rows', class: className, children }: KeyValueListProps) {
  return <dl class={cx('kv', `kv--${variant}`, className)}>{children}</dl>;
}

export interface KeyValueProps {
  label: ComponentChildren;
  /** Secondary line under the value. */
  hint?: ComponentChildren;
  /** Stack label above value (for long values such as full addresses). */
  stacked?: boolean;
  /** Emphasise the row (e.g. the total). */
  emphasis?: boolean;
  class?: string;
  children?: ComponentChildren;
}

export function KeyValue({ label, hint, stacked = false, emphasis = false, class: className, children }: KeyValueProps) {
  return (
    <div class={cx('kv__row', stacked && 'kv__row--stacked', emphasis && 'kv__row--emphasis', className)}>
      <dt class="kv__label">{label}</dt>
      <dd class="kv__value">
        {children}
        {hint != null ? <span class="kv__hint">{hint}</span> : null}
      </dd>
    </div>
  );
}
