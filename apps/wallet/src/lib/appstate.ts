/**
 * Validation of persisted app state (labels, contacts, settings) and of
 * contact files. Everything read from storage or files passes through here.
 */
import type { AppState, Contact, Platform, Settings, ThemeSetting, WalletId } from '../backend/types';
import { normalizeAddress, NATIVE_CONTRACT } from './address';
import { DEFAULT_FEE_LEVEL, isFeeLevel } from './fees';
import { randomId } from './id';
import { sanitizeText, validateName, type NameError } from './text';

export const MAX_ADDRESSES = 100;
export const DEFAULT_ADDRESS_COUNT = 3;
export const LABEL_MAX = 32;
export const WALLET_NAME_MAX = 64;
export const CONTACT_NAME_MAX = 64;
export const CONTACT_NOTE_MAX = 200;
export const MAX_CONTACTS = 1000;
export const DEFAULT_AUTO_LOCK_MINUTES = 5;
const AUTO_LOCK_OPTIONS = [5, 15, 30, 60, 6 * 60, 24 * 60, 0] as const;
export const AUTO_LOCK_CHOICES: Record<Platform, readonly number[]> = {
  desktop: AUTO_LOCK_OPTIONS,
  web: AUTO_LOCK_OPTIONS,
};
const MAX_AUTO_LOCK_MINUTES = 24 * 60;
const CONTACT_ID = /^[A-Za-z0-9_-]{1,64}$/;

export function defaultSettings(): Settings {
  return { theme: 'system', autoLockMinutes: DEFAULT_AUTO_LOCK_MINUTES, feeLevel: DEFAULT_FEE_LEVEL };
}

export function defaultAppState(): AppState {
  return { labels: {}, contacts: [], settings: defaultSettings() };
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

function isTheme(v: unknown): v is ThemeSetting {
  return v === 'system' || v === 'light' || v === 'dark';
}

export function sanitizeSettings(raw: unknown, platform: Platform): Settings {
  const s = defaultSettings();
  if (!isRecord(raw)) return s;
  if (isTheme(raw.theme)) s.theme = raw.theme;
  const minutes = raw.autoLockMinutes;
  if (typeof minutes === 'number' && Number.isInteger(minutes) && minutes >= 0 && minutes <= MAX_AUTO_LOCK_MINUTES) {
    s.autoLockMinutes = minutes;
  }
  if (isFeeLevel(raw.feeLevel)) s.feeLevel = raw.feeLevel;
  if (platform === 'desktop' && typeof raw.rpcUrl === 'string') {
    const url = raw.rpcUrl.trim();
    if (url && url.length <= 512 && !/[\s\u{0}-\u{1f}\u{7f}]/u.test(url)) s.rpcUrl = url;
  }
  return s;
}

/** A label is trimmed, single-line text of at most LABEL_MAX characters; empty → undefined. */
export function cleanLabel(value: unknown): string | undefined {
  const text = sanitizeText(value, LABEL_MAX).trim();
  return text || undefined;
}

export function sanitizeLabels(raw: unknown): Record<WalletId, Record<number, string>> {
  const out: Record<WalletId, Record<number, string>> = {};
  if (!isRecord(raw)) return out;
  for (const [walletId, labels] of Object.entries(raw)) {
    if (!walletId || walletId.length > 256 || !isRecord(labels)) continue;
    const clean: Record<number, string> = {};
    for (const [key, value] of Object.entries(labels)) {
      if (!/^\d{1,3}$/.test(key)) continue;
      const index = Number(key);
      if (index >= MAX_ADDRESSES) continue;
      const label = cleanLabel(value);
      if (label) clean[index] = label;
    }
    if (Object.keys(clean).length > 0) out[walletId] = clean;
  }
  return out;
}

/** Validates one stored contact; returns null if unusable. */
export function sanitizeContact(raw: unknown): Contact | null {
  if (!isRecord(raw)) return null;
  const address = typeof raw.address === 'string' ? normalizeAddress(raw.address) : null;
  if (!address || address === NATIVE_CONTRACT) return null;
  const name = sanitizeText(raw.name, CONTACT_NAME_MAX).trim();
  if (!name) return null;
  const id = typeof raw.id === 'string' && CONTACT_ID.test(raw.id) ? raw.id : randomId('c-');
  const note = sanitizeText(raw.note, CONTACT_NOTE_MAX).trim();
  return note ? { id, name, address, note } : { id, name, address };
}

/** Valid, unique-by-address and unique-by-id contacts (first wins), capped at MAX_CONTACTS. */
export function sanitizeContacts(raw: unknown): Contact[] {
  if (!Array.isArray(raw)) return [];
  const out: Contact[] = [];
  const addresses = new Set<string>();
  const ids = new Set<string>();
  for (const item of raw) {
    if (out.length >= MAX_CONTACTS) break;
    const c = sanitizeContact(item);
    if (!c || addresses.has(c.address)) continue;
    if (ids.has(c.id)) c.id = randomId('c-');
    addresses.add(c.address);
    ids.add(c.id);
    out.push(c);
  }
  return out;
}

export function sanitizeAppState(raw: unknown, platform: Platform): AppState {
  const state = defaultAppState();
  if (!isRecord(raw)) return state;
  if (typeof raw.activeWallet === 'string' && raw.activeWallet && raw.activeWallet.length <= 256) {
    state.activeWallet = raw.activeWallet;
  }
  state.labels = sanitizeLabels(raw.labels);
  state.contacts = sanitizeContacts(raw.contacts);
  state.settings = sanitizeSettings(raw.settings, platform);
  return state;
}

export interface ContactInput {
  name: string;
  address: string;
  note?: string;
}

export interface ContactErrors {
  name?: NameError;
  address?: 'empty' | 'invalid' | 'duplicate' | 'own' | 'full';
  note?: 'too_long' | 'invalid';
}

/**
 * Validates a contact being added or edited. `editingId` excludes that contact
 * from the duplicate check; `ownAddresses` rejects the wallet's own addresses.
 */
export function validateContact(
  input: ContactInput,
  existing: readonly Contact[],
  editingId?: string,
  ownAddresses: readonly string[] = [],
): ContactErrors {
  const errors: ContactErrors = {};
  const nameError = validateName(input.name, CONTACT_NAME_MAX);
  if (nameError) errors.name = nameError;
  const raw = input.address.trim();
  if (!raw) errors.address = 'empty';
  else {
    const address = normalizeAddress(raw);
    if (!address || address === NATIVE_CONTRACT) errors.address = 'invalid';
    else if (existing.some((c) => c.address === address && c.id !== editingId)) errors.address = 'duplicate';
    else if (ownAddresses.includes(address)) errors.address = 'own';
    else if (editingId === undefined && existing.length >= MAX_CONTACTS) errors.address = 'full';
  }
  const note = input.note ?? '';
  if (Array.from(note.trim()).length > CONTACT_NOTE_MAX) errors.note = 'too_long';
  else if (note && sanitizeText(note, CONTACT_NOTE_MAX) !== note) errors.note = 'invalid';
  return errors;
}

export function hasErrors(errors: object): boolean {
  return Object.values(errors).some((v) => v !== undefined);
}

const FILE_FORMAT = 'mmx-wallet-contacts';

/** Contacts file contents (pretty JSON, ids omitted). */
export function serializeContacts(contacts: readonly Contact[]): string {
  return (
    JSON.stringify(
      {
        format: FILE_FORMAT,
        version: 1,
        contacts: contacts.map((c) => (c.note ? { name: c.name, address: c.address, note: c.note } : { name: c.name, address: c.address })),
      },
      null,
      2,
    ) + '\n'
  );
}

/** Largest contacts file accepted for import, on every platform (desktop: src-tauri files.rs). */
export const CONTACTS_FILE_MAX_BYTES = 2 * 1024 * 1024;

/**
 * Parses a contacts file: this app's format, or a plain array of
 * `{name, address, note?}`. Returns null when the file is not a contacts file.
 * Invalid entries are dropped.
 */
export function parseContactsFile(text: string): Contact[] | null {
  if (text.length > CONTACTS_FILE_MAX_BYTES) return null;
  let data: unknown;
  try {
    data = JSON.parse(text);
  } catch {
    return null;
  }
  let list: unknown;
  if (Array.isArray(data)) list = data;
  else if (isRecord(data) && Array.isArray(data.contacts)) list = data.contacts;
  else return null;
  // Imported ids are never trusted: fresh ids avoid collisions with existing contacts.
  const items = (list as unknown[]).map((item) => (isRecord(item) ? { ...item, id: undefined } : item));
  return sanitizeContacts(items);
}

export interface MergeResult {
  contacts: Contact[];
  added: number;
  /** Addresses already saved (or repeated in the file). */
  skipped: number;
  /** New addresses left out because the address book is full. */
  full: number;
}

/** Adds incoming contacts whose address is not saved yet; existing contacts are kept unchanged. */
export function mergeContacts(existing: readonly Contact[], incoming: readonly Contact[]): MergeResult {
  const contacts = existing.slice();
  const addresses = new Set(existing.map((c) => c.address));
  const ids = new Set(existing.map((c) => c.id));
  let added = 0;
  let skipped = 0;
  let full = 0;
  for (const c of incoming) {
    if (addresses.has(c.address)) {
      skipped++;
      continue;
    }
    if (contacts.length >= MAX_CONTACTS) {
      full++;
      continue;
    }
    const id = ids.has(c.id) ? randomId('c-') : c.id;
    contacts.push({ ...c, id });
    addresses.add(c.address);
    ids.add(id);
    added++;
  }
  return { contacts, added, skipped, full };
}
