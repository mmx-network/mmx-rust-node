import { Icon, type IconName } from './Icon';
import type { Tone } from './Badge';
import { cx } from './internal/cx';

export interface IconTileProps {
  icon: IconName;
  tone?: Tone;
  size?: 'sm' | 'md' | 'lg';
  /** Accessible name; decorative when omitted. */
  label?: string;
  class?: string;
}

/** An icon on a tinted round tile, e.g. the direction marker of an activity row. */
export function IconTile({ icon, tone = 'neutral', size = 'md', label, class: className }: IconTileProps) {
  return (
    <span
      class={cx('icon-tile', `icon-tile--${tone}`, `icon-tile--${size}`, className)}
      role={label ? 'img' : undefined}
      aria-label={label}
    >
      <Icon name={icon} size={size === 'lg' ? 'lg' : size === 'sm' ? 'sm' : 'md'} />
    </span>
  );
}
