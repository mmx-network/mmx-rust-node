import { useRef } from 'preact/hooks';
import { useFieldGroup } from './Field';
import { Icon, type IconName } from './Icon';
import { cx } from './internal/cx';

export interface SegmentOption<T extends string> {
  value: T;
  label: string;
  icon?: IconName;
  /** Show only the icon (label stays the accessible name). */
  iconOnly?: boolean;
  disabled?: boolean;
}

export interface SegmentedControlProps<T extends string> {
  value: T;
  onChange: (value: T) => void;
  options: Array<SegmentOption<T>>;
  /** Accessible name when not inside a Field. */
  label?: string;
  size?: 'sm' | 'md';
  /** Stretch segments to fill the width. */
  block?: boolean;
  class?: string;
}

/** Single choice between a few options (radio semantics, arrow keys change the selection). */
export function SegmentedControl<T extends string>({
  value,
  onChange,
  options,
  label,
  size = 'md',
  block = false,
  class: className,
}: SegmentedControlProps<T>) {
  const field = useFieldGroup();
  const root = useRef<HTMLDivElement>(null);
  const enabled = options.filter((o) => !o.disabled);

  const move = (delta: number): void => {
    const idx = enabled.findIndex((o) => o.value === value);
    const next = enabled[(idx + delta + enabled.length) % enabled.length];
    if (!next) return;
    onChange(next.value);
    const btn = root.current?.querySelector<HTMLButtonElement>(`[data-value="${CSS.escape(next.value)}"]`);
    btn?.focus();
  };

  return (
    <div
      ref={root}
      role="radiogroup"
      class={cx('segmented', `segmented--${size}`, block && 'segmented--block', className)}
      aria-labelledby={label ? undefined : field.labelId}
      aria-label={label}
      onKeyDown={(event) => {
        if (event.key === 'ArrowRight' || event.key === 'ArrowDown') {
          event.preventDefault();
          move(1);
        } else if (event.key === 'ArrowLeft' || event.key === 'ArrowUp') {
          event.preventDefault();
          move(-1);
        }
      }}
    >
      {options.map((o) => {
        const checked = o.value === value;
        return (
          <button
            key={o.value}
            type="button"
            role="radio"
            data-value={o.value}
            class={cx('segmented__item', checked && 'segmented__item--active')}
            aria-checked={checked ? 'true' : 'false'}
            aria-label={o.iconOnly ? o.label : undefined}
            title={o.iconOnly ? o.label : undefined}
            tabIndex={checked || (!options.some((x) => x.value === value) && o === enabled[0]) ? 0 : -1}
            disabled={o.disabled}
            onClick={() => onChange(o.value)}
          >
            {o.icon ? <Icon name={o.icon} size="sm" /> : null}
            {o.iconOnly ? null : <span class="segmented__label">{o.label}</span>}
          </button>
        );
      })}
    </div>
  );
}
