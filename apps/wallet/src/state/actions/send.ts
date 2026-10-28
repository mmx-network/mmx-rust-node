/** Send screen helpers: reading a recipient from the clipboard. */
import { normalizeAddress } from '../../lib/address';

/** Longest text kept from a paste that is not an address. */
const PASTE_MAX = 128;
/** Candidate addresses inside free text (bech32 characters, either case). */
const ADDRESS_IN_TEXT = /mmx1[qpzry9x8gf2tvdw0s3jn54khce6mua7l]{20,}/gi;

/** Whether the app may read the clipboard when the user asks for it (Paste button). */
export function canPasteFromClipboard(): boolean {
  if (typeof navigator === 'undefined' || typeof navigator.clipboard?.readText !== 'function') return false;
  // A Permissions-Policy that turns clipboard reading off (Chromium reports it).
  type Policy = { allowsFeature?: (feature: string) => boolean };
  const doc = typeof document === 'undefined' ? undefined : (document as Document & { permissionsPolicy?: Policy; featurePolicy?: Policy });
  const policy = doc?.permissionsPolicy ?? doc?.featurePolicy;
  return policy?.allowsFeature?.('clipboard-read') !== false;
}

/** Reads text from the clipboard. Resolves null when reading is not allowed or nothing is there. */
export async function readClipboardText(): Promise<string | null> {
  if (!canPasteFromClipboard()) return null;
  try {
    const text = await navigator.clipboard.readText();
    return typeof text === 'string' && text.trim() !== '' ? text : null;
  } catch {
    return null;
  }
}

/**
 * The recipient to put in the To field for pasted text: the text itself when
 * it is an address, the single address it contains, or the trimmed text
 * (shown with a validation error). Text with several addresses is not guessed.
 */
export function recipientFromText(text: string): string {
  const trimmed = text.trim();
  if (normalizeAddress(trimmed)) return trimmed;
  const found = new Set<string>();
  for (const match of trimmed.matchAll(ADDRESS_IN_TEXT)) {
    const address = normalizeAddress(match[0]);
    if (address) found.add(address);
  }
  if (found.size === 1) return [...found][0]!;
  return Array.from(trimmed).slice(0, PASTE_MAX).join('');
}

/** QR scans accept an address or MMX URI; never open a scanned link. */
export function recipientFromQr(text: string): string | null {
  if (text.length > 2048) return null;
  const value = text.trim();
  const uri = /^mmx:(?:\/\/)?([^?#]+)(?:\?[^#]*)?$/i.exec(value);
  return normalizeAddress(uri ? uri[1]! : value);
}
