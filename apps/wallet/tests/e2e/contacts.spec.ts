// The address book: add, edit, delete with undo, search, export and import.
import { readFile } from 'node:fs/promises';
import { capture, goTo, openFundedWallet } from './app';
import { expect, test } from './fixtures';
import { testAddress } from './mock-rpc';

test('adds, finds, edits and deletes contacts', async ({ page, rpc }) => {
  const wallet = await openFundedWallet(page, (address) => rpc.fund({ address, amount: '1000000' }));
  await goTo(page, 'Contacts');
  await expect(page.getByText('No contacts yet')).toBeVisible();
  await capture(page, 'contacts-empty');

  const alice = testAddress('e2e-contact-alice');
  const bob = testAddress('e2e-contact-bob');
  await page.getByRole('button', { name: 'Add contact' }).click();
  let dialog = page.getByRole('dialog', { name: 'Add contact' });
  await dialog.getByLabel('Name').fill('Alice');
  await dialog.getByLabel('Address').fill(alice.toUpperCase());
  await dialog.getByLabel('Note').fill('Plot hosting');
  await dialog.getByRole('button', { name: /Save|Add/u }).click();
  await expect(dialog).toBeHidden();
  await expect(page.getByText('Alice added to contacts.')).toBeVisible();

  // Validation: own address, duplicate, invalid.
  await page.getByRole('button', { name: 'Add contact' }).click();
  dialog = page.getByRole('dialog', { name: 'Add contact' });
  await dialog.getByLabel('Address').fill(wallet.addresses[0]!);
  await expect(dialog.getByText('This is one of your addresses.')).toBeVisible();
  await dialog.getByLabel('Address').fill(alice);
  await expect(dialog.getByText('Already saved as Alice.')).toBeVisible();
  await dialog.getByLabel('Address').fill('mmx1abc');
  await dialog.getByRole('button', { name: /Save|Add/u }).click();
  await expect(dialog.getByText('Enter a name.')).toBeVisible();
  await expect(dialog.getByText('Enter a valid MMX address.')).toBeVisible();
  await capture(page, 'contacts-errors');
  await dialog.getByLabel('Name').fill('Bob');
  await dialog.getByLabel('Address').fill(bob);
  await dialog.getByRole('button', { name: /Save|Add/u }).click();
  await expect(dialog).toBeHidden();

  const list = page.getByRole('list', { name: 'Contacts' });
  await expect(list.getByRole('listitem')).toHaveCount(2);
  await page.getByLabel('Search contacts').fill('hosting');
  await expect(list.getByRole('listitem')).toHaveCount(1);
  await expect(list).toContainText('Alice');
  await page.getByLabel('Search contacts').fill('');
  await capture(page, 'contacts');

  // Edit.
  await list.getByRole('button', { name: /^Bob/u }).click();
  dialog = page.getByRole('dialog', { name: 'Edit contact' });
  await dialog.getByLabel('Name').fill('Bob Miner');
  await dialog.getByRole('button', { name: /Save/u }).click();
  await expect(dialog).toBeHidden();
  await expect(list).toContainText('Bob Miner');

  // Delete, then undo.
  await list.getByRole('button', { name: /^Bob Miner/u }).click();
  await page.getByRole('dialog', { name: 'Edit contact' }).getByRole('button', { name: 'Delete' }).click();
  await page.getByRole('dialog', { name: 'Delete Bob Miner?' }).getByRole('button', { name: 'Delete' }).click();
  await expect(list.getByRole('listitem')).toHaveCount(1);
  await page.getByRole('button', { name: 'Undo' }).click();
  await expect(list.getByRole('listitem')).toHaveCount(2);

  // Contacts are kept for the next visit.
  await page.reload();
  const stored = await page.evaluate(() => JSON.parse(localStorage.getItem('mmx-wallet/v1') ?? '{}').contacts as { name: string; address: string }[]);
  expect(stored.map((c) => c.name).sort()).toEqual(['Alice', 'Bob Miner']);
  expect(stored.find((c) => c.name === 'Alice')?.address).toBe(alice);
});

test('sends to a contact from the address book', async ({ page, rpc }) => {
  await openFundedWallet(page, (address) => rpc.fund({ address, amount: '3000000' }));
  const carol = testAddress('e2e-contact-carol');
  await goTo(page, 'Contacts');
  await page.getByRole('button', { name: 'Add contact' }).click();
  const dialog = page.getByRole('dialog', { name: 'Add contact' });
  await dialog.getByLabel('Name').fill('Carol');
  await dialog.getByLabel('Address').fill(carol);
  await dialog.getByRole('button', { name: /Save|Add/u }).click();
  await expect(dialog).toBeHidden();

  await page.getByRole('button', { name: 'Send to Carol' }).click();
  await expect(page).toHaveURL(/#\/send$/u);
  await expect(page.getByLabel('To')).toHaveValue(carol);
  await expect(page.getByText('Carol', { exact: true })).toBeVisible();
});

test('exports contacts to a file and imports them again', async ({ page, rpc }) => {
  await openFundedWallet(page, (address) => rpc.fund({ address, amount: '1000000' }));
  await goTo(page, 'Contacts');
  const dave = testAddress('e2e-contact-dave');
  await page.getByRole('button', { name: 'Add contact' }).click();
  const dialog = page.getByRole('dialog', { name: 'Add contact' });
  await dialog.getByLabel('Name').fill('Dave');
  await dialog.getByLabel('Address').fill(dave);
  await dialog.getByRole('button', { name: /Save|Add/u }).click();
  await expect(dialog).toBeHidden();

  await page.getByRole('button', { name: 'Import or export' }).click();
  const download = page.waitForEvent('download');
  await page.getByRole('menuitem', { name: 'Export contacts' }).click();
  const file = await download;
  expect(file.suggestedFilename()).toBe('mmx-contacts.json');
  const exported = JSON.parse(await readFile(await file.path(), 'utf8')) as { contacts: { name: string; address: string }[] };
  expect(exported.contacts).toEqual([{ name: 'Dave', address: dave }]);

  // Import a file with a new contact, a known one, a broken entry and hidden text direction characters.
  const erin = testAddress('e2e-contact-erin');
  const contents = JSON.stringify({
    contacts: [
      { name: 'Dave again', address: dave },
      { name: 'Erin\u{202E}', address: erin, note: 'Pool' },
      { name: 'Nobody', address: 'mmx1invalid' },
    ],
  });
  await page.getByRole('button', { name: 'Import or export' }).click();
  const chooser = page.waitForEvent('filechooser');
  await page.getByRole('menuitem', { name: 'Import contacts' }).click();
  await (await chooser).setFiles({ name: 'contacts.json', mimeType: 'application/json', buffer: Buffer.from(contents) });
  await expect(page.getByText('Added 1 contact. 1 was already saved.')).toBeVisible();
  const list = page.getByRole('list', { name: 'Contacts' });
  await expect(list.getByRole('listitem')).toHaveCount(2);
  await expect(list.getByRole('button', { name: /^Erin/u })).toBeVisible();
  await expect(list).not.toContainText('\u{202E}');
  await expect(list).toContainText('Dave');
  await expect(list).not.toContainText('Dave again');

  // Not a contacts file.
  await page.getByRole('button', { name: 'Import or export' }).click();
  const chooser2 = page.waitForEvent('filechooser');
  await page.getByRole('menuitem', { name: 'Import contacts' }).click();
  await (await chooser2).setFiles({ name: 'notes.json', mimeType: 'application/json', buffer: Buffer.from('{"hello":1}') });
  await expect(page.getByText('This file has no contacts to import.')).toBeVisible();
});
