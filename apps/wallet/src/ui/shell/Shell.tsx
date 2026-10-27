import type { ComponentChildren } from 'preact';
import { useEffect, useId, useRef, useState } from 'preact/hooks';
import type { WalletInfo } from '../../backend/types';
import { BrandLockup, BrandMark } from '../components/Brand';
import { Icon, type IconName } from '../components/Icon';
import { IconButton } from '../components/IconButton';
import { Menu } from '../components/Menu';
import { Tooltip } from '../components/Tooltip';
import { cx } from '../components/internal/cx';
import { NetworkStatus, type NetworkInfo } from './NetworkStatus';
import { WalletSwitcher } from './WalletSwitcher';

export type RouteId = 'overview' | 'send' | 'receive' | 'activity' | 'contacts' | 'settings';

export interface ShellProps {
  route: RouteId;
  onNavigate(r: RouteId): void;
  wallets: WalletInfo[];
  activeWalletId?: string;
  onSelectWallet(id: string): void;
  onAddWallet(): void;
  network: NetworkInfo;
  canLock: boolean;
  onLock(): void;
  onUnlock?(): void;
  children: ComponentChildren;
}

interface NavDef {
  id: RouteId;
  label: string;
  icon: IconName;
}

export const NAV_ITEMS: readonly NavDef[] = [
  { id: 'overview', label: 'Overview', icon: 'overview' },
  { id: 'send', label: 'Send', icon: 'send' },
  { id: 'receive', label: 'Receive', icon: 'receive' },
  { id: 'activity', label: 'Activity', icon: 'activity' },
  { id: 'contacts', label: 'Contacts', icon: 'contacts' },
  { id: 'settings', label: 'Settings', icon: 'settings' },
];

const TAB_IDS: readonly RouteId[] = ['overview', 'send', 'receive', 'activity'];
const MORE_IDS: readonly RouteId[] = ['contacts', 'settings'];

const APP_TITLE = 'MMX Wallet';

/** Browser tab and window title of a page, e.g. "Send · MMX Wallet". */
export function documentTitle(route: RouteId): string {
  const label = NAV_ITEMS.find((n) => n.id === route)?.label;
  return label ? `${label} · ${APP_TITLE}` : APP_TITLE;
}

/**
 * After a page change, moves focus to the new page's heading (or the main
 * area) when the focused control went away with the old page, so keyboard
 * and screen reader users land on the new page. Focus that is still on a
 * control (the navigation, or a field the new page focused itself) stays.
 * Returns the element that received focus.
 */
export function focusNewPage(
  main: HTMLElement | null,
  doc: Pick<Document, 'activeElement' | 'body'> = document,
): HTMLElement | null {
  const active = doc.activeElement;
  if (!main || (active && active !== doc.body && active.isConnected)) return null;
  const heading = main.querySelector<HTMLElement>('h1');
  if (heading) {
    if (!heading.hasAttribute('tabindex')) heading.setAttribute('tabindex', '-1');
    heading.focus({ preventScroll: true });
    if (doc.activeElement === heading) return heading;
  }
  main.focus({ preventScroll: true });
  return main;
}

interface NavMode {
  /** Nav labels are hidden (rail layout, collapsed sidebar, narrow top bar). */
  iconOnly: boolean;
  /** Where nav tooltips open. */
  tooltip: 'right' | 'bottom';
}

const COLLAPSE_QUERY: Record<string, string | null> = {
  sidebar: '(min-width: 720px) and (max-width: 1023.98px)',
  topbar: '(min-width: 720px) and (max-width: 1099.98px)',
  rail: null,
};

function useNavMode(): NavMode {
  const layout = typeof document !== 'undefined' ? document.documentElement.dataset.layout ?? 'sidebar' : 'sidebar';
  const query = COLLAPSE_QUERY[layout] ?? null;
  const [collapsed, setCollapsed] = useState(() => (query ? window.matchMedia(query).matches : false));
  useEffect(() => {
    if (!query) return;
    const mq = window.matchMedia(query);
    const update = (): void => setCollapsed(mq.matches);
    update();
    mq.addEventListener('change', update);
    return () => mq.removeEventListener('change', update);
  }, [query]);
  return { iconOnly: layout === 'rail' || collapsed, tooltip: layout === 'topbar' ? 'bottom' : 'right' };
}

export function Shell({
  route,
  onNavigate,
  wallets,
  activeWalletId,
  onSelectWallet,
  onAddWallet,
  network,
  canLock,
  onLock,
  onUnlock,
  children,
}: ShellProps) {
  const mainRef = useRef<HTMLElement>(null);
  const auto = useId();
  const mainId = `main${auto}`;
  const { iconOnly, tooltip } = useNavMode();
  // An icon-only side rail is too narrow for the full switcher: show the avatar below Lock.
  const railSwitcher = iconOnly && tooltip === 'right';
  const firstRender = useRef(true);

  // Runs after the new page's own effects, so a page that focuses a field keeps that focus.
  useEffect(() => {
    document.title = documentTitle(route);
    if (firstRender.current) {
      firstRender.current = false;
      return;
    }
    window.scrollTo({ top: 0 });
    focusNewPage(mainRef.current);
  }, [route]);
  useEffect(
    () => () => {
      document.title = APP_TITLE;
    },
    [],
  );

  const switcherProps = { wallets, activeWalletId, onSelectWallet, onAddWallet };
  const moreActive = MORE_IDS.includes(route);

  return (
    <div class="shell" data-route={route}>
      <button type="button" class="skip-link" onClick={() => mainRef.current?.focus()}>
        Skip to content
      </button>

      <header class="shell__mobilebar">
        <BrandMark size={28} class="shell__mobilebrand" label="MMX Wallet" />
        <WalletSwitcher {...switcherProps} class="shell__mobileswitcher" />
        <NetworkStatus {...network} display="auto" class="shell__mobilestatus" />
        {canLock || onUnlock ? <IconButton icon={canLock ? 'unlock' : 'lock'} label={canLock ? 'Lock' : 'Unlock'} class="shell__mobilelock" onClick={canLock ? onLock : onUnlock} /> : null}
      </header>

      <header class="shell__nav">
        <div class="shell__brand">
          <BrandLockup size="sm" class="shell__lockup" />
        </div>
        {railSwitcher ? null : (
          <div class="shell__switcher">
            <WalletSwitcher {...switcherProps} placement={tooltip === 'bottom' ? 'bottom-end' : 'bottom-start'} />
          </div>
        )}
        <nav class="shell__navlist" aria-label="Main">
          <ul class="shell__navitems">
            {NAV_ITEMS.map((item) => {
              const active = item.id === route;
              const button = (
                <button
                  type="button"
                  class={cx('nav-item', active && 'nav-item--active')}
                  aria-current={active ? 'page' : undefined}
                  onClick={() => onNavigate(item.id)}
                >
                  <Icon name={item.icon} class="nav-item__icon" />
                  <span class="nav-item__label">{item.label}</span>
                </button>
              );
              return (
                <li key={item.id}>
                  {iconOnly ? (
                    <Tooltip content={item.label} placement={tooltip} describe={false}>
                      {button}
                    </Tooltip>
                  ) : (
                    button
                  )}
                </li>
              );
            })}
          </ul>
        </nav>
        <div class="shell__status">
          <NetworkStatus {...network} display={railSwitcher ? 'dot' : 'full'} />
        </div>
        {canLock || onUnlock ? (
          <div class="shell__lock">
            {iconOnly ? (
              <Tooltip content={canLock ? 'Lock' : 'Unlock'} placement={tooltip} describe={false}>
                <IconButton icon={canLock ? 'unlock' : 'lock'} label={canLock ? 'Lock' : 'Unlock'} noTitle onClick={canLock ? onLock : onUnlock} />
              </Tooltip>
            ) : (
              <button type="button" class="nav-item nav-item--quiet" onClick={canLock ? onLock : onUnlock}>
                <Icon name={canLock ? 'unlock' : 'lock'} class="nav-item__icon" />
                <span class="nav-item__label">{canLock ? 'Lock' : 'Unlock'}</span>
              </button>
            )}
          </div>
        ) : null}
        {railSwitcher ? (
          <div class="shell__switcher shell__switcher--bottom">
            <WalletSwitcher {...switcherProps} compact placement="right-end" />
          </div>
        ) : null}
      </header>

      <main id={mainId} ref={mainRef} class="shell__main" tabIndex={-1}>
        <div class="shell__content">{children}</div>
      </main>

      <nav class="tabbar" aria-label="Main">
        <ul class="tabbar__items">
          {NAV_ITEMS.filter((n) => TAB_IDS.includes(n.id)).map((item) => {
            const active = item.id === route;
            return (
              <li key={item.id}>
                <button
                  type="button"
                  class={cx('tabbar__item', active && 'tabbar__item--active')}
                  aria-current={active ? 'page' : undefined}
                  onClick={() => onNavigate(item.id)}
                >
                  <Icon name={item.icon} class="tabbar__icon" />
                  <span class="tabbar__label">{item.label}</span>
                </button>
              </li>
            );
          })}
          <li>
            <Menu
              label="More"
              placement="top-end"
              items={NAV_ITEMS.filter((n) => MORE_IDS.includes(n.id)).map((item) => ({
                id: item.id,
                label: item.label,
                icon: item.icon,
                checked: item.id === route,
                onSelect: () => onNavigate(item.id),
              }))}
              trigger={(props) => (
                <button
                  {...props}
                  type="button"
                  class={cx('tabbar__item', moreActive && 'tabbar__item--active')}
                  aria-current={moreActive ? 'page' : undefined}
                >
                  <Icon name="more" class="tabbar__icon" />
                  <span class="tabbar__label">More</span>
                </button>
              )}
            />
          </li>
        </ul>
      </nav>
    </div>
  );
}
