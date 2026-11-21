/** TextEncoder replaces lone UTF-16 surrogates; never sign that altered text. */
export function validMessage(value: unknown): value is string {
  return typeof value === 'string' && value.length > 0 && value.length <= 4096
    && !/[\ud800-\udfff]/u.test(value)
    && new TextEncoder().encode(value).length <= 4096;
}
