import { useRef, useState } from 'preact/hooks';
import type { Contact } from '../../../backend/types';
import { MAX_CONTACTS } from '../../../lib/appstate';
import { copyAddress, searchContacts, sortedContacts, startSend } from '../../../state';
import {
  contactsTransferring,
  deleteContactWithUndo,
  exportContactsToFile,
  importContactsFromFile,
  runContactsTransfer,
} from '../../../state/actions/contacts';
import {
  AddressText,
  Button,
  Card,
  ConfirmDialog,
  CopyButton,
  EmptyState,
  IconButton,
  Identicon,
  List,
  ListItem,
  Menu,
  Page,
  PageHeader,
  SearchInput,
  Stack,
} from '../../components';
import { ContactDialog } from './ContactDialog';
import './contacts.css';

type Editor = { mode: 'add' } | { mode: 'edit'; contact: Contact } | null;

export default function ContactsScreen() {
  const all = sortedContacts.value;
  const [query, setQuery] = useState('');
  const [editor, setEditor] = useState<Editor>(null);
  const [toDelete, setToDelete] = useState<Contact | null>(null);
  const [deleting, setDeleting] = useState(false);
  const transferring = contactsTransferring.value;
  const searchRef = useRef<HTMLInputElement>(null);

  const searching = query.trim() !== '';
  const shown = searching ? searchContacts(query, MAX_CONTACTS) : all;

  const sendTo = (contact: Contact): void => {
    void startSend({ to: contact.address });
  };

  const confirmDelete = async (): Promise<void> => {
    if (!toDelete || deleting) return;
    setDeleting(true);
    await deleteContactWithUndo(toDelete.id);
    setDeleting(false);
    setToDelete(null);
    // The deleted row had focus; continue from the search field.
    setTimeout(() => searchRef.current?.focus({ preventScroll: true }), 0);
  };

  const moreMenu = (
    <Menu
      label="Contact file"
      placement="bottom-end"
      items={[
        { id: 'import', label: 'Import contacts', icon: 'upload', onSelect: () => void runContactsTransfer(importContactsFromFile) },
        {
          id: 'export',
          label: 'Export contacts',
          icon: 'download',
          disabled: all.length === 0,
          onSelect: () => void runContactsTransfer(exportContactsToFile),
        },
      ]}
      trigger={(props) => <IconButton {...props} icon="more" label="Import or export" variant="secondary" loading={transferring} />}
    />
  );

  return (
    <Page class="contacts">
      <PageHeader
        title="Contacts"
        actions={
          all.length > 0 ? (
            <>
              {moreMenu}
              <Button variant="primary" icon="user-plus" onClick={() => setEditor({ mode: 'add' })}>
                Add contact
              </Button>
            </>
          ) : undefined
        }
      />

      {all.length === 0 ? (
        <Card class="contacts__empty">
          <EmptyState
            icon="contacts"
            title="No contacts yet"
            message="Save addresses you send to often and see who sent you funds."
            action={
              <Stack direction="row" gap={2} wrap justify="center">
                <Button variant="primary" icon="user-plus" onClick={() => setEditor({ mode: 'add' })}>
                  Add contact
                </Button>
                <Button icon="upload" loading={transferring} onClick={() => void runContactsTransfer(importContactsFromFile)}>
                  Import
                </Button>
              </Stack>
            }
          />
        </Card>
      ) : (
        <Stack gap={4}>
          <SearchInput
            value={query}
            onValue={setQuery}
            label="Search contacts"
            placeholder="Search by name, address or note"
            inputRef={searchRef}
            class="contacts__search"
          />
          {shown.length === 0 ? (
            <Card variant="outline">
              <EmptyState size="sm" tone="neutral" icon="search" title="No contacts found" message="Try another name or address." />
            </Card>
          ) : (
            <List inset label="Contacts" class="contacts__list">
              {shown.map((c) => (
                <ListItem
                  key={c.id}
                  class="contacts__item"
                  leading={<Identicon value={c.address} size={36} />}
                  title={c.name}
                  subtitle={
                    <span class="contacts__item-sub">
                      <AddressText address={c.address} mode="short" class="contacts__item-address" />
                      {c.note ? <span class="contacts__item-note">{c.note}</span> : null}
                    </span>
                  }
                  onClick={() => setEditor({ mode: 'edit', contact: c })}
                  chevron={false}
                  actions={
                    <>
                      <IconButton icon="send" label={`Send to ${c.name}`} onClick={() => sendTo(c)} />
                      <CopyButton label={`Copy address of ${c.name}`} copiedLabel="Copied" onCopy={() => copyAddress(c.address)} />
                    </>
                  }
                />
              ))}
            </List>
          )}
        </Stack>
      )}

      <ContactDialog
        open={editor !== null}
        contact={editor?.mode === 'edit' ? editor.contact : undefined}
        onClose={() => setEditor(null)}
        onDelete={(c) => {
          setEditor(null);
          setToDelete(c);
        }}
      />

      <ConfirmDialog
        open={toDelete !== null}
        title={toDelete ? `Delete ${toDelete.name}?` : 'Delete contact?'}
        message="The address is removed from your contacts."
        confirmLabel="Delete"
        tone="danger"
        busy={deleting}
        onConfirm={() => void confirmDelete()}
        onCancel={() => setToDelete(null)}
      />
    </Page>
  );
}
