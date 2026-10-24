import { cx } from './internal/cx';

export interface SpinnerProps {
  size?: 'sm' | 'md' | 'lg';
  /** When set, the spinner is announced (role="status"); otherwise it is decorative. */
  label?: string;
  class?: string;
}

export function Spinner({ size = 'md', label, class: className }: SpinnerProps) {
  return (
    <span
      class={cx('spinner', `spinner--${size}`, className)}
      role={label ? 'status' : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : 'true'}
    >
      <svg viewBox="0 0 24 24" focusable="false" aria-hidden="true">
        <circle class="spinner__track" cx="12" cy="12" r="9" fill="none" />
        <circle class="spinner__arc" cx="12" cy="12" r="9" fill="none" />
      </svg>
    </span>
  );
}
