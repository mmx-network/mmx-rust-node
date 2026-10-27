// Creating, importing and opening wallets in the web wallet.
import type { Page } from '@playwright/test';
import {
  capture,
  createWallet,
  expectNoWords,
  expectActiveWallet,
  expectOverview,
  expectPassphrasePrivacy,
  goTo,
  importWallet,
  lockWallet,
  openApp,
  storedText,
  unlockWithWords,
  walletSwitcher,
} from './app';
import { expect, test } from './fixtures';
import { newMnemonic, walletAddresses, walletId } from './wallet';

type AutofillEvent = 'input' | 'change' | 'silent';

async function autofillCredential(page: Page, words: string, event: AutofillEvent, username?: string): Promise<void> {
  await page.locator('form.unlock-credentials').evaluate((form: HTMLFormElement, fill) => {
    const user = form.querySelector<HTMLInputElement>('input[name=username]')!;
    const password = form.querySelector<HTMLInputElement>('input[name=password]')!;
    if (fill.username !== undefined) user.value = fill.username;
    password.value = fill.words;
    // Some extensions fill both fields, then notify only one of them.
    if (fill.event !== 'silent') password.dispatchEvent(new Event(fill.event, { bubbles: true }));
  }, { words, event, username });
}

for (const event of ['input', 'change', 'silent'] as const) {
  test(`opens a saved wallet after ${event} autofill and revealing its phrase`, async ({ page }) => {
    await openApp(page);
    await page.getByRole('button', { name: /Open a saved wallet/u }).click();
    const words = newMnemonic();
    const username = `Autofilled (${walletId(words)})`;
    await autofillCredential(page, words, event, username);
    await page.getByRole('button', { name: 'Show', exact: true }).click();
    await expect(page.locator('input[name=username]')).toHaveValue(username);
    await expect(page.locator('input[name=password]')).toHaveValue(words);
    await page.getByRole('button', { name: 'Hide', exact: true }).click();
    await page.getByRole('button', { name: 'Open wallet', exact: true }).click();
    await expectOverview(page, 'Autofilled');
    expectNoWords(await storedText(page), words);

    // The selected-wallet form has a readonly username, but the same fill path.
    await page.reload();
    await expectOverview(page, 'Autofilled');
    await page.getByRole('button', { name: 'Unlock', exact: true }).first().click();
    await autofillCredential(page, words, event);
    await page.getByRole('button', { name: 'Show', exact: true }).click();
    await expect(page.locator('input[name=password]')).toHaveValue(words);
    await page.getByRole('dialog', { name: 'Unlock for spending' }).getByRole('button', { name: 'Unlock', exact: true }).click();
    await expectOverview(page, 'Autofilled');
    expectNoWords(await storedText(page), words);
  });

  test(`confirms a new wallet after ${event} autofill and revealing its phrase`, async ({ page }) => {
    await openApp(page);
    const { words } = await startCreate(page, 'Autofilled');
    await page.getByRole('button', { name: 'Save to password manager' }).click();
    await expect(page.getByRole('heading', { name: 'Open your new wallet' })).toBeVisible();
    await autofillCredential(page, words, event);
    await page.getByRole('button', { name: 'Show', exact: true }).click();
    await expect(page.locator('input[name=password]')).toHaveValue(words);
    await page.getByRole('button', { name: 'Open wallet', exact: true }).click();
    await expectOverview(page, 'Autofilled');
    expectNoWords(await storedText(page), words);
  });
}

test('keeps the generated recovery phrase when a password manager changes the save field', async ({ page }) => {
  await openApp(page);
  const { words } = await startCreate(page, 'Generated');
  await page.locator('form.create-save input[name=password]').evaluate((input: HTMLInputElement) => {
    input.value = 'extension-generated-password';
    input.dispatchEvent(new Event('change', { bubbles: true }));
  });
  await expect(page.locator('form.create-save input[name=password]')).toHaveValue(words);
});

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
  await expectOverview(page, 'Daily');
  await page.getByRole('button', { name: 'Unlock', exact: true }).first().click();
  await expect(page.locator('input[name=username]')).toHaveValue(`Daily (${id})`);
  await capture(page, 'unlock');
  await unlockWithWords(page, newMnemonic());
  await expect(page.getByText('These recovery words belong to a different wallet.')).toBeVisible();
  await unlockWithWords(page, words);
  await expectOverview(page, 'Daily');
  await expect(page.locator('input[name=password]')).toHaveCount(0);
});

test('creates a wallet with a passphrase', async ({ page }) => {
  // Observe what the wallet actually offers to the native password manager.
  // This is isolated test memory, not browser storage or a real saved credential.
  await page.addInitScript(() => {
    const passwords: string[] = [];
    Object.defineProperty(window, '__testStoredPasswords', { value: passwords });
    Object.defineProperty(navigator.credentials, 'store', {
      value: async (credential: Credential & { password: string }) => {
        passwords.push(credential.password);
        return credential;
      },
    });
  });
  await openApp(page);
  const passphrase = 'blue harbor 42';
  const { words, id } = await createWallet(page, { name: 'Vault', passphrase });
  expect(id).toBe(walletId(words, passphrase));
  expect(await page.evaluate(() => (window as unknown as { __testStoredPasswords: string[] }).__testStoredPasswords)).toEqual([words]);
  expect(await storedText(page)).not.toContain(passphrase);
  await lockWallet(page);

  await unlockWithWords(page, words, 'wrong');
  await expect(page.getByText('Wrong passphrase. Try again.')).toBeVisible();
  await expectPassphrasePrivacy(page);
  await expect(page.locator('input[name=wallet-passphrase]')).toBeFocused();
  await capture(page, 'unlock-wrong-passphrase');
  await page.locator('input[name=wallet-passphrase]').fill(passphrase);
  await page.getByRole('dialog', { name: 'Unlock for spending' }).getByRole('button', { name: 'Unlock', exact: true }).click();
  await expectOverview(page, 'Vault');
  await goTo(page, 'Settings');
  await page.getByRole('button', { name: 'Show', exact: true }).click();
  await expectPassphrasePrivacy(page);
  await page.locator('input[name=wallet-passphrase]').fill(passphrase);
  await page.getByRole('button', { name: 'Show phrase', exact: true }).click();
  await expect(page.getByRole('dialog', { name: 'Recovery phrase', exact: true })).toBeVisible();
  expect(await page.evaluate(() => (window as unknown as { __testStoredPasswords: string[] }).__testStoredPasswords)).toEqual([words]);
  expect(await storedText(page)).not.toContain(passphrase);
  await page.getByRole('dialog', { name: 'Recovery phrase', exact: true }).getByRole('button', { name: 'Done' }).click();
  await lockWallet(page);
  // This created wallet cached three addresses. More addresses need fresh authorization,
  // and deriving them alone must not open a spending session.
  await page.getByRole('spinbutton', { name: 'Number of addresses' }).fill('12');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  const derive = page.getByRole('dialog', { name: 'Unlock for spending' });
  await expect(derive).toBeVisible();
  await expectPassphrasePrivacy(page);
  await unlockWithWords(page, words, passphrase);
  await expect(derive).toBeHidden();
  await expect(page.getByRole('button', { name: 'Unlock', exact: true }).first()).toBeVisible();
  await goTo(page, 'Receive');
  await expect(page.getByRole('list', { name: 'Your addresses' }).getByRole('listitem')).toHaveCount(12);
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
  await page.clock.fastForward('24:01:00');
  await expect(page).toHaveURL(/#\/welcome$/u);
  await expect(page.getByText('Wallet setup was cancelled after inactivity.')).toBeVisible();
  await expectNothingStored(page, created);
});

test('inactivity while recovery words are typed into import drops them', async ({ page }) => {
  await page.clock.install();
  await openApp(page);
  await page.getByRole('button', { name: /Import with recovery phrase/u }).click();
  await page.getByRole('button', { name: 'Continue' }).click();
  const words = newMnemonic();
  await page.getByLabel('Word 1', { exact: true }).fill(words);
  await expect(page.getByLabel('Word 24', { exact: true })).toHaveValue(words.split(' ')[23]!);
  // Let the screen's effects run (the page clock is installed) before the idle wait.
  await page.clock.runFor(1000);
  // No wallet is open, yet the typed words count as one for auto-lock.
  await page.clock.fastForward('24:01:00');
  await expect(page).toHaveURL(/#\/welcome$/u);
  await expect(page.getByText('Wallet setup was cancelled after inactivity.')).toBeVisible();
  await expect(page.getByLabel('Word 1', { exact: true })).toHaveCount(0);
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
  await page.getByRole('dialog', { name: 'Unlock for spending' }).getByRole('button', { name: 'Unlock', exact: true }).click();
  await expect(page).toHaveURL(/#\/receive$/u);
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
