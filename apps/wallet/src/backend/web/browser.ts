/** Browser services of the web wallet: clipboard, explorer links, file export and import. */
import { BackendError } from '../types';
import { isExplorerUrl } from '../../lib/explorer';

/** Sensitive text (recovery words) is removed from the clipboard after this delay. */
export const CLIPBOARD_CLEAR_MS = 60_000;
export const MAX_IMPORT_BYTES = 1024 * 1024;

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
 * Clipboard writes. A sensitive copy is cleared after a minute if the last
 * thing this page copied is still that text; a copy made by the user in the
 * page in the meantime cancels the clearing. When the page is not focused at
 * that moment (browsers refuse clipboard writes then) the clearing happens as
 * soon as the page is focused again.
 */
export class Clipboard {
  private last: string | null = null;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private retry: (() => void) | null = null;
  private listening = false;

  async copy(text: string, sensitive: boolean): Promise<void> {
    // Sensitive text still waiting to be cleared.
    const pending = this.timer !== null || this.retry !== null ? this.last : null;
    this.cancelClear();
    try {
      await writeClipboard(text);
    } catch (e) {
      if (pending !== null) void this.clear(pending);
      throw e;
    }
    this.last = text;
    this.listen();
    if (sensitive) this.timer = setTimeout(() => void this.clear(text), CLIPBOARD_CLEAR_MS);
  }

  private listen(): void {
    if (this.listening || typeof document === 'undefined') return;
    this.listening = true;
    const forget = () => {
      this.last = null;
      this.cancelClear();
    };
    document.addEventListener('copy', forget, true);
    document.addEventListener('cut', forget, true);
  }

  private cancelClear(): void {
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    if (this.retry && typeof window !== 'undefined') window.removeEventListener('focus', this.retry);
    this.retry = null;
  }

  private async clear(text: string): Promise<void> {
    this.timer = null;
    if (this.last !== text) return;
    let cleared = false;
    if (typeof navigator !== 'undefined' && navigator.clipboard?.writeText) {
      try {
        await navigator.clipboard.writeText('');
        cleared = true;
      } catch {
        cleared = false;
      }
    }
    if (!cleared && this.last === text) cleared = copyWithSelection(' ');
    if (cleared) {
      if (this.last === text) this.last = null;
      return;
    }
    if (typeof window === 'undefined' || this.retry || this.last !== text) return;
    const retry = () => {
      window.removeEventListener('focus', retry);
      if (this.retry === retry) this.retry = null;
      void this.clear(text);
    };
    this.retry = retry;
    window.addEventListener('focus', retry);
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
 * Lets the user pick a JSON file (at most 1 MiB) and returns its text, or null
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
