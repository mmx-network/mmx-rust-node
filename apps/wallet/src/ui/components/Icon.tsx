import { icons, type IconName } from '../icons';
import { cx } from './internal/cx';

export type { IconName };

export interface IconProps {
  name: IconName;
  /** sm = 16 px, md = 20 px (default), lg = 24 px, xl = 32 px. */
  size?: 'sm' | 'md' | 'lg' | 'xl';
  class?: string;
  /** Accessible name. Omit for decorative icons (the default). */
  label?: string;
}

export function Icon({ name, size = 'md', class: className, label }: IconProps) {
  return (
    <svg
      class={cx('icon', `icon--${size}`, className)}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-linecap="round"
      stroke-linejoin="round"
      focusable="false"
      aria-hidden={label ? undefined : 'true'}
      role={label ? 'img' : undefined}
      aria-label={label}
      data-icon={name}
    >
      <path d={icons[name]} />
    </svg>
  );
}
