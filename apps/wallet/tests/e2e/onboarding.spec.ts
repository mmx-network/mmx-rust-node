// Creating, importing and opening wallets in the web wallet.
import { capture, createWallet, expectOverview, importWallet, lockWallet, openApp, unlockWithWords, walletSwitcher } from './app';
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
  const stored = await page.evaluate(() => JSON.stringify({ ...localStorage }));
  expect(stored).toContain(id);
  for (const word of new Set(words.split(' '))) expect(stored).not.toMatch(new RegExp(`\\b${word}\\b`, 'u'));
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

test('returns to the confirm step when a new wallet was saved but never opened', async ({ page }) => {
  await openApp(page);
  await page.getByRole('button', { name: /Create a new wallet/u }).click();
  await page.getByLabel('Wallet name').fill('Later');
  await page.getByRole('button', { name: 'Continue' }).click();
  const words = await page.locator('form.create-save input[name=password]').inputValue();
  await page.getByRole('button', { name: 'Save to password manager' }).click();
  await expect(page.getByRole('heading', { name: 'Open your new wallet' })).toBeVisible();

  await page.reload();
  await expect(page.getByRole('heading', { name: 'Finish setting up' })).toBeVisible();
  await capture(page, 'unlock-unfinished');
  await page.locator('input[name=password]').fill(words);
  await page.getByRole('button', { name: 'Open wallet' }).click();
  await expectOverview(page, 'Later');
  await expect(page.getByText('Wallet created')).toBeVisible();
});

test('leaving wallet creation before saving removes the new wallet', async ({ page }) => {
  await openApp(page);
  await page.getByRole('button', { name: /Create a new wallet/u }).click();
  await page.getByRole('button', { name: 'Continue' }).click();
  await expect(page.getByRole('heading', { name: 'Save your recovery phrase' })).toBeVisible();
  await page.getByRole('button', { name: 'Cancel' }).click();
  await expect(page).toHaveURL(/#\/welcome$/u);
  await page.reload();
  await expect(page).toHaveURL(/#\/welcome$/u);
  expect(await page.evaluate(() => localStorage.getItem('mmx-wallet/v1') ?? '')).not.toMatch(/"wallets":\{"\d/u);
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
  await expect(walletSwitcher(page)).toContainText('Imported');
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
