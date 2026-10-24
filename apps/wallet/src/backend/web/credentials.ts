/**
 * Password manager integration for the web wallet (docs/wallet-app.md §6.6).
 *
 * Credential: username = "<wallet name> (<fingerprint>)", password = the 24
 * recovery words (lowercase, single spaces). Nothing here stores, logs or
 * returns the words anywhere except to the caller. Every function works (and
 * never throws) in browsers without the Credential Management API.
 */
import { WALLET_NAME_MAX } from '../../lib/appstate';
import { sanitizeText } from '../../lib/text';

/** Chromium-only Credential Management types (not in the standard DOM typings). */
interface PasswordCredentialData {
  id: string;
  password: string;
  name?: string;
}

interface PasswordCredentialLike extends Credential {
  readonly password?: string;
  readonly name?: string;
}

type PasswordCredentialConstructor = new (data: PasswordCredentialData | HTMLFormElement) => PasswordCredentialLike;

const MAX_FINGERPRINT = 4_294_967_295;
const USERNAME_PATTERN = /^(.*?)\s*\((\d{1,10})\)\s*$/su;
const DEFAULT_NAME = 'Wallet';

function cleanName(name: unknown): string {
  return sanitizeText(name, WALLET_NAME_MAX).trim();
}

/** Username saved with the credential. */
export function credentialUsername(name: string, fingerprint: string): string {
  return `${cleanName(name) || DEFAULT_NAME} (${fingerprint})`;
}

export interface ParsedUsername {
  /** Display-safe wallet name (1–64 characters). */
  name: string;
  /** Present when the username ends in "(<u32>)". */
  fingerprint?: string;
}

/** Splits a saved username into wallet name and fingerprint. */
export function parseCredentialUsername(username: string): ParsedUsername {
  const text = typeof username === 'string' ? username.trim() : '';
  const m = USERNAME_PATTERN.exec(text);
  if (m) {
    const digits = m[2]!;
    const value = Number(digits);
    if (Number.isSafeInteger(value) && value <= MAX_FINGERPRINT && String(value) === digits) {
      return { name: cleanName(m[1]) || DEFAULT_NAME, fingerprint: digits };
    }
  }
  return { name: cleanName(text) || DEFAULT_NAME };
}

function passwordCredentialClass(): PasswordCredentialConstructor | null {
  try {
    if (typeof window === 'undefined') return null;
    const ctor = (window as unknown as { PasswordCredential?: unknown }).PasswordCredential;
    return typeof ctor === 'function' ? (ctor as PasswordCredentialConstructor) : null;
  } catch {
    return null;
  }
}

function credentialsContainer(): CredentialsContainer | null {
  try {
    if (typeof navigator === 'undefined') return null;
    const c = navigator.credentials as CredentialsContainer | undefined;
    return c && typeof c.get === 'function' && typeof c.store === 'function' ? c : null;
  } catch {
    return null;
  }
}

/** True when the browser offers the Credential Management API for passwords (Chromium). */
export function canUseCredentialApi(): boolean {
  return passwordCredentialClass() !== null && credentialsContainer() !== null;
}

/**
 * Asks the browser to save the credential (Chromium). Other browsers and
 * password manager extensions detect the submitted form instead. Resolves
 * false when the API is unavailable or the user declined.
 */
export async function storeCredential(username: string, words: string): Promise<boolean> {
  const Ctor = passwordCredentialClass();
  const container = credentialsContainer();
  if (!Ctor || !container || typeof username !== 'string' || !username || typeof words !== 'string' || !words) {
    return false;
  }
  try {
    const credential = new Ctor({ id: username, password: words, name: username });
    await container.store(credential);
    return true;
  } catch {
    return false;
  }
}

export interface StoredCredential {
  username: string;
  words: string;
}

/**
 * Offers the credentials saved for this site (Chromium, `mediation: 'optional'`:
 * the browser may show an account chooser). Resolves null when unavailable,
 * cancelled or not a password credential. The caller checks that the
 * username belongs to the wallet being opened.
 */
export async function getCredential(): Promise<StoredCredential | null> {
  const container = credentialsContainer();
  if (!container || !canUseCredentialApi()) return null;
  try {
    const options = { password: true, mediation: 'optional' } as unknown as CredentialRequestOptions;
    const credential = (await container.get(options)) as PasswordCredentialLike | null;
    if (
      !credential ||
      credential.type !== 'password' ||
      typeof credential.id !== 'string' ||
      typeof credential.password !== 'string' ||
      !credential.password
    ) {
      return null;
    }
    return { username: credential.id, words: credential.password };
  } catch {
    return null;
  }
}

/** Same as getCredential. */
export const requestCredential = getCredential;

/**
 * Stops the browser from handing out a saved credential without asking the
 * user again (called when wallets are locked).
 */
export async function preventSilentAccess(): Promise<void> {
  try {
    if (typeof navigator === 'undefined') return;
    const c = navigator.credentials as CredentialsContainer | undefined;
    if (!c || typeof c.preventSilentAccess !== 'function') return;
    await c.preventSilentAccess();
  } catch {
    // Not supported: nothing to do.
  }
}
