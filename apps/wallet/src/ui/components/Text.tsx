import { h, type ComponentChildren } from 'preact';
import { cx } from './internal/cx';

export interface TextProps {
  as?: 'p' | 'span' | 'div' | 'strong' | 'small' | 'time';
  size?: 'xs' | 'sm' | 'md' | 'lg';
  tone?: 'default' | 'muted' | 'faint' | 'accent' | 'positive' | 'negative' | 'warning';
  weight?: 'regular' | 'medium' | 'semibold';
  mono?: boolean;
  /** Tabular numerals. */
  numeric?: boolean;
  /** Single line with ellipsis. */
  truncate?: boolean;
  /** Break long unspaced strings (addresses, ids) anywhere. */
  breakAll?: boolean;
  align?: 'start' | 'center' | 'end';
  id?: string;
  title?: string;
  dateTime?: string;
  class?: string;
  children?: ComponentChildren;
}

export function Text({
  as = 'p',
  size = 'md',
  tone = 'default',
  weight,
  mono = false,
  numeric = false,
  truncate = false,
  breakAll = false,
  align,
  class: className,
  children,
  ...rest
}: TextProps) {
  return h(
    as,
    {
      ...rest,
      class: cx(
        'text',
        `text--${size}`,
        tone !== 'default' && `text--${tone}`,
        weight && `text--${weight}`,
        mono && 'text--mono',
        numeric && 'text--numeric',
        truncate && 'text--truncate',
        breakAll && 'text--break',
        align && `text--${align}`,
        className,
      ),
    },
    children,
  );
}

export interface HeadingProps {
  level: 1 | 2 | 3 | 4;
  /** Visual size, independent of the level. */
  size?: 'xl' | 'lg' | 'md' | 'sm';
  id?: string;
  class?: string;
  children?: ComponentChildren;
}

const defaultSize = { 1: 'xl', 2: 'lg', 3: 'md', 4: 'sm' } as const;

export function Heading({ level, size, id, class: className, children }: HeadingProps) {
  return h(`h${level}`, { id, class: cx('heading', `heading--${size ?? defaultSize[level]}`, className) }, children);
}

export function VisuallyHidden({ children }: { children?: ComponentChildren }) {
  return <span class="visually-hidden">{children}</span>;
}
