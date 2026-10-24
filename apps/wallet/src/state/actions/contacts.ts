/** Contacts screen: user-facing validation messages, import/export and delete with undo. */
import { signal } from '@preact/signals';
import { isBackendError, type Contact } from '../../backend/types';
import { CONTACT_NAME_MAX, CONTACT_NOTE_MAX, MAX_CONTACTS, type ContactErrors } from '../../lib/appstate';
import { errorMessage } from '../../lib/errors';
import { contactForAddress, deleteContact, exportContacts, importContacts, restoreContact, type ImportContactsResult } from '../contacts';
import { notify, type ToastKind } from '../toasts';

export interface ContactMessages {
  name?: string;
  address?: string;
  note?: string;
}

/** One line per invalid field. `address` is used to name the contact that already has it. */
export function contactErrorMessages(errors: ContactErrors, address = ''): ContactMessages {
  const out: ContactMessages = {};
  switch (errors.name) {
    case 'empty':
      out.name = 'Enter a name.';
      break;
    case 'too_long':
      out.name = `Use up to ${CONTACT_NAME_MAX} characters.`;
      break;
    case 'invalid':
      out.name = 'Remove line breaks and special characters.';
      break;
  }
  switch (errors.address) {
    case 'empty':
      out.address = 'Enter an address.';
      break;
    case 'invalid':
      out.address = 'Enter a valid MMX address.';
      break;
    case 'duplicate': {
      const existing = contactForAddress(address);
      out.address = existing ? `Already saved as ${existing.name}.` : 'This address is already saved.';
      break;
    }
    case 'own':
      out.address = 'This is one of your addresses.';
      break;
    case 'full':
      out.address = `You can save up to ${MAX_CONTACTS.toLocaleString()} contacts. Delete one to add another.`;
      break;
  }
  switch (errors.note) {
    case 'too_long':
      out.note = `Use up to ${CONTACT_NOTE_MAX} characters.`;
      break;
    case 'invalid':
      out.note = 'Remove line breaks and special characters.';
      break;
  }
  return out;
}

const plural = (n: number, one: string, many: string): string => `${n} ${n === 1 ? one : many}`;

const FULL_MESSAGE = `You can save up to ${MAX_CONTACTS.toLocaleString()} contacts.`;

/** Toast for an import result; null when the user cancelled. */
export function importSummary(result: ImportContactsResult): { message: string; kind: ToastKind } | null {
  switch (result.status) {
    case 'cancelled':
      return null;
    case 'unreadable':
      return { message: 'This file can’t be opened. Choose another file.', kind: 'error' };
    case 'invalid':
      return { message: 'This file has no contacts to import.', kind: 'error' };
    case 'imported': {
      const { added, skipped, full } = result;
      if (added === 0) {
        if (full > 0) return { message: `${FULL_MESSAGE} Delete some to import more.`, kind: 'warning' };
        return skipped > 0
          ? { message: 'No new contacts. They are all saved already.', kind: 'info' }
          : { message: 'This file has no contacts to import.', kind: 'info' };
      }
      const already = skipped > 0 ? ` ${plural(skipped, 'was', 'were')} already saved.` : '';
      const left = full > 0 ? ` ${full} more didn’t fit because your contacts are full.` : '';
      return { message: `Added ${plural(added, 'contact', 'contacts')}.${already}${left}`, kind: full > 0 ? 'warning' : 'success' };
    }
  }
}

/**
 * A contacts file dialog or import is in progress. Module-level so the
 * spinner survives leaving and reopening the Contacts screen.
 */
export const contactsTransferring = signal(false);

/** Runs one import or export at a time; a second request while one runs is ignored. */
export async function runContactsTransfer(task: () => Promise<void>): Promise<void> {
  if (contactsTransferring.value) return;
  contactsTransferring.value = true;
  try {
    await task();
  } finally {
    contactsTransferring.value = false;
  }
}

/** Asks for a contacts file, merges it and reports the result. */
export async function importContactsFromFile(): Promise<void> {
  let result: ImportContactsResult;
  try {
    result = await importContacts();
  } catch (e) {
    const message = isBackendError(e, 'invalid_argument')
      ? 'This file has no contacts to import.'
      : isBackendError(e, 'io_error')
        ? "This file can't be opened."
        : errorMessage(e);
    notify(message, 'error');
    return;
  }
  const summary = importSummary(result);
  if (summary) notify(summary.message, summary.kind);
}

/** Saves all contacts to a file and reports success. */
export async function exportContactsToFile(): Promise<void> {
  try {
    if (await exportContacts()) notify('Contacts saved to a file.', 'success');
  } catch (e) {
    notify(errorMessage(e), 'error');
  }
}

/** Deletes a contact and offers to undo it. */
export async function deleteContactWithUndo(id: string): Promise<boolean> {
  let removed: Contact | undefined;
  try {
    removed = await deleteContact(id);
  } catch (e) {
    notify(errorMessage(e), 'error');
    return false;
  }
  if (!removed) return false;
  const contact = removed;
  notify(`${contact.name} deleted.`, 'info', {
    timeout: 8000,
    action: {
      label: 'Undo',
      onClick: () => {
        restoreContact(contact).then(
          (result) => {
            if (result === 'duplicate') notify('This address is saved again already.', 'info');
            else if (result === 'full') notify(`${FULL_MESSAGE} Delete one to add another.`, 'info');
          },
          (e: unknown) => notify(errorMessage(e), 'error'),
        );
      },
    },
  });
  return true;
}
