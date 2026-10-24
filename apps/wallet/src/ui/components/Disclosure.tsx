import type { ComponentChildren } from 'preact';
import { useId, useState } from 'preact/hooks';
import { Icon, type IconName } from './Icon';
import { cx } from './internal/cx';

export interface DisclosureProps {
  title: ComponentChildren;
  /** Quiet text after the title, e.g. a count. */
  meta?: ComponentChildren;
  icon?: IconName;
  /** Uncontrolled initial state (default closed). */
  defaultOpen?: boolean;
  /** Controlled state. */
  open?: boolean;
  onToggle?: (open: boolean) => void;
  class?: string;
  children?: ComponentChildren;
}

/** A collapsible group, e.g. "Other tokens". */
export function Disclosure({ title, meta, icon, defaultOpen = false, open, onToggle, class: className, children }: DisclosureProps) {
  const [inner, setInner] = useState(defaultOpen);
  const isOpen = open ?? inner;
  const auto = useId();
  const panelId = `dc${auto}`;
  return (
    <div class={cx('disclosure', isOpen && 'disclosure--open', className)}>
      <button
        type="button"
        class="disclosure__toggle"
        aria-expanded={isOpen ? 'true' : 'false'}
        aria-controls={panelId}
        onClick={() => {
          const next = !isOpen;
          if (open === undefined) setInner(next);
          onToggle?.(next);
        }}
      >
        {icon ? <Icon name={icon} size="sm" class="disclosure__icon" /> : null}
        <span class="disclosure__title">{title}</span>
        {meta != null ? <span class="disclosure__meta">{meta}</span> : null}
        <Icon name="chevron-down" size="sm" class="disclosure__chevron" />
      </button>
      <div id={panelId} class="disclosure__panel" hidden={!isOpen}>
        {isOpen ? children : null}
      </div>
    </div>
  );
}
