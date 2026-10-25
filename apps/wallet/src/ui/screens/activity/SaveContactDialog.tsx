import { useId, useState } from 'preact/hooks';
import { AddressText, Button, Dialog, Field, Stack, Text, TextInput } from '../../components';
import type { Contact } from '../../../backend/types';
import { addContact, contactErrors, type ContactResult } from '../../../state';
import { contactErrorMessages } from '../../../state/actions/contacts';
import { CONTACT_NAME_MAX, type ContactErrors } from '../../../lib/appstate';
import { errorMessage } from '../../../lib/errors';

export interface SaveContactDialogProps {
  /** Address to save; null = closed. */
  address: string | null;
  onClose: () => void;
  /** Called after the contact was saved (before onClose). */
  onSaved?: (contact: Contact) => void;
}

function firstProblem(errors: ContactErrors, address: string): string | null {
  const m = contactErrorMessages(errors, address);
  return m.name ?? m.address ?? null;
}

/**
 * Small dialog that saves an address from a transaction as a contact. Render it
 * with `key={address}` so each address starts with empty fields.
 */
export function SaveContactDialog({ address, onClose, onSaved }: SaveContactDialogProps) {
  const [name, setName] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const formId = `save-contact${useId()}`;

  if (address === null) return null;

  const submit = async (event: Event): Promise<void> => {
    event.preventDefault();
    if (busy) return;
    const input = { name, address };
    const problem = firstProblem(contactErrors(input), address);
    if (problem) {
      setError(problem);
      return;
    }
    setBusy(true);
    let result: ContactResult;
    try {
      result = await addContact(input);
    } catch (e) {
      setBusy(false);
      setError(errorMessage(e));
      return;
    }
    setBusy(false);
    if (!result.ok) {
      setError(firstProblem(result.errors, address) ?? errorMessage(null));
      return;
    }
    onSaved?.(result.contact);
    onClose();
  };

  return (
    <Dialog
      open
      onClose={onClose}
      title="Save to contacts"
      size="sm"
      dismissible={!busy}
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" type="submit" form={formId} loading={busy}>
            Save
          </Button>
        </>
      }
    >
      <form id={formId} class="activity-save" onSubmit={submit} noValidate>
        <Stack gap={5}>
          <Field label="Name" error={error}>
            <TextInput
              value={name}
              onValue={(v) => {
                setName(v);
                if (error) setError(null);
              }}
              maxLength={CONTACT_NAME_MAX * 2}
              autocomplete="off"
              data-autofocus
            />
          </Field>
          <Stack gap={1}>
            <Text as="span" size="sm" tone="muted">
              Address
            </Text>
            <Text as="div" size="sm" class="activity-save__address">
              <AddressText address={address} mode="full" />
            </Text>
          </Stack>
        </Stack>
      </form>
    </Dialog>
  );
}
