import type { ComponentChildren } from 'preact';
import { useFieldControl } from './Field';
import { cx } from './internal/cx';

export interface SwitchProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  /** Visible label next to the switch. Inside a Field the Field label is used instead. */
  label?: ComponentChildren;
  /** Accessible name when there is no visible label. */
  'aria-label'?: string;
  disabled?: boolean;
  id?: string;
  class?: string;
}

export function Switch({ checked, onChange, label, disabled = false, id, class: className, ...aria }: SwitchProps) {
  const field = useFieldControl({ id });
  return (
    <button
      type="button"
      role="switch"
      id={field.id}
      class={cx('switch', checked && 'switch--on', className)}
      aria-checked={checked ? 'true' : 'false'}
      aria-label={aria['aria-label']}
      aria-describedby={field.describedBy}
      disabled={disabled}
      onClick={() => onChange(!checked)}
    >
      <span class="switch__track" aria-hidden="true">
        <span class="switch__thumb" />
      </span>
      {label != null ? <span class="switch__label">{label}</span> : null}
    </button>
  );
}
