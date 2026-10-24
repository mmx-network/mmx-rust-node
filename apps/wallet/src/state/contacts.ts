/** Address book and address labels. */
import { computed } from '@preact/signals';
import { BackendError, isBackendError, type Contact } from '../backend/types';
import {
  cleanLabel,
  hasErrors,
  LABEL_MAX,
  MAX_CONTACTS,
  mergeContacts,
  parseContactsFile,
  serializeContacts,
  validateContact,
  type ContactErrors,
  type ContactInput,
} from '../lib/appstate';
import { normalizeAddress } from '../lib/address';
import { randomId } from '../lib/id';
import { activeWalletId, appState, backend, contacts, persistAppState } from './core';
import { addresses } from './session';

export type ContactResult = { ok: true; contact: Contact } | { ok: false; errors: ContactErrors };

/** Contacts sorted by name. */
export const sortedContacts = computed(() =>
  contacts.value.slice().sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: 'base' })),
);

export function contactForAddress(address: string): Contact | undefined {
  const normalized = normalizeAddress(address);
  return normalized ? contacts.value.find((c) => c.address === normalized) : undefined;
}

/** Case-insensitive match on name, note or address. */
export function searchContacts(query: string, limit = 50): Contact[] {
  const q = query.trim().toLowerCase();
  const list = sortedContacts.value;
  if (!q) return list.slice(0, limit);
  return list
    .filter((c) => c.name.toLowerCase().includes(q) || c.address.includes(q) || (c.note?.toLowerCase().includes(q) ?? false))
    .slice(0, limit);
}

/** Field errors for a contact form (without saving). */
export function contactErrors(input: ContactInput, editingId?: string): ContactErrors {
  return validateContact(input, contacts.value, editingId, addresses.value);
}

function build(input: ContactInput, id: string): Contact {
  const note = (input.note ?? '').trim();
  const contact: Contact = { id, name: input.name.trim(), address: normalizeAddress(input.address)! };
  if (note) contact.note = note;
  return contact;
}

export async function addContact(input: ContactInput): Promise<ContactResult> {
  const errors = contactErrors(input);
  if (hasErrors(errors)) return { ok: false, errors };
  const contact = build(input, randomId('c-'));
  await persistAppState((s) => ({ ...s, contacts: [...s.contacts, contact] }));
  return { ok: true, contact };
}

export async function updateContact(id: string, input: ContactInput): Promise<ContactResult> {
  if (!contacts.value.some((c) => c.id === id)) throw new BackendError('invalid_argument');
  const errors = contactErrors(input, id);
  if (hasErrors(errors)) return { ok: false, errors };
  const contact = build(input, id);
  await persistAppState((s) => ({ ...s, contacts: s.contacts.map((c) => (c.id === id ? contact : c)) }));
  return { ok: true, contact };
}

/** Deletes a contact; returns it so the caller can offer undo via restoreContact. */
export async function deleteContact(id: string): Promise<Contact | undefined> {
  const contact = contacts.value.find((c) => c.id === id);
  if (!contact) return undefined;
  await persistAppState((s) => ({ ...s, contacts: s.contacts.filter((c) => c.id !== id) }));
  return contact;
}

export type RestoreResult = 'restored' | 'duplicate' | 'full';

/** Re-adds a deleted contact unless its address was saved again meanwhile or the address book is full. */
export async function restoreContact(contact: Contact): Promise<RestoreResult> {
  if (contacts.value.some((c) => c.address === contact.address)) return 'duplicate';
  if (contacts.value.length >= MAX_CONTACTS) return 'full';
  await persistAppState((s) => ({ ...s, contacts: [...s.contacts, contact] }));
  return 'restored';
}

/** Saves contacts to a file. Resolves false when cancelled. */
export async function exportContacts(): Promise<boolean> {
  return backend().exportFile('mmx-contacts.json', serializeContacts(sortedContacts.value));
}

export type ImportContactsResult =
  | { status: 'cancelled' }
  /** The file could not be read (desktop: permissions, a folder, a broken link). */
  | { status: 'unreadable' }
  | { status: 'invalid' }
  /** skipped: already saved or one of your addresses; full: left out because the address book is full. */
  | { status: 'imported'; added: number; skipped: number; full: number };

/** Reads a contacts file and adds contacts whose address is not saved yet. */
export async function importContacts(): Promise<ImportContactsResult> {
  let text: string | null;
  try {
    text = await backend().importFile();
  } catch (e) {
    if (isBackendError(e, 'io_error')) return { status: 'unreadable' };
    throw e;
  }
  if (text === null) return { status: 'cancelled' };
  const incoming = parseContactsFile(text);
  if (!incoming) return { status: 'invalid' };
  const own = new Set(addresses.value);
  const candidates = incoming.filter((c) => !own.has(c.address));
  const result = mergeContacts(contacts.value, candidates);
  if (result.added > 0) await persistAppState((s) => ({ ...s, contacts: mergeContacts(s.contacts, candidates).contacts }));
  return { status: 'imported', added: result.added, skipped: result.skipped + (incoming.length - candidates.length), full: result.full };
}

/** Sets or clears (empty text) the label of an address of the active wallet. */
export async function setAddressLabel(index: number, label: string): Promise<void> {
  const id = activeWalletId.value;
  if (!id) throw new BackendError('wallet_not_found');
  if (!Number.isInteger(index) || index < 0 || index >= 100) throw new BackendError('invalid_argument');
  if (Array.from(label.trim()).length > LABEL_MAX) throw new BackendError('invalid_argument');
  const clean = cleanLabel(label);
  if ((appState.value.labels[id]?.[index] ?? undefined) === clean) return;
  await persistAppState((s) => {
    const walletLabels = { ...(s.labels[id] ?? {}) };
    if (clean) walletLabels[index] = clean;
    else delete walletLabels[index];
    const labels = { ...s.labels };
    if (Object.keys(walletLabels).length > 0) labels[id] = walletLabels;
    else delete labels[id];
    return { ...s, labels };
  });
}
