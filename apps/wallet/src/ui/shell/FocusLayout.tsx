import type { ComponentChildren } from 'preact';
import { BrandLockup } from '../components/Brand';
import { Icon } from '../components/Icon';
import { cx } from '../components/internal/cx';

export interface FocusLayoutProps {
  /** Shows a back button in the header. */
  onBack?: () => void;
  backLabel?: string;
  /** Content at the end of the header (e.g. a "Cancel" button). */
  headerAction?: ComponentChildren;
  /** Content width: narrow (440 px, default) for forms, wide (640 px) for the recovery phrase. */
  width?: 'narrow' | 'wide';
  /** Quiet line at the bottom of the page. */
  footer?: ComponentChildren;
  class?: string;
  children: ComponentChildren;
}

/** Full-page layout without navigation, for onboarding, unlock and wallet creation. */
export function FocusLayout({ onBack, backLabel = 'Back', headerAction, width = 'narrow', footer, class: className, children }: FocusLayoutProps) {
  return (
    <div class={cx('focus-layout', `focus-layout--${width}`, className)}>
      <header class="focus-layout__header">
        <div class="focus-layout__start">
          {onBack ? (
            <button type="button" class="page-header__back focus-layout__back" onClick={onBack}>
              <Icon name="arrow-left" size="sm" />
              <span>{backLabel}</span>
            </button>
          ) : (
            <BrandLockup size="sm" />
          )}
        </div>
        {headerAction != null ? <div class="focus-layout__end">{headerAction}</div> : null}
      </header>
      <main class="focus-layout__main" tabIndex={-1}>
        <div class="focus-layout__content">{children}</div>
      </main>
      {footer != null ? <footer class="focus-layout__footer">{footer}</footer> : null}
    </div>
  );
}
