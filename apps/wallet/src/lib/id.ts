/** Random identifiers (not secrets) from the platform CSPRNG. */
export function randomId(prefix: string, bytes = 8): string {
  const buf = new Uint8Array(bytes);
  crypto.getRandomValues(buf);
  return prefix + Array.from(buf, (b) => b.toString(16).padStart(2, '0')).join('');
}
