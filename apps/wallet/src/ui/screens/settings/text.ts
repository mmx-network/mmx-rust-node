/** User-facing text helpers for the settings screen. */

/** "Address 12 holds funds." / "Addresses 11, 12 and 14 hold funds." (indices are 0-based). */
export function fundedMessage(indices: readonly number[]): string {
  const numbers = indices.map((i) => i + 1);
  if (numbers.length === 0) return '';
  if (numbers.length === 1) return `Address ${numbers[0]} holds funds.`;
  const shown = numbers.slice(0, 5);
  const rest = numbers.length - shown.length;
  const list = rest > 0 ? `${shown.join(', ')} and ${rest} more` : `${shown.slice(0, -1).join(', ')} and ${shown[shown.length - 1]}`;
  return `Addresses ${list} hold funds.`;
}

/** "5 minutes", "1 hour", "Never". */
export function autoLockLabel(minutes: number): string {
  if (minutes <= 0) return 'Never';
  if (minutes % 60 === 0) {
    const hours = minutes / 60;
    return hours === 1 ? '1 hour' : `${hours} hours`;
  }
  return minutes === 1 ? '1 minute' : `${minutes} minutes`;
}

/** True for http:// servers that are not on this computer. */
export function isUnencryptedRemote(url: string): boolean {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return false;
  }
  if (parsed.protocol !== 'http:') return false;
  const host = parsed.hostname.replace(/^\[|\]$/g, '').toLowerCase();
  return !(host === 'localhost' || host.endsWith('.localhost') || host === '::1' || /^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(host));
}
