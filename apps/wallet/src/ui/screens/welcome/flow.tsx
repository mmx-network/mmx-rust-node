/**
 * Pieces shared by the welcome, create, import and unlock screens.
 */
import type { ComponentChildren, RefObject } from 'preact';
import { useEffect, useRef } from 'preact/hooks';
import { cx } from '../../components/internal/cx';
import './onboarding.css';

export interface FlowHeaderProps {
  title: ComponentChildren;
  subtitle?: ComponentChildren;
  /** Moves keyboard and screen reader focus to the title when shown (a new step). */
  autoFocus?: boolean;
  /** Visual above the title (identicon, brand mark). */
  leading?: ComponentChildren;
  align?: 'start' | 'center';
  class?: string;
}

/** Title block of a flow step. Uses the page header classes so every style applies to it. */
export function FlowHeader({ title, subtitle, autoFocus = false, leading, align = 'start', class: className }: FlowHeaderProps) {
  const ref = useRef<HTMLHeadingElement>(null);
  useEffect(() => {
    if (!autoFocus) return;
    ref.current?.focus({ preventScroll: true });
    window.scrollTo(0, 0);
  }, []);
  return (
    <header class={cx('page-header', 'onboarding-header', `onboarding-header--${align}`, className)}>
      {leading != null ? <div class="onboarding-header__leading">{leading}</div> : null}
      <div class="page-header__row">
        <div class="page-header__titles">
          <h1 ref={ref} tabIndex={-1} class="page-header__title onboarding-header__title">
            {title}
          </h1>
          {subtitle != null ? <p class="page-header__subtitle onboarding-header__subtitle">{subtitle}</p> : null}
        </div>
      </div>
    </header>
  );
}

/** Focuses an input once after mount (dynamic `autofocus` is unreliable across browsers). */
export function useInitialFocus(ref: RefObject<HTMLElement>, enabled = true): void {
  useEffect(() => {
    if (!enabled) return;
    // The frame can arrive late (background tab, slow machine): never take focus
    // away from a field the user (or a password manager) is already filling.
    const id = requestAnimationFrame(() => {
      const active = document.activeElement;
      if (active && active !== document.body && active !== document.documentElement) return;
      ref.current?.focus({ preventScroll: true });
    });
    return () => cancelAnimationFrame(id);
  }, []);
}

/** Tracks whether the component is still mounted (async handlers must not update it after unmount). */
export function useMounted(): RefObject<boolean> {
  const mounted = useRef(true);
  useEffect(
    () => () => {
      mounted.current = false;
    },
    [],
  );
  return mounted;
}

/** Attributes that keep password manager extensions away from a text field (e.g. the wallet name). */
export const PASSWORD_MANAGER_IGNORE = {
  autocomplete: 'off',
  'data-1p-ignore': 'true',
  'data-lpignore': 'true',
  'data-bwignore': 'true',
  'data-form-type': 'other',
} as const;

/**
 * Attributes for passphrase fields (always a PasswordInput, so the value stays
 * masked). Browsers' own password managers ignore autocomplete="off" on
 * password fields; "one-time-code" marks the field as not a password, so a
 * passphrase is never offered for saving and never replaces the saved
 * recovery words. The data-* hints do the same for extensions.
 */
export const PASSPHRASE_FIELD = {
  autocomplete: 'one-time-code',
  'data-1p-ignore': 'true',
  'data-lpignore': 'true',
  'data-bwignore': 'true',
  'data-form-type': 'other',
} as const;
