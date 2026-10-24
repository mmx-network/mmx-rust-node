import type { ButtonHTMLAttributes } from 'preact';
import { Icon, type IconName } from './Icon';
import { Spinner } from './Spinner';
import { cx } from './internal/cx';
import { blockClick } from './internal/events';

export interface IconButtonProps
  extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, 'size' | 'icon' | 'label' | 'children'> {
  icon: IconName;
  /** Required accessible name (aria-label) and native tooltip. */
  label: string;
  variant?: 'ghost' | 'secondary' | 'primary' | 'danger';
  size?: 'sm' | 'md' | 'lg';
  loading?: boolean;
  /** Suppresses the native title tooltip (e.g. when wrapped in <Tooltip>). */
  noTitle?: boolean;
  type?: 'button' | 'submit' | 'reset';
  class?: string;
}

export function IconButton({
  icon,
  label,
  variant = 'ghost',
  size = 'md',
  loading = false,
  noTitle = false,
  type = 'button',
  disabled,
  class: className,
  onClick,
  ...rest
}: IconButtonProps) {
  return (
    <button
      {...rest}
      type={type}
      class={cx('icon-btn', `icon-btn--${variant}`, `icon-btn--${size}`, className)}
      aria-label={label}
      title={noTitle ? undefined : label}
      disabled={disabled}
      aria-disabled={loading && !disabled ? 'true' : undefined}
      aria-busy={loading ? 'true' : undefined}
      onClick={disabled ? undefined : loading ? blockClick : onClick}
    >
      {loading ? <Spinner size="sm" /> : <Icon name={icon} size={size === 'lg' ? 'lg' : 'md'} />}
    </button>
  );
}
