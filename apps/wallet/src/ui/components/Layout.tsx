import { h, type ComponentChildren } from 'preact';
import { Icon } from './Icon';
import { cx } from './internal/cx';

export type Gap = 0 | 1 | 2 | 3 | 4 | 5 | 6 | 8 | 10 | 12;

export interface PageProps {
  /** narrow: forms and flows (680 px, a full address fits on one line); default: 960 px; wide: full content width. */
  width?: 'narrow' | 'default' | 'wide';
  class?: string;
  children?: ComponentChildren;
}

/** Screen container: max width, vertical rhythm between direct children. */
export function Page({ width = 'default', class: className, children }: PageProps) {
  return <div class={cx('page', `page--${width}`, className)}>{children}</div>;
}

export interface PageHeaderProps {
  title: ComponentChildren;
  subtitle?: ComponentChildren;
  actions?: ComponentChildren;
  /** Shows a back button. */
  onBack?: () => void;
  backLabel?: string;
  class?: string;
}

export function PageHeader({ title, subtitle, actions, onBack, backLabel = 'Back', class: className }: PageHeaderProps) {
  return (
    <header class={cx('page-header', className)}>
      {onBack ? (
        <button type="button" class="page-header__back" onClick={onBack}>
          <Icon name="arrow-left" size="sm" />
          <span>{backLabel}</span>
        </button>
      ) : null}
      <div class="page-header__row">
        <div class="page-header__titles">
          <h1 class="page-header__title">{title}</h1>
          {subtitle != null ? <p class="page-header__subtitle">{subtitle}</p> : null}
        </div>
        {actions != null ? <div class="page-header__actions">{actions}</div> : null}
      </div>
    </header>
  );
}

export interface StackProps {
  /** Spacing in steps of --space-unit (default 4). */
  gap?: Gap;
  direction?: 'column' | 'row';
  align?: 'start' | 'center' | 'end' | 'stretch' | 'baseline';
  justify?: 'start' | 'center' | 'end' | 'between';
  wrap?: boolean;
  /** Row direction collapses to a column below 720 px. */
  responsive?: boolean;
  as?: 'div' | 'section' | 'ul' | 'ol' | 'form' | 'footer' | 'header';
  class?: string;
  children?: ComponentChildren;
  [attr: `aria-${string}` | `data-${string}`]: string | undefined;
}

export function Stack({
  gap = 4,
  direction = 'column',
  align,
  justify,
  wrap = false,
  responsive = false,
  as = 'div',
  class: className,
  children,
  ...attrs
}: StackProps) {
  return h(
    as,
    {
      ...attrs,
      class: cx(
        'stack',
        direction === 'row' && 'stack--row',
        `stack--gap-${gap}`,
        align && `stack--align-${align}`,
        justify && `stack--justify-${justify}`,
        wrap && 'stack--wrap',
        responsive && 'stack--responsive',
        className,
      ),
    },
    children,
  );
}

export interface GridProps {
  /** Fixed column count above 720 px (collapses to one column below). */
  columns?: 1 | 2 | 3 | 4;
  /** Minimum column width for an auto-fitting grid, e.g. "14rem". Overrides `columns`. */
  min?: string;
  gap?: Gap;
  class?: string;
  children?: ComponentChildren;
}

export function Grid({ columns = 2, min, gap = 4, class: className, children }: GridProps) {
  return (
    <div
      class={cx('grid', min ? 'grid--auto' : `grid--cols-${columns}`, `grid--gap-${gap}`, className)}
      style={min ? { '--grid-min': min } : undefined}
    >
      {children}
    </div>
  );
}
