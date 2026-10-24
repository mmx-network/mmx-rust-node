import type { ComponentChildren } from 'preact';
import { Icon, type IconName } from './Icon';
import { IconButton } from './IconButton';
import { cx } from './internal/cx';

export type BannerTone = 'info' | 'positive' | 'warning' | 'danger';

export interface BannerProps {
  tone?: BannerTone;
  title?: ComponentChildren;
  /** Overrides the tone's default icon. */
  icon?: IconName;
  /** Usually a small Button. */
  action?: ComponentChildren;
  onDismiss?: () => void;
  class?: string;
  children?: ComponentChildren;
}

const toneIcon: Record<BannerTone, IconName> = {
  info: 'info',
  positive: 'check-circle',
  warning: 'warning',
  danger: 'warning',
};

/** Inline status message. Danger banners are announced immediately (role="alert"). */
export function Banner({ tone = 'info', title, icon, action, onDismiss, class: className, children }: BannerProps) {
  return (
    <div class={cx('banner', `banner--${tone}`, className)} role={tone === 'danger' ? 'alert' : 'status'}>
      <Icon name={icon ?? toneIcon[tone]} class="banner__icon" />
      <div class="banner__content">
        {title != null ? <p class="banner__title">{title}</p> : null}
        {children != null ? <div class="banner__message">{children}</div> : null}
      </div>
      {action != null ? <div class="banner__action">{action}</div> : null}
      {onDismiss ? <IconButton icon="close" label="Dismiss" size="sm" class="banner__dismiss" onClick={onDismiss} /> : null}
    </div>
  );
}
