import type { ComponentChildren } from 'preact';
import { useEffect, useLayoutEffect, useRef } from 'preact/hooks';
import { Icon, type IconName } from './Icon';
import { IconButton } from './IconButton';
import { cx } from './internal/cx';
import { showInTopLayer } from './internal/floating';

export interface Toast {
  id: string;
  tone?: 'info' | 'positive' | 'warning' | 'negative';
  title: ComponentChildren;
  message?: ComponentChildren;
  action?: { label: string; onClick: () => void };
  /** Auto-dismiss after this many ms (default 5000; 0 keeps it until dismissed). */
  timeout?: number;
}

export interface ToastViewportProps {
  toasts: Toast[];
  onDismiss: (id: string) => void;
}

const toneIcon: Record<NonNullable<Toast['tone']>, IconName> = {
  info: 'info',
  positive: 'check-circle',
  warning: 'warning',
  negative: 'x-circle',
};

function ToastItem({ toast, onDismiss }: { toast: Toast; onDismiss: (id: string) => void }) {
  const duration = toast.timeout ?? 5000;
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const remaining = useRef(duration);
  const started = useRef(0);

  const stop = (): void => {
    if (!timer.current) return;
    clearTimeout(timer.current);
    timer.current = null;
    remaining.current -= Date.now() - started.current;
  };
  const start = (): void => {
    if (duration === 0 || timer.current) return;
    started.current = Date.now();
    timer.current = setTimeout(() => onDismiss(toast.id), Math.max(remaining.current, 1500));
  };
  useEffect(() => {
    start();
    return stop;
  }, [toast.id]);

  const tone = toast.tone ?? 'info';
  return (
    <div
      class={cx('toast', `toast--${tone}`)}
      role={tone === 'negative' ? 'alert' : 'status'}
      onPointerEnter={stop}
      onPointerLeave={start}
      onFocusIn={stop}
      onFocusOut={start}
    >
      <Icon name={toneIcon[tone]} class="toast__icon" />
      <div class="toast__content">
        <p class="toast__title">{toast.title}</p>
        {toast.message != null ? <p class="toast__message">{toast.message}</p> : null}
      </div>
      {toast.action ? (
        <button
          type="button"
          class="toast__action"
          onClick={() => {
            toast.action?.onClick();
            onDismiss(toast.id);
          }}
        >
          {toast.action.label}
        </button>
      ) : null}
      <IconButton icon="close" label="Dismiss" size="sm" class="toast__close" onClick={() => onDismiss(toast.id)} />
    </div>
  );
}

/** Renders the given toasts in a fixed live region above all other content. */
export function ToastViewport({ toasts, onDismiss }: ToastViewportProps) {
  const ref = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    if (ref.current && toasts.length > 0) showInTopLayer(ref.current);
  }, [toasts.length, toasts[toasts.length - 1]?.id]);
  return (
    <div ref={ref} class={cx('toasts', toasts.length === 0 && 'toasts--empty')} aria-label="Notifications" role="region">
      {toasts.map((t) => (
        <ToastItem key={t.id} toast={t} onDismiss={onDismiss} />
      ))}
    </div>
  );
}
