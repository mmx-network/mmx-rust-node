import type { ComponentChildren } from 'preact';
import { useId } from 'preact/hooks';
import { cx } from './internal/cx';

export interface OptionRowProps {
  title: ComponentChildren;
  /** One short line under the title. */
  description?: ComponentChildren;
  /** The control or action (Switch, Select, NumberStepper, Button …). */
  children?: ComponentChildren;
  /** Control below the text instead of beside it (wide controls). */
  stacked?: boolean;
  /** Tone for destructive rows (e.g. Remove wallet). */
  tone?: 'default' | 'danger';
  class?: string;
}

/**
 * Settings-style row: title and description on the start side, a control on
 * the end side. Consecutive rows are separated by dividers; wrap them in a Card.
 * The control is labelled by the title (aria-labelledby on the group).
 */
export function OptionRow({ title, description, children, stacked = false, tone = 'default', class: className }: OptionRowProps) {
  const auto = useId();
  const titleId = `or${auto}-t`;
  const descId = `or${auto}-d`;
  return (
    <div
      class={cx('option-row', stacked && 'option-row--stacked', tone === 'danger' && 'option-row--danger', className)}
      role="group"
      aria-labelledby={titleId}
      aria-describedby={description != null ? descId : undefined}
    >
      <div class="option-row__text">
        <p class="option-row__title" id={titleId}>
          {title}
        </p>
        {description != null ? (
          <p class="option-row__description" id={descId}>
            {description}
          </p>
        ) : null}
      </div>
      {children != null ? <div class="option-row__control">{children}</div> : null}
    </div>
  );
}
