import type { ComponentChildren } from 'preact';
import { Icon, type IconName } from './Icon';
import { cx } from './internal/cx';

export interface EmptyStateProps {
  icon: IconName;
  title: ComponentChildren;
  /** One short line. */
  message?: ComponentChildren;
  /** Usually a single Button. */
  action?: ComponentChildren;
  size?: 'sm' | 'md';
  /** Colour of the icon tile; use positive for a completed action. */
  tone?: 'accent' | 'positive' | 'warning' | 'negative' | 'neutral';
  class?: string;
}

export function EmptyState({ icon, title, message, action, size = 'md', tone = 'accent', class: className }: EmptyStateProps) {
  return (
    <div class={cx('empty-state', `empty-state--${size}`, `empty-state--${tone}`, className)}>
      <span class="empty-state__icon" aria-hidden="true">
        <Icon name={icon} size="lg" />
      </span>
      <p class="empty-state__title">{title}</p>
      {message != null ? <p class="empty-state__message">{message}</p> : null}
      {action != null ? <div class="empty-state__action">{action}</div> : null}
    </div>
  );
}
