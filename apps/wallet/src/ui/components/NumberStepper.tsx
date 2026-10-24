import { useEffect, useState } from 'preact/hooks';
import { useFieldControl } from './Field';
import { IconButton } from './IconButton';
import { cx } from './internal/cx';

export interface NumberStepperProps {
  value: number;
  onChange: (value: number) => void;
  min: number;
  max: number;
  step?: number;
  /** Accessible name when not inside a Field. */
  label?: string;
  disabled?: boolean;
  id?: string;
  invalid?: boolean;
  class?: string;
}

const clamp = (n: number, min: number, max: number): number => Math.min(max, Math.max(min, n));

/** Integer input with − / + buttons. Typed values in [min, max] apply at once; others are clamped on blur or Enter. */
export function NumberStepper({
  value,
  onChange,
  min,
  max,
  step = 1,
  label,
  disabled = false,
  id,
  invalid,
  class: className,
}: NumberStepperProps) {
  const field = useFieldControl({ id, invalid });
  const [draft, setDraft] = useState(String(value));
  useEffect(() => setDraft(String(value)), [value]);

  const commit = (next: number): void => {
    const n = clamp(Math.round(next), min, max);
    setDraft(String(n));
    if (n !== value) onChange(n);
  };
  const commitDraft = (): void => {
    const trimmed = draft.trim();
    if (!/^\d+$/.test(trimmed)) {
      setDraft(String(value));
      return;
    }
    commit(Number(trimmed));
  };

  return (
    <div class={cx('stepper', field.invalid && 'stepper--invalid', disabled && 'stepper--disabled', className)}>
      <IconButton
        icon="minus"
        label="Decrease"
        size="sm"
        variant="ghost"
        class="stepper__button"
        tabIndex={-1}
        disabled={disabled || value <= min}
        onClick={() => commit(value - step)}
      />
      <input
        id={field.id}
        class="stepper__control"
        type="text"
        inputMode="numeric"
        autocomplete="off"
        role="spinbutton"
        aria-label={field.labelId ? undefined : label}
        aria-valuemin={min}
        aria-valuemax={max}
        aria-valuenow={value}
        aria-invalid={field.invalid ? 'true' : undefined}
        aria-describedby={field.describedBy}
        disabled={disabled}
        value={draft}
        onInput={(event) => {
          const digits = event.currentTarget.value.replace(/[^\d]/g, '');
          setDraft(digits);
          // A complete value in range applies while typing; anything else is clamped on blur or Enter.
          const n = digits ? Number(digits) : NaN;
          if (Number.isInteger(n) && n >= min && n <= max && n !== value) onChange(n);
        }}
        onBlur={commitDraft}
        onKeyDown={(event) => {
          switch (event.key) {
            case 'ArrowUp':
              event.preventDefault();
              commit(value + step);
              break;
            case 'ArrowDown':
              event.preventDefault();
              commit(value - step);
              break;
            case 'PageUp':
              event.preventDefault();
              commit(value + step * 10);
              break;
            case 'PageDown':
              event.preventDefault();
              commit(value - step * 10);
              break;
            case 'Home':
              event.preventDefault();
              commit(min);
              break;
            case 'End':
              event.preventDefault();
              commit(max);
              break;
            case 'Enter':
              event.preventDefault();
              commitDraft();
              break;
          }
        }}
      />
      <IconButton
        icon="plus"
        label="Increase"
        size="sm"
        variant="ghost"
        class="stepper__button"
        tabIndex={-1}
        disabled={disabled || value >= max}
        onClick={() => commit(value + step)}
      />
    </div>
  );
}
