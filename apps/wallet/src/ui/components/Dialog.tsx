import type { ComponentChildren, RefObject } from 'preact';
import { useEffect, useId, useLayoutEffect, useRef } from 'preact/hooks';
import { IconButton } from './IconButton';
import { cx } from './internal/cx';
import { restoreFocus, trapTab } from './internal/focus';

export interface DialogProps {
  open: boolean;
  /** Called on Esc, the close button or a backdrop click (only when dismissible). */
  onClose: () => void;
  title: ComponentChildren;
  /** One short line under the title. */
  description?: ComponentChildren;
  /** Action buttons; secondary first, primary last. */
  footer?: ComponentChildren;
  size?: 'sm' | 'md' | 'lg';
  /** false: no close button, Esc and backdrop clicks are ignored (e.g. while sending). */
  dismissible?: boolean;
  /** Element to focus when opened. Otherwise the first element marked data-autofocus, else the panel. */
  initialFocus?: RefObject<HTMLElement>;
  /** Visually hide the title (it still names the dialog). */
  hideTitle?: boolean;
  class?: string;
  children?: ComponentChildren;
}

interface PanelProps extends Omit<DialogProps, 'open'> {
  variant: 'dialog' | 'sheet';
}

let openCount = 0;

function lockScroll(): () => void {
  openCount++;
  document.documentElement.classList.add('has-modal');
  return () => {
    openCount = Math.max(0, openCount - 1);
    if (openCount === 0) document.documentElement.classList.remove('has-modal');
  };
}

function DialogPanel({
  onClose,
  title,
  description,
  footer,
  size = 'md',
  dismissible = true,
  initialFocus,
  hideTitle = false,
  variant,
  class: className,
  children,
}: PanelProps) {
  const ref = useRef<HTMLDialogElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const pressedBackdrop = useRef(false);
  const auto = useId();
  const titleId = `d${auto}-t`;
  const descId = `d${auto}-d`;
  const latest = useRef({ onClose, dismissible });
  latest.current = { onClose, dismissible };

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const previous = document.activeElement;
    const unlock = lockScroll();
    if (!el.open) {
      try {
        el.showModal();
      } catch {
        el.setAttribute('open', '');
      }
    }
    const target =
      initialFocus?.current ??
      el.querySelector<HTMLElement>('[data-autofocus]') ??
      el.querySelector<HTMLElement>('[autofocus]') ??
      panelRef.current;
    target?.focus({ preventScroll: true });
    return () => {
      unlock();
      if (el.open) el.close();
      restoreFocus(previous);
    };
  }, []);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const onCancel = (event: Event): void => {
      event.preventDefault();
      if (latest.current.dismissible) latest.current.onClose();
    };
    // Some browsers close a modal dialog on repeated Esc even when cancel is
    // prevented. Keep the controlled state authoritative.
    const onNativeClose = (): void => {
      if (!el.isConnected) return;
      if (latest.current.dismissible) latest.current.onClose();
      else {
        try {
          el.showModal();
        } catch {
          el.setAttribute('open', '');
        }
      }
    };
    el.addEventListener('cancel', onCancel);
    el.addEventListener('close', onNativeClose);
    return () => {
      el.removeEventListener('cancel', onCancel);
      el.removeEventListener('close', onNativeClose);
    };
  }, []);

  return (
    <dialog
      ref={ref}
      class={cx('dialog', `dialog--${variant}`, `dialog--${size}`, className)}
      aria-labelledby={titleId}
      aria-describedby={description != null ? descId : undefined}
      aria-modal="true"
      onKeyDown={(event) => {
        if (!event.defaultPrevented && panelRef.current) trapTab(event, panelRef.current);
      }}
      onPointerDown={(event) => {
        pressedBackdrop.current = event.target === event.currentTarget;
      }}
      onClick={(event) => {
        if (pressedBackdrop.current && event.target === event.currentTarget && latest.current.dismissible) {
          latest.current.onClose();
        }
        pressedBackdrop.current = false;
      }}
    >
      <div class="dialog__panel" ref={panelRef} tabIndex={-1}>
        {variant === 'sheet' ? <span class="dialog__grabber" aria-hidden="true" /> : null}
        <header class={cx('dialog__header', hideTitle && 'dialog__header--hidden')}>
          <div class="dialog__titles">
            <h2 class={cx('dialog__title', hideTitle && 'visually-hidden')} id={titleId}>
              {title}
            </h2>
            {description != null ? (
              <p class="dialog__description" id={descId}>
                {description}
              </p>
            ) : null}
          </div>
          {dismissible ? (
            <IconButton icon="close" label="Close" class="dialog__close" onClick={() => latest.current.onClose()} />
          ) : null}
        </header>
        <div class="dialog__body">{children}</div>
        {footer != null ? <footer class="dialog__footer">{footer}</footer> : null}
      </div>
    </dialog>
  );
}

/**
 * Modal dialog (native <dialog> in the top layer): focus is trapped, Esc and
 * backdrop clicks close it when dismissible, and focus returns to the
 * previously focused element. Centered on desktop, a bottom sheet below 720 px.
 */
export function Dialog(props: DialogProps) {
  if (!props.open) return null;
  return <DialogPanel {...props} variant="dialog" />;
}

/**
 * Side panel on desktop (slides in from the end edge), bottom sheet below
 * 720 px. Same behaviour and props as Dialog; use it for details and pickers.
 */
export function Sheet(props: DialogProps) {
  if (!props.open) return null;
  return <DialogPanel {...props} variant="sheet" />;
}

