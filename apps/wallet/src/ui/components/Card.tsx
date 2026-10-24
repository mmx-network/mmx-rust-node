import { h, type ComponentChildren } from 'preact';
import { cx } from './internal/cx';

export interface CardProps {
  as?: 'div' | 'section' | 'article' | 'aside';
  /** default: elevated surface; sunken: recessed well; outline: border only; hero: emphasised (balance). */
  variant?: 'default' | 'sunken' | 'outline' | 'hero';
  padding?: 'none' | 'sm' | 'md' | 'lg';
  class?: string;
  id?: string;
  'aria-label'?: string;
  'aria-labelledby'?: string;
  children?: ComponentChildren;
}

export function Card({ as = 'div', variant = 'default', padding = 'md', class: className, children, ...rest }: CardProps) {
  return h(as, { ...rest, class: cx('card', `card--${variant}`, `card--pad-${padding}`, className) }, children);
}
