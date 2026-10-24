import type { SelectHTMLAttributes } from 'preact';
import { useFieldControl } from './Field';
import { Icon } from './Icon';
import { cx } from './internal/cx';

export interface SelectOption {
  value: string;
  label: string;
  disabled?: boolean;
}

export interface SelectProps
  extends Omit<SelectHTMLAttributes<HTMLSelectElement>, 'value' | 'onChange' | 'size' | 'class' | 'className' | 'id' | 'aria-describedby'> {
  id?: string;
  'aria-describedby'?: string;
  value: string;
  onValue: (value: string) => void;
  options: SelectOption[];
  invalid?: boolean;
  size?: 'sm' | 'md';
  class?: string;
}

export function Select({
  value,
  onValue,
  options,
  invalid,
  size = 'md',
  class: className,
  id,
  'aria-describedby': ariaDescribedBy,
  ...rest
}: SelectProps) {
  const field = useFieldControl({ id, 'aria-describedby': ariaDescribedBy, invalid });
  return (
    <div class={cx('select', `select--${size}`, field.invalid && 'select--invalid', rest.disabled && 'select--disabled', className)}>
      <select
        {...rest}
        id={field.id}
        class="select__control"
        value={value}
        aria-invalid={field.invalid ? 'true' : undefined}
        aria-describedby={field.describedBy}
        onChange={(event) => onValue(event.currentTarget.value)}
      >
        {options.map((o) => (
          <option key={o.value} value={o.value} disabled={o.disabled}>
            {o.label}
          </option>
        ))}
      </select>
      <Icon name="chevron-down" class="select__chevron" />
    </div>
  );
}
