import type { ComponentChildren } from 'preact';
import { Icon, type IconName } from './Icon';
import { cx } from './internal/cx';

export type Tone = 'neutral' | 'positive' | 'negative' | 'warning' | 'accent';

export interface BadgeProps {
  tone?: Tone;
  icon?: IconName;
  /** Small dot instead of an icon. */
  dot?: boolean;
  class?: string;
  title?: string;
  children?: ComponentChildren;
}

export function Badge({ tone = 'neutral', icon, dot = false, class: className, title, children }: BadgeProps) {
  return (
    <span class={cx('badge', `badge--${tone}`, className)} title={title}>
      {dot ? <span class="badge__dot" aria-hidden="true" /> : null}
      {icon ? <Icon name={icon} size="sm" class="badge__icon" /> : null}
      {children != null ? <span class="badge__label">{children}</span> : null}
    </span>
  );
}
