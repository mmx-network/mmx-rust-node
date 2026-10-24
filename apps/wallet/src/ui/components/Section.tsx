import { h, type ComponentChildren } from 'preact';
import { useId } from 'preact/hooks';
import { cx } from './internal/cx';

export interface SectionProps {
  title: ComponentChildren;
  /** One short line under the title. */
  description?: ComponentChildren;
  /** Controls at the end of the header row (buttons, filters). */
  actions?: ComponentChildren;
  /** Heading level (default 2). */
  level?: 2 | 3;
  /** Wrap the body in a card surface. */
  card?: boolean;
  class?: string;
  id?: string;
  children?: ComponentChildren;
}

export function Section({ title, description, actions, level = 2, card = false, class: className, id, children }: SectionProps) {
  const auto = useId();
  const headingId = `s${auto}`;
  return (
    <section id={id} class={cx('section', card && 'section--card', className)} aria-labelledby={headingId}>
      <div class="section__header">
        <div class="section__titles">
          {h(level === 2 ? 'h2' : 'h3', { class: 'section__title', id: headingId }, title)}
          {description != null ? <p class="section__description">{description}</p> : null}
        </div>
        {actions != null ? <div class="section__actions">{actions}</div> : null}
      </div>
      {children != null ? <div class="section__body">{children}</div> : null}
    </section>
  );
}
