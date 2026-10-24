// Creating, importing and opening wallets in the web wallet.
import type { Page } from '@playwright/test';
import {
  capture,
  createWallet,
  expectNoWords,
  expectActiveWallet,
  expectOverview,
  importWallet,
  lockWallet,
  openApp,
  storedText,
  unlockWithWords,
  walletSwitcher,
} from './app';
import { expect, test } from './fixtures';
import { newMnemonic, walletAddresses, walletId } from './wallet';

test('creates a wallet, saves it and confirms it by opening it once', async ({ page }) => {
  await openApp(page);
  await expect(page).toHaveURL(/#\/welcome$/u);
  await capture(page, 'welcome');

  const { words, id } = await createWallet(page, { name: 'Daily' });
  await expect(page.getByText('Wallet created')).toBeVisible();
  await expect(page.getByText('No activity yet')).toBeVisible();

  // Only the wallet's public details are stored on this device.
  const stored = await storedText(page);
  expect(stored).toContain(id);
  expectNoWords(stored, words);
  expect(await page.locator('input[name=password]').count()).toBe(0);

  // A new page load asks for the words again.
  await page.reload();
  await expect(page).toHaveURL(/#\/unlock$/u);
  await expect(page.locator('input[name=username]')).toHaveValue(`Daily (${id})`);
  await capture(page, 'unlock');
  await unlockWithWords(page, newMnemonic());
  await expect(page.getByText('These recovery words belong to a different wallet.')).toBeVisible();
  await unlockWithWords(page, words);
  await expectOverview(page, 'Daily');
  await expect(page.locator('input[name=password]')).toHaveCount(0);
});

test('creates a wallet with a passphrase', async ({ page }) => {
  await openApp(page);
  const passphrase = 'blue harbor 42';
  const { words, id } = await createWallet(page, { name: 'Vault', passphrase });
  expect(id).toBe(walletId(words, passphrase));
  await lockWallet(page);

  await unlockWithWords(page, words, 'wrong');
  await expect(page.getByText('Wrong passphrase. Try again.')).toBeVisible();
  await expect(page.locator('input[name=wallet-passphrase]')).toBeFocused();
  await capture(page, 'unlock-wrong-passphrase');
  await page.locator('input[name=wallet-passphrase]').fill(passphrase);
  await page.getByRole('button', { name: 'Unlock', exact: true }).click();
  await expectOverview(page, 'Vault');
});

/** Starts creating a wallet and stops on the password manager step; returns the new words and their wallet ID. */
async function startCreate(page: Page, name: string): Promise<{ words: string; id: string }> {
  await page.getByRole('button', { name: /Create a new wallet/u }).click();
  await page.getByLabel('Wallet name').fill(name);
  await page.getByRole('button', { name: 'Continue' }).click();
  await expect(page.getByRole('heading', { name: 'Save your recovery phrase' })).toBeVisible();
  const words = await page.locator('form.create-save input[name=password]').inputValue();
  const id = walletId(words);
  await expect(page.locator('form.create-save input[name=username]')).toHaveValue(`${name} (${id})`);
  return { words, id };
}

/** Nothing about a wallet that was never confirmed is on this device. */
async function expectNothingStored(page: Page, created: { words: string; id: string }): Promise<void> {
  const stored = await storedText(page);
  expect(stored).not.toContain(created.id);
  expectNoWords(stored, created.words);
}

test('a reload before the new wallet is opened stores nothing; the saved wallet opens later', async ({ page }) => {
  await openApp(page);
  const created = await startCreate(page, 'Later');
  await page.getByRole('button', { name: 'Save to password manager' }).click();
  await expect(page.getByRole('heading', { name: 'Open your new wallet' })).toBeVisible();
  await expectNothingStored(page, created);

  // The words lived only in the page: a reload starts over and the wallet is not on this device.
  await page.reload();
  await expect(page.getByRole('heading', { name: 'Create a wallet' })).toBeVisible();
  await expect(page.getByLabel('Wallet name')).not.toHaveValue('Later');
  await expectNothingStored(page, created);
  await page.getByRole('button', { name: 'Back' }).click();
  await expect(page).toHaveURL(/#\/welcome$/u);

  // The password manager kept it: "Open a saved wallet" adds it.
  await page.getByRole('button', { name: /Open a saved wallet/u }).click();
  await expect(page.getByRole('heading', { name: 'Open a saved wallet' })).toBeVisible();
  await page.locator('input[name=username]').fill(`Later (${created.id})`);
  await page.locator('input[name=password]').fill(created.words);
  await capture(page, 'open-saved');
  await page.getByRole('button', { name: 'Open wallet' }).click();
  await expectOverview(page, 'Later');
  expect(await storedText(page)).toContain(created.id);
});

test('abandoning wallet creation stores nothing', async ({ page }) => {
  await openApp(page);
  // Cancel on the password manager step.
  const first = await startCreate(page, 'Abandoned');
  await page.getByRole('button', { name: 'Cancel' }).click();
  await expect(page).toHaveURL(/#\/welcome$/u);
  await expectNothingStored(page, first);

  // Cancel on the confirmation step, after saving.
  const second = await startCreate(page, 'Abandoned too');
  expect(second.words).not.toBe(first.words);
  await page.getByRole('button', { name: 'Save to password manager' }).click();
  await expect(page.getByRole('heading', { name: 'Open your new wallet' })).toBeVisible();
  await page.getByRole('button', { name: 'Cancel' }).click();
  await expect(page).toHaveURL(/#\/welcome$/u);
  await expectNothingStored(page, second);

  // Wrong words on the confirmation step store nothing either.
  const third = await startCreate(page, 'Mistyped');
  await page.getByRole('button', { name: 'Save to password manager' }).click();
  await page.locator('input[name=password]').fill(newMnemonic());
  await page.getByRole('button', { name: 'Open wallet' }).click();
  await expect(page.getByRole('heading', { name: 'Open your new wallet' })).toBeVisible();
  await expect(page.locator('.field__error, [role=alert]').first()).toBeVisible();
  await expectNothingStored(page, third);

  await page.reload();
  await expect(page.getByRole('heading', { name: 'Create a wallet' })).toBeVisible();
  for (const created of [first, second, third]) await expectNothingStored(page, created);
});

test('inactivity during wallet creation drops the new words and says why', async ({ page }) => {
  await page.clock.install();
  await openApp(page);
  const created = await startCreate(page, 'Idle');
  await page.clock.fastForward('16:00');
  await expect(page).toHaveURL(/#\/welcome$/u);
  await expect(page.getByText('Wallet setup was cancelled after inactivity.')).toBeVisible();
  await expectNothingStored(page, created);
});

test('imports a wallet with a passphrase and finds its used addresses', async ({ page, rpc }) => {
  const words = newMnemonic();
  const passphrase = 'correct horse';
  const addresses = walletAddresses(words, passphrase, 16);
  await rpc.fund({ address: addresses[0]!, amount: '1500000' });
  await rpc.fund({ address: addresses[13]!, amount: '2250000', memo: 'Plot hosting' });

  await openApp(page);
  await importWallet(page, { name: 'Imported', words, passphrase });
  await expect(page.getByText('Wallet imported')).toBeVisible();
  await expect(page.locator('.overview-hero')).toContainText('3.75');
  await capture(page, 'overview-imported');

  // Address 14 holds funds: the wallet now uses 14 addresses.
  await page.goto('./#/receive');
  const list = page.getByRole('list', { name: 'Your addresses' });
  await expect(list.getByRole('listitem')).toHaveCount(14);
  await expect(list.getByText('Address 14')).toBeVisible();

  await lockWallet(page);
  await unlockWithWords(page, words, 'correct house');
  await expect(page.getByText('Wrong passphrase. Try again.')).toBeVisible();
  await page.locator('input[name=wallet-passphrase]').fill(passphrase);
  await page.getByRole('button', { name: 'Unlock', exact: true }).click();
  await expect(page).toHaveURL(/#\/overview$/u);
  await expectActiveWallet(page, 'Imported');
});

test('rejects invalid recovery words and a wallet that is already added', async ({ page }) => {
  await openApp(page);
  const { words } = await createWallet(page, { name: 'First' });

  await walletSwitcher(page).click();
  await page.getByRole('menuitem', { name: 'Add wallet' }).click();
  await expect(page.getByRole('heading', { name: 'Add a wallet' })).toBeVisible();
  await page.getByRole('button', { name: /Import with recovery phrase/u }).click();
  await page.getByRole('button', { name: 'Continue' }).click();

  const list = words.split(' ');
  const swapped = [list[1], list[0], ...list.slice(2)].join(' ');
  await page.getByLabel('Word 1', { exact: true }).fill(`${swapped.split(' ').slice(0, 23).join(' ')} zzzz`);
  await page.getByRole('button', { name: 'Import wallet' }).click();
  await expect(page.getByText('Word 24 isn’t on the word list.')).toBeVisible();
  await capture(page, 'import-word-error');

  await page.getByLabel('Word 24', { exact: true }).fill(list[23]!);
  await page.getByRole('button', { name: 'Import wallet' }).click();
  // Swapping two words breaks the checksum (or, rarely, gives another valid wallet).
  const invalid = page.getByText('These recovery words are not valid. Check each word and the order.');
  const opened = page.getByText('Save it in your password manager');
  await expect(invalid.or(opened)).toBeVisible();
  if (await opened.isVisible()) return;

  await page.getByLabel('Word 1', { exact: true }).fill(words);
  await page.getByRole('button', { name: 'Import wallet' }).click();
  await expect(page.getByText('This wallet is already added.')).toBeVisible();
});
