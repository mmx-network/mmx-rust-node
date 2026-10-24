import type { ButtonHTMLAttributes, ComponentChildren } from 'preact';
import { Icon, type IconName } from './Icon';
import { Spinner } from './Spinner';
import { cx } from './internal/cx';
import { blockClick } from './internal/events';

export type ButtonVariant = 'primary' | 'secondary' | 'ghost' | 'danger';
export type ButtonSize = 'sm' | 'md' | 'lg';

export interface ButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, 'size' | 'icon' | 'loading'> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  /** Leading icon. */
  icon?: IconName;
  /** Trailing icon. */
  iconEnd?: IconName;
  /** Shows a spinner, blocks clicks and sets aria-busy. */
  loading?: boolean;
  /** Stretches to the container width. */
  block?: boolean;
  type?: 'button' | 'submit' | 'reset';
  class?: string;
  children?: ComponentChildren;
}

export function Button({
  variant = 'secondary',
  size = 'md',
  icon,
  iconEnd,
  loading = false,
  block = false,
  type = 'button',
  disabled,
  class: className,
  children,
  onClick,
  ...rest
}: ButtonProps) {
  return (
    <button
      {...rest}
      type={type}
      class={cx('btn', `btn--${variant}`, `btn--${size}`, block && 'btn--block', loading && 'btn--loading', className)}
      disabled={disabled}
      aria-disabled={loading && !disabled ? 'true' : undefined}
      aria-busy={loading ? 'true' : undefined}
      onClick={disabled ? undefined : loading ? blockClick : onClick}
    >
      {loading ? <Spinner size="sm" class="btn__spinner" /> : null}
      <span class="btn__content">
        {icon ? <Icon name={icon} class="btn__icon" /> : null}
        {children != null && children !== false ? <span class="btn__label">{children}</span> : null}
        {iconEnd ? <Icon name={iconEnd} class="btn__icon btn__icon--end" /> : null}
      </span>
    </button>
  );
}
