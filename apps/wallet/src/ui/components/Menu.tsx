import type { ComponentChildren, VNode } from 'preact';
import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState } from 'preact/hooks';
import { Icon, type IconName } from './Icon';
import { cx } from './internal/cx';
import { useFloatingPosition, useOutsidePress, type Placement } from './internal/floating';

export interface MenuItem {
  /** Stable key (defaults to the label). */
  id?: string;
  label: ComponentChildren;
  /** Short second line (e.g. a wallet id). */
  description?: ComponentChildren;
  icon?: IconName;
  /** Custom leading visual (overrides `icon`), e.g. an Identicon. */
  leading?: ComponentChildren;
  /** Content at the end of the row (e.g. a lock icon). */
  trailing?: ComponentChildren;
  /** Makes the item a radio item; the checked one shows a check mark. */
  checked?: boolean;
  tone?: 'default' | 'danger';
  disabled?: boolean;
  /** Text used for type-ahead when `label` is not a string. */
  textValue?: string;
  onSelect: () => void;
}

export type MenuEntry = MenuItem | { separator: true };

export interface MenuTriggerProps {
  id: string;
  'aria-haspopup': 'menu';
  'aria-expanded': 'true' | 'false';
  'aria-controls'?: string;
  onClick: (event: MouseEvent) => void;
  onKeyDown: (event: KeyboardEvent) => void;
}

export interface MenuProps {
  /** Accessible name of the menu. */
  label: string;
  items: MenuEntry[];
  /** Renders the trigger; spread the given props onto a <button>, Button or IconButton. */
  trigger: (props: MenuTriggerProps, open: boolean) => VNode;
  placement?: Placement;
  /** Make the menu at least as wide as the trigger. */
  matchWidth?: boolean;
  class?: string;
}

const isItem = (entry: MenuEntry): entry is MenuItem => !('separator' in entry);

/** Menu button pattern: arrow keys, Home/End, type-ahead, Esc and outside clicks close it. */
export function Menu({ label, items, trigger, placement = 'bottom-start', matchWidth = false, class: className }: MenuProps) {
  const [open, setOpen] = useState(false);
  const [focusOn, setFocusOn] = useState<'first' | 'last' | 'checked'>('checked');
  const rootRef = useRef<HTMLSpanElement>(null);
  const triggerRef = useRef<HTMLElement | null>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const typeahead = useRef({ text: '', at: 0 });
  const auto = useId();
  const triggerId = `m${auto}-t`;
  const menuId = `m${auto}-m`;

  const close = useCallback((refocus: boolean) => {
    setOpen(false);
    if (refocus) triggerRef.current?.focus({ preventScroll: true });
  }, []);
  const onOutside = useCallback(() => close(false), [close]);

  // The trigger is the first child of the root; resolved on every render so
  // any component (Button, IconButton, plain <button>) can be the trigger.
  useLayoutEffect(() => {
    triggerRef.current = (rootRef.current?.firstElementChild as HTMLElement | null) ?? rootRef.current;
  });
  useFloatingPosition(open, triggerRef, panelRef, placement, matchWidth);
  useOutsidePress(open, [rootRef], onOutside);

  const menuItems = (): HTMLElement[] =>
    Array.from(panelRef.current?.querySelectorAll<HTMLElement>('[data-menu-item]:not([aria-disabled="true"])') ?? []);

  useLayoutEffect(() => {
    if (!open) return;
    const list = menuItems();
    let target: HTMLElement | undefined;
    if (focusOn === 'last') target = list[list.length - 1];
    else if (focusOn === 'checked') target = list.find((el) => el.getAttribute('aria-checked') === 'true') ?? list[0];
    else target = list[0];
    (target ?? panelRef.current)?.focus({ preventScroll: true });
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onBlur = (): void => close(false);
    window.addEventListener('blur', onBlur);
    return () => window.removeEventListener('blur', onBlur);
  }, [open, close]);

  const openWith = (where: 'first' | 'last' | 'checked'): void => {
    setFocusOn(where);
    setOpen(true);
  };

  const triggerProps: MenuTriggerProps = {
    id: triggerId,
    'aria-haspopup': 'menu',
    'aria-expanded': open ? 'true' : 'false',
    'aria-controls': open ? menuId : undefined,
    onClick: () => (open ? close(false) : openWith('checked')),
    onKeyDown: (event) => {
      if (event.key === 'ArrowDown' || event.key === 'Enter' || event.key === ' ') {
        event.preventDefault();
        openWith(event.key === 'ArrowDown' ? 'first' : 'checked');
      } else if (event.key === 'ArrowUp') {
        event.preventDefault();
        openWith('last');
      }
    },
  };

  const onMenuKeyDown = (event: KeyboardEvent): void => {
    const list = menuItems();
    const index = list.indexOf(document.activeElement as HTMLElement);
    const focusAt = (i: number): void => list[(i + list.length) % list.length]?.focus();
    switch (event.key) {
      case 'ArrowDown':
        event.preventDefault();
        focusAt(index + 1);
        break;
      case 'ArrowUp':
        event.preventDefault();
        focusAt(index - 1);
        break;
      case 'Home':
        event.preventDefault();
        focusAt(0);
        break;
      case 'End':
        event.preventDefault();
        focusAt(list.length - 1);
        break;
      case 'Escape':
        event.preventDefault();
        event.stopPropagation();
        close(true);
        break;
      case 'Tab':
        event.preventDefault();
        close(true);
        break;
      default:
        if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
          const now = Date.now();
          const state = typeahead.current;
          state.text = now - state.at > 700 ? event.key.toLowerCase() : state.text + event.key.toLowerCase();
          state.at = now;
          const start = Math.max(index, 0);
          const ordered = [...list.slice(start + 1), ...list.slice(0, start + 1)];
          const match = ordered.find((el) => (el.dataset.text ?? el.textContent ?? '').trim().toLowerCase().startsWith(state.text));
          match?.focus();
        }
    }
  };

  const hasChecks = items.some((e) => isItem(e) && e.checked !== undefined);

  return (
    <span ref={rootRef} class={cx('menu-root', className)}>
      {trigger(triggerProps, open)}
      {open ? (
        <div
          ref={panelRef}
          id={menuId}
          class="menu"
          role="menu"
          aria-label={label}
          tabIndex={-1}
          onKeyDown={onMenuKeyDown}
        >
          {items.map((entry, i) => {
            if (!isItem(entry)) return <div key={`sep-${i}`} class="menu__separator" role="separator" />;
            const radio = entry.checked !== undefined;
            return (
              <button
                key={entry.id ?? (typeof entry.label === 'string' ? entry.label : i)}
                type="button"
                class={cx(
                  'menu__item',
                  entry.tone === 'danger' && 'menu__item--danger',
                  entry.checked && 'menu__item--checked',
                )}
                role={radio ? 'menuitemradio' : 'menuitem'}
                aria-checked={radio ? (entry.checked ? 'true' : 'false') : undefined}
                aria-disabled={entry.disabled ? 'true' : undefined}
                tabIndex={-1}
                data-menu-item=""
                data-text={entry.textValue ?? (typeof entry.label === 'string' ? entry.label : undefined)}
                onClick={() => {
                  if (entry.disabled) return;
                  close(true);
                  entry.onSelect();
                }}
              >
                {entry.leading != null ? (
                  <span class="menu__leading">{entry.leading}</span>
                ) : entry.icon ? (
                  <span class="menu__leading">
                    <Icon name={entry.icon} />
                  </span>
                ) : null}
                <span class="menu__text">
                  <span class="menu__label">{entry.label}</span>
                  {entry.description != null ? <span class="menu__description">{entry.description}</span> : null}
                </span>
                {entry.trailing != null ? <span class="menu__trailing">{entry.trailing}</span> : null}
                {hasChecks ? (
                  <span class="menu__check" aria-hidden="true">
                    {entry.checked ? <Icon name="check" size="sm" /> : null}
                  </span>
                ) : null}
              </button>
            );
          })}
        </div>
      ) : null}
    </span>
  );
}
