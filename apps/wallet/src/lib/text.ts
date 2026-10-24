/**
 * Display safety for text that comes from the network (memos, token symbols
 * and names) or from files: control and bidirectional override characters are
 * removed and the length is capped. Always render the result as text.
 */

// C0 controls except tab/newline/CR (handled as line breaks), DEL, C1 controls.
const CONTROL_CLASS = '\\u{0}-\\u{8}\\u{b}\\u{c}\\u{e}-\\u{1f}\\u{7f}-\\u{84}\\u{86}-\\u{9f}';
const LINE_BREAK_CLASS = '\\t\\n\\r\\u{85}\\u{2028}\\u{2029}';
// Bidi embeddings/overrides/isolates and marks, zero-width space, word joiner, BOM, soft hyphen.
const INVISIBLE_CLASS = '\\u{202a}-\\u{202e}\\u{2066}-\\u{2069}\\u{200e}\\u{200f}\\u{61c}\\u{200b}\\u{2060}\\u{feff}\\u{ad}';

const CONTROL = new RegExp(`[${CONTROL_CLASS}]`, 'gu');
const LINE_BREAKS = new RegExp(`[${LINE_BREAK_CLASS}]+`, 'gu');
const INVISIBLE = new RegExp(`[${INVISIBLE_CLASS}]`, 'gu');
const UNSAFE = new RegExp(`[${CONTROL_CLASS}${LINE_BREAK_CLASS}${INVISIBLE_CLASS}]`, 'u');

const ELLIPSIS = '\u{2026}';

/** Strips control/bidi characters, collapses line breaks to spaces, caps length (in code points). */
export function sanitizeText(value: unknown, maxLength = 256): string {
  if (typeof value !== 'string') return '';
  const cleaned = value.replace(LINE_BREAKS, ' ').replace(CONTROL, '').replace(INVISIBLE, '');
  const points = Array.from(cleaned);
  if (points.length <= maxLength) return cleaned;
  return points.slice(0, maxLength).join('');
}

/** Token symbols and names: single line, trimmed, at most 24 characters. */
export function sanitizeSymbol(value: unknown): string {
  return sanitizeText(value, 24).trim();
}

/** Memos are at most 64 bytes on chain. */
export function sanitizeMemo(value: unknown): string {
  return sanitizeText(value, 64);
}

/** True when the string contains characters sanitizeText would remove or change. */
export function hasUnsafeCharacters(value: string): boolean {
  return UNSAFE.test(value);
}

/** Shortens a hash such as a transaction id: "1A2B3C…7Y8Z". */
export function shortenHash(hash: string, head = 6, tail = 6): string {
  if (hash.length <= head + tail + 1) return hash;
  return `${hash.slice(0, head)}${ELLIPSIS}${hash.slice(-tail)}`;
}

export type NameError = 'empty' | 'too_long' | 'invalid';

/** Validates a user-entered name (wallet, contact, label): 1..max characters after trim, no control characters. */
export function validateName(value: string, max = 64): NameError | null {
  const trimmed = value.trim();
  if (!trimmed) return 'empty';
  if (Array.from(trimmed).length > max) return 'too_long';
  if (hasUnsafeCharacters(trimmed)) return 'invalid';
  return null;
}
