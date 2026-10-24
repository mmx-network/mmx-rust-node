import type { ComponentChildren } from 'preact';
import { useId } from 'preact/hooks';
import { Icon } from './Icon';
import { cx } from './internal/cx';

export interface ListProps {
  /** Accessible name for the list. */
  label?: string;
  /** Render inside a bordered surface with dividers. */
  inset?: boolean;
  /** Divider lines between items (default true). */
  divided?: boolean;
  class?: string;
  children?: ComponentChildren;
}

export function List({ label, inset = false, divided = true, class: className, children }: ListProps) {
  return (
    <ul class={cx('list', inset && 'list--inset', divided && 'list--divided', className)} aria-label={label}>
      {children}
    </ul>
  );
}

export interface ListGroupProps {
  /** Group heading, e.g. a date. */
  title: ComponentChildren;
  /** Text at the end of the heading row. */
  aside?: ComponentChildren;
  inset?: boolean;
  class?: string;
  children?: ComponentChildren;
}

/** A titled list, e.g. activity grouped by day. */
export function ListGroup({ title, aside, inset = false, class: className, children }: ListGroupProps) {
  const auto = useId();
  const id = `lg${auto}`;
  return (
    <section class={cx('list-group', className)} aria-labelledby={id}>
      <div class="list-group__header">
        <h3 class="list-group__title" id={id}>
          {title}
        </h3>
        {aside != null ? <span class="list-group__aside">{aside}</span> : null}
      </div>
      <ul class={cx('list', 'list--divided', inset && 'list--inset')}>{children}</ul>
    </section>
  );
}

export interface ListItemProps {
  leading?: ComponentChildren;
  title: ComponentChildren;
  subtitle?: ComponentChildren;
  /** Right-aligned primary detail (e.g. an Amount). */
  trailing?: ComponentChildren;
  /** Right-aligned secondary line under `trailing`. */
  trailingSub?: ComponentChildren;
  /** Makes the row a button. */
  onClick?: () => void;
  /** Buttons rendered after the row content (outside the row button). */
  actions?: ComponentChildren;
  /** Shows a chevron (defaults to true when onClick is set and there is no trailing content). */
  chevron?: boolean;
  selected?: boolean;
  disabled?: boolean;
  /** Accessible name for the row button when the visible content is not enough. */
  'aria-label'?: string;
  class?: string;
}

export function ListItem({
  leading,
  title,
  subtitle,
  trailing,
  trailingSub,
  onClick,
  actions,
  chevron,
  selected = false,
  disabled = false,
  class: className,
  ...aria
}: ListItemProps) {
  const showChevron = chevron ?? (onClick != null && trailing == null && trailingSub == null && actions == null);
  const content = (
    <>
      {leading != null ? <span class="list-item__leading">{leading}</span> : null}
      <span class="list-item__main">
        <span class="list-item__title">{title}</span>
        {subtitle != null ? <span class="list-item__subtitle">{subtitle}</span> : null}
      </span>
      {trailing != null || trailingSub != null ? (
        <span class="list-item__trailing">
          {trailing != null ? <span class="list-item__trailing-main">{trailing}</span> : null}
          {trailingSub != null ? <span class="list-item__trailing-sub">{trailingSub}</span> : null}
        </span>
      ) : null}
      {showChevron ? <Icon name="chevron-right" class="list-item__chevron" /> : null}
    </>
  );
  return (
    <li
      class={cx(
        'list-item',
        onClick && 'list-item--interactive',
        selected && 'list-item--selected',
        disabled && 'list-item--disabled',
        className,
      )}
    >
      {onClick ? (
        <button
          type="button"
          class="list-item__body"
          onClick={onClick}
          disabled={disabled}
          aria-current={selected ? 'true' : undefined}
          aria-label={aria['aria-label']}
        >
          {content}
        </button>
      ) : (
        <div class="list-item__body">{content}</div>
      )}
      {actions != null ? <span class="list-item__actions">{actions}</span> : null}
    </li>
  );
}
