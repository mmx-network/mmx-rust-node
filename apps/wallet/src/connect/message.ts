/** TextEncoder replaces lone UTF-16 surrogates; never sign that altered text. */
export function validMessage(value: unknown): value is string {
  return typeof value === 'string' && value.length > 0 && value.length <= 4096
    && !/[\ud800-\udfff]/u.test(value)
    && new TextEncoder().encode(value).length <= 4096;
}

/** Expose invisible/reordering controls without changing the bytes we sign.
 * Escape literal backslashes too so the representation stays unambiguous.
 * LF remains a line break for readable multiline challenges. */
export function messageForReview(message: string): string {
  return message.replace(/\\/gu, '\\\\').replace(
    /[\u0000-\u0009\u000b-\u001f\u007f-\u009f\u061c\u200b-\u200f\u2028-\u202e\u2060-\u206f\ufeff]/gu,
    c => `\\u${c.charCodeAt(0).toString(16).padStart(4, '0')}`,
  );
}
