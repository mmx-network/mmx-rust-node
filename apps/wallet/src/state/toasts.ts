/**
 * Short-lived notifications. The toast viewport renders them and dismisses
 * them after `timeout` ms (pausing while hovered or focused).
 */
import { signal } from '@preact/signals';

export type ToastKind = 'success' | 'error' | 'info' | 'warning';

export interface Toast {
  id: number;
  kind: ToastKind;
  message: string;
  /** Auto-dismiss delay in ms; 0 keeps the toast until dismissed. */
  timeout: number;
  action?: { label: string; onClick: () => void };
}

export const toasts = signal<readonly Toast[]>([]);

const MAX_TOASTS = 3;
let nextId = 1;

export interface NotifyOptions {
  timeout?: number;
  action?: { label: string; onClick: () => void };
}

function defaultTimeout(kind: ToastKind): number {
  return kind === 'error' || kind === 'warning' ? 7000 : 4000;
}

/** Shows a toast (replacing one with the same text); returns its id. */
export function notify(message: string, kind: ToastKind = 'info', options: NotifyOptions | number = {}): number {
  const opts = typeof options === 'number' ? { timeout: options } : options;
  const id = nextId++;
  const toast: Toast = { id, kind, message, timeout: opts.timeout ?? defaultTimeout(kind) };
  if (opts.action) toast.action = opts.action;
  const rest = toasts.value.filter((t) => t.message !== message);
  toasts.value = [...rest, toast].slice(-MAX_TOASTS);
  return id;
}

export function dismissToast(id: number): void {
  if (toasts.value.some((t) => t.id === id)) toasts.value = toasts.value.filter((t) => t.id !== id);
}

export function clearToasts(): void {
  toasts.value = [];
}
