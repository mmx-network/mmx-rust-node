/** Browser services of the web wallet: clipboard, explorer links, file export and import. */
import { BackendError } from '../types';
import { CONTACTS_FILE_MAX_BYTES } from '../../lib/appstate';
import { isExplorerUrl } from '../../lib/explorer';

/** Sensitive text (recovery words) is removed from the clipboard after this delay. */
export const CLIPBOARD_CLEAR_MS = 60_000;
export const MAX_IMPORT_BYTES = CONTACTS_FILE_MAX_BYTES;

async function writeClipboard(text: string): Promise<void> {
  if (typeof navigator !== 'undefined' && navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(text);
      return;
    } catch {
      // Fall back to a selection copy below.
    }
  }
  if (!copyWithSelection(text)) throw new BackendError('internal');
}

/** Older fallback for browsers without (or refusing) the asynchronous clipboard API. */
function copyWithSelection(text: string): boolean {
  if (typeof document === 'undefined' || !document.body) return false;
  const area = document.createElement('textarea');
  area.value = text;
  area.setAttribute('readonly', '');
  area.setAttribute('aria-hidden', 'true');
  area.style.position = 'fixed';
  area.style.top = '0';
  area.style.left = '0';
  area.style.opacity = '0';
  area.style.pointerEvents = 'none';
  const active = document.activeElement as HTMLElement | null;
  document.body.appendChild(area);
  let ok = false;
  try {
    area.select();
    ok = document.execCommand('copy');
  } catch {
    ok = false;
  } finally {
    area.value = '';
    area.remove();
    active?.focus?.();
  }
  return ok;
}

/**
 * Events after which a refused clearing is tried again. Browsers accept
 * clipboard writes only while the page has focus (Chromium) or during a user
 * gesture (Firefox, Safari: pointerdown with a mouse, pointerup on touch,
 * keydown). pagehide is a last attempt before the page goes away.
 */
const RETRY_EVENTS = ['pointerdown', 'pointerup', 'keydown', 'focus', 'pagehide'] as const;

/**
 * Clipboard writes. A sensitive copy (recovery words) is cleared after a
 * minute, or earlier when the wallets are locked. A later copy, by this page
 * or by the user in the page, replaces it and cancels the clearing.
 *
 * Clearing is best effort: when the browser refuses the write (page not
 * focused, no user gesture) it is tried again on the next click, tap or key
 * press in the page, when the page is focused again and when it is hidden.
 * Nothing can clear the clipboard once the page is closed.
 */
export class Clipboard {
  private seq = 0;
  /** Id of the sensitive copy still to be cleared; 0 = none. */
  private pending = 0;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private retry: (() => void) | null = null;
  private listening = false;

  async copy(text: string, sensitive: boolean): Promise<void> {
    const pending = this.pending;
    this.cancelClear();
    try {
      await writeClipboard(text);
    } catch (e) {
      // The earlier sensitive text is still on the clipboard: clear it now.
      if (pending !== 0) {
        this.pending = pending;
        this.attempt(pending);
      }
      throw e;
    }
    this.listen();
    if (!sensitive) return;
    const id = ++this.seq;
    this.pending = id;
    this.timer = setTimeout(() => {
      this.timer = null;
      this.attempt(id);
    }, CLIPBOARD_CLEAR_MS);
  }

  /** Clears sensitive text at once (wallets locked) instead of waiting for the timer. */
  clearPending(): void {
    const id = this.pending;
    if (id === 0) return;
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    this.attempt(id);
  }

  private listen(): void {
    if (this.listening || typeof document === 'undefined') return;
    this.listening = true;
    // The user copied something in the page: the clipboard no longer holds our text.
    const forget = () => this.cancelClear();
    document.addEventListener('copy', forget, true);
    document.addEventListener('cut', forget, true);
  }

  private cancelClear(): void {
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    this.pending = 0;
    this.disarm();
  }

  private disarm(): void {
    const retry = this.retry;
    this.retry = null;
    if (retry && typeof window !== 'undefined') {
      for (const type of RETRY_EVENTS) window.removeEventListener(type, retry, true);
    }
  }

  /**
   * One clearing attempt. The clipboard is called synchronously, so an attempt
   * from an event handler runs inside the user gesture (Safari drops the
   * gesture across an await).
   */
  private attempt(id: number): void {
    if (this.pending !== id) return;
    let write: Promise<void> | null = null;
    const clipboard = typeof navigator !== 'undefined' ? navigator.clipboard : undefined;
    if (clipboard?.writeText) {
      try {
        write = clipboard.writeText('');
      } catch {
        write = null;
      }
    }
    if (write === null) {
      if (copyWithSelection(' ')) this.cleared(id);
      else this.arm(id);
      return;
    }
    write.then(
      () => this.cleared(id),
      () => this.arm(id),
    );
  }

  private cleared(id: number): void {
    if (this.pending !== id) return;
    this.pending = 0;
    this.disarm();
  }

  private arm(id: number): void {
    if (this.pending !== id || this.retry !== null || typeof window === 'undefined') return;
    const retry = () => this.attempt(id);
    this.retry = retry;
    for (const type of RETRY_EVENTS) window.addEventListener(type, retry, true);
  }
}

/** Opens a block explorer page in a new tab without giving it access to this page. */
export function openExplorer(url: string): void {
  if (!isExplorerUrl(url)) throw new BackendError('invalid_argument');
  if (typeof window === 'undefined') throw new BackendError('internal');
  window.open(url, '_blank', 'noopener,noreferrer');
}

/** A plain file name: letters, digits, dot, dash and underscore. */
export function safeFileName(name: string): string {
  const clean = name
    .replace(/[^A-Za-z0-9._-]+/gu, '-')
    .replace(/^[.-]+/u, '')
    .slice(0, 100);
  return clean || 'export.json';
}

/** Saves text as a download. Browsers do not report a cancelled save dialog. */
export function downloadFile(suggestedName: string, contents: string): boolean {
  if (typeof document === 'undefined' || !document.body) throw new BackendError('internal');
  const url = URL.createObjectURL(new Blob([contents], { type: 'application/json' }));
  const a = document.createElement('a');
  a.href = url;
  a.download = safeFileName(suggestedName);
  a.rel = 'noopener';
  a.hidden = true;
  document.body.appendChild(a);
  try {
    a.click();
  } finally {
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 30_000);
  }
  return true;
}

/**
 * Lets the user pick a JSON file (at most 2 MiB) and returns its text, or null
 * when the dialog was cancelled. Must be called from a user action: the file
 * dialog opens synchronously.
 */
export function pickTextFile(): Promise<string | null> {
  if (typeof document === 'undefined' || !document.body) return Promise.reject(new BackendError('internal'));
  return new Promise((resolve, reject) => {
    const input = document.createElement('input');
    input.type = 'file';
    input.accept = 'application/json,.json';
    input.hidden = true;
    let done = false;
    let focusTimer: ReturnType<typeof setTimeout> | null = null;
    const cleanup = (): boolean => {
      if (done) return false;
      done = true;
      if (focusTimer !== null) clearTimeout(focusTimer);
      window.removeEventListener('focus', onFocus);
      input.remove();
      return true;
    };
    // Browsers without the "cancel" event: focus returning without a selected file means cancelled.
    const onFocus = () => {
      if (focusTimer !== null) clearTimeout(focusTimer);
      focusTimer = setTimeout(() => {
        if ((!input.files || input.files.length === 0) && cleanup()) resolve(null);
      }, 1000);
    };
    input.addEventListener('cancel', () => {
      if (cleanup()) resolve(null);
    });
    input.addEventListener('change', () => {
      const file = input.files?.[0];
      if (!cleanup()) return;
      if (!file) resolve(null);
      else if (file.size > MAX_IMPORT_BYTES) reject(new BackendError('invalid_argument'));
      else {
        file.text().then(
          (text) => (text.length > MAX_IMPORT_BYTES ? reject(new BackendError('invalid_argument')) : resolve(text)),
          () => reject(new BackendError('io_error')),
        );
      }
    });
    document.body.appendChild(input);
    window.addEventListener('focus', onFocus);
    input.click();
  });
}
