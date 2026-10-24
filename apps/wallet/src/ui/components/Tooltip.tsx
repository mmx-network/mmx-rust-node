import { cloneElement, type VNode } from 'preact';
import { useEffect, useId, useRef, useState } from 'preact/hooks';
import { cx } from './internal/cx';
import { useFloatingPosition, type Placement } from './internal/floating';

export interface TooltipProps {
  /** Short text. */
  content: string;
  /** A single focusable element (Button, IconButton, …). */
  children: VNode;
  placement?: Placement;
  /**
   * true (default): the tooltip describes the element (aria-describedby).
   * false: purely visual, for elements whose accessible name already says the same.
   */
  describe?: boolean;
  disabled?: boolean;
  /** Hover delay in ms (default 350). Keyboard focus shows it immediately. */
  delay?: number;
  class?: string;
}

function focusVisible(el: HTMLElement): boolean {
  try {
    return el.matches(':focus-visible');
  } catch {
    return true;
  }
}

export function Tooltip({ content, children, placement = 'top', describe = true, disabled = false, delay = 350, class: className }: TooltipProps) {
  const [open, setOpen] = useState(false);
  const anchorRef = useRef<HTMLSpanElement>(null);
  const bubbleRef = useRef<HTMLSpanElement>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const auto = useId();
  const id = `tt${auto}`;
  useFloatingPosition(open && !disabled, anchorRef, bubbleRef, placement);

  const clear = (): void => {
    if (timer.current) clearTimeout(timer.current);
    timer.current = null;
  };
  useEffect(() => clear, []);
  useEffect(() => {
    if (!open) return;
    const onKey = (event: KeyboardEvent): void => {
      if (event.key === 'Escape') setOpen(false);
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [open]);

  const show = (immediate: boolean): void => {
    if (disabled) return;
    clear();
    if (immediate) setOpen(true);
    else timer.current = setTimeout(() => setOpen(true), delay);
  };
  const hide = (): void => {
    clear();
    setOpen(false);
  };

  const existing = (children.props as { 'aria-describedby'?: string })['aria-describedby'];
  const child = describe ? cloneElement(children, { 'aria-describedby': existing ? `${existing} ${id}` : id }) : children;
  return (
    <span
      ref={anchorRef}
      class={cx('tooltip-anchor', className)}
      onPointerEnter={(event) => {
        if (event.pointerType === 'mouse') show(false);
      }}
      onPointerLeave={hide}
      onPointerDown={hide}
      onFocusIn={(event) => {
        if (focusVisible(event.target as HTMLElement)) show(true);
      }}
      onFocusOut={hide}
    >
      {child}
      {describe ? (
        <span id={id} class="visually-hidden">
          {content}
        </span>
      ) : null}
      {open && !disabled ? (
        <span ref={bubbleRef} class="tooltip" role="presentation" aria-hidden="true">
          {content}
        </span>
      ) : null}
    </span>
  );
}
