import { useId, useRef, useState } from 'preact/hooks';
import type { Contact } from '../../../backend/types';
import { normalizeAddress } from '../../../lib/address';
import { hasErrors } from '../../../lib/appstate';
import { errorMessage } from '../../../lib/errors';
import { addContact, contactErrors, notify, updateContact } from '../../../state';
import { contactErrorMessages } from '../../../state/actions/contacts';
import { canPasteFromClipboard, readClipboardText, recipientFromText } from '../../../state/actions/send';
import { AddressText, Banner, Button, Dialog, Field, Identicon, Stack, TextInput } from '../../components';
import './contacts.css';

export interface ContactDialogProps {
  open: boolean;
  /** The contact being edited; omit to add a new one. */
  contact?: Contact;
  /** Address of a new contact (e.g. the recipient of a transfer); shown read-only. */
  address?: string;
  onClose: () => void;
  onSaved?: (contact: Contact) => void;
  /** Shows a Delete button when editing. */
  onDelete?: (contact: Contact) => void;
}

type FieldName = 'name' | 'address' | 'note';

/** Add or edit a contact: name, address and an optional note. */
export function ContactDialog(props: ContactDialogProps) {
  if (!props.open) return null;
  // Mounted per opening, so every opening starts from fresh values.
  return <ContactForm {...props} />;
}

function ContactForm({ contact, address: fixed, onClose, onSaved, onDelete }: ContactDialogProps) {
  const fixedAddress = !contact && fixed ? normalizeAddress(fixed) : null;
  const [name, setName] = useState(contact?.name ?? '');
  const [address, setAddress] = useState(contact?.address ?? fixedAddress ?? '');
  const [note, setNote] = useState(contact?.note ?? '');
  const [touched, setTouched] = useState<Partial<Record<FieldName, boolean>>>({});
  const [submitted, setSubmitted] = useState(false);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const nameRef = useRef<HTMLInputElement>(null);
  const addressRef = useRef<HTMLInputElement>(null);
  const noteRef = useRef<HTMLInputElement>(null);
  const formId = `contact-form${useId()}`;

  const input = { name, address, note };
  const errors = contactErrors(input, contact?.id);
  const messages = contactErrorMessages(errors, address);
  const visible = (field: FieldName): string | undefined => {
    if (submitted || touched[field]) return messages[field];
    // Already saved or own address: say so right away.
    if (field === 'address' && (errors.address === 'duplicate' || errors.address === 'own' || errors.address === 'full')) return messages.address;
    return undefined;
  };
  const touch = (field: FieldName) => () => setTouched((t) => (t[field] ? t : { ...t, [field]: true }));

  const submit = async (event: Event): Promise<void> => {
    event.preventDefault();
    if (busy) return;
    setSubmitted(true);
    setFailure(null);
    if (hasErrors(errors)) {
      const first = errors.name ? nameRef : errors.address ? addressRef : noteRef;
      first.current?.focus();
      return;
    }
    setBusy(true);
    try {
      const result = contact ? await updateContact(contact.id, input) : await addContact(input);
      if (!result.ok) {
        setBusy(false);
        return;
      }
      notify(contact ? 'Contact saved.' : `${result.contact.name} added to contacts.`, 'success');
      onSaved?.(result.contact);
      onClose();
    } catch (e) {
      setFailure(errorMessage(e));
      setBusy(false);
    }
  };

  const paste = async (): Promise<void> => {
    const text = await readClipboardText();
    if (text !== null) {
      setAddress(recipientFromText(text));
      setTouched((t) => ({ ...t, address: true }));
    }
    addressRef.current?.focus();
  };

  return (
    <Dialog
      open
      onClose={onClose}
      dismissible={!busy}
      title={contact ? 'Edit contact' : 'Add contact'}
      size="lg"
      class="contact-dialog"
      footer={
        <>
          {contact && onDelete ? (
            <Button variant="ghost" icon="trash" class="contact-dialog__delete" disabled={busy} onClick={() => onDelete(contact)}>
              Delete
            </Button>
          ) : null}
          <Button variant="secondary" disabled={busy} onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" type="submit" form={formId} loading={busy}>
            Save
          </Button>
        </>
      }
    >
      <form id={formId} class="contact-dialog__form" onSubmit={submit} noValidate>
        <Stack gap={5}>
          {failure ? <Banner tone="danger">{failure}</Banner> : null}
          {fixedAddress ? (
            <div class="contact-dialog__recipient">
              <Identicon value={fixedAddress} size={36} />
              <AddressText address={fixedAddress} class="contact-dialog__recipient-address" />
            </div>
          ) : null}
          {fixedAddress && messages.address ? <Banner tone="warning">{messages.address}</Banner> : null}
          <Field label="Name" error={visible('name')}>
            <TextInput
              value={name}
              onValue={setName}
              onBlur={touch('name')}
              inputRef={nameRef}
              autocomplete="off"
              enterkeyhint={fixedAddress ? 'done' : 'next'}
              data-autofocus=""
            />
          </Field>
          {fixedAddress ? null : (
            <Field label="Address" error={visible('address')}>
              <TextInput
                value={address}
                onValue={setAddress}
                onBlur={touch('address')}
                inputRef={addressRef}
                mono
                sensitive
                placeholder="mmx1…"
                leading={normalizeAddress(address) ? <Identicon value={normalizeAddress(address)!} size={20} /> : undefined}
                trailing={
                  address === '' && canPasteFromClipboard() ? (
                    <Button size="sm" variant="ghost" onClick={() => void paste()}>
                      Paste
                    </Button>
                  ) : undefined
                }
              />
            </Field>
          )}
          <Field label="Note" optional error={visible('note')}>
            <TextInput value={note} onValue={setNote} onBlur={touch('note')} inputRef={noteRef} autocomplete="off" />
          </Field>
        </Stack>
      </form>
    </Dialog>
  );
}
