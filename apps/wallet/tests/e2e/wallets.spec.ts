// Several wallets, locking and settings of the web wallet.
import { capture, expectOverview, goTo, importWallet, lockWallet, openApp, openFundedWallet, unlockWithWords, walletSwitcher } from './app';
import { expect, test } from './fixtures';
import { newMnemonic, walletAddresses } from './wallet';

test('switches between wallets and keeps each one apart', async ({ page, rpc }) => {
  const first = await openFundedWallet(page, (address) => rpc.fund({ address, amount: '7000000' }), { name: 'Spending' });
  const words = newMnemonic();
  await rpc.fund({ address: walletAddresses(words)[0]!, amount: '1250000' });

  await walletSwitcher(page).click();
  await capture(page, 'wallet-menu');
  await page.getByRole('menuitem', { name: 'Add wallet' }).click();
  await importWallet(page, { name: 'Savings', words });
  await expect(page.locator('.overview-hero')).toContainText('1.25');

  await walletSwitcher(page).click();
  await page.getByRole('menuitemradio', { name: /Spending/u }).click();
  await expect(walletSwitcher(page)).toContainText('Spending');
  await expect(page.locator('.overview-hero')).toContainText('7');
  await goTo(page, 'Receive');
  await expect(page.locator('.receive-card__address')).toHaveText(first.addresses[0]!);

  // The page stays when switching.
  await walletSwitcher(page).click();
  await page.getByRole('menuitemradio', { name: /Savings/u }).click();
  await expect(page).toHaveURL(/#\/receive$/u);
  await expect(page.locator('.receive-card__address')).toHaveText(walletAddresses(words)[0]!);

  // Locking closes every wallet; the last one used is offered first.
  await lockWallet(page);
  await expect(page.locator('input[name=username]')).toHaveValue(/^Savings \(/u);
  await page.getByRole('button', { name: 'Change' }).click();
  await expect(page.getByRole('heading', { name: 'Choose a wallet' })).toBeVisible();
  await capture(page, 'unlock-choose');
  await page.getByRole('button', { name: /Spending/u }).click();
  await unlockWithWords(page, first.words);
  await expectOverview(page, 'Spending');
});

test('Lock in one tab locks the wallet in every tab', async ({ page, rpc }) => {
  const wallet = await openFundedWallet(page, (address) => rpc.fund({ address, amount: '2000000' }), { name: 'Shared' });
  // A second tab of the same site opens the wallet on its own.
  const other = await page.context().newPage();
  await openApp(other);
  await unlockWithWords(other, wallet.words);
  await expectOverview(other, 'Shared');
  await goTo(other, 'Send');

  await lockWallet(page);
  await expect(other).toHaveURL(/#\/unlock$/u);
  await expect(other.getByRole('heading', { name: 'Unlock your wallet' })).toBeVisible();
  // The other tab dropped its keys too: it needs the words again.
  await unlockWithWords(other, wallet.words);
  await expectOverview(other, 'Shared');
  // Unlocking there leaves this tab locked.
  await expect(page).toHaveURL(/#\/unlock$/u);
  await other.close();
});

test('locks after the chosen time without activity', async ({ page, rpc }) => {
  await page.clock.install();
  await openFundedWallet(page, (address) => rpc.fund({ address, amount: '1000000' }));
  await goTo(page, 'Settings');
  await page.getByLabel('Lock automatically after').selectOption({ label: '5 minutes' });
  await expect(page.getByLabel('Lock automatically after')).toHaveValue('5');

  await page.clock.fastForward('04:00');
  await expect(page).toHaveURL(/#\/settings$/u);
  await page.clock.fastForward('01:30');
  await expect(page).toHaveURL(/#\/unlock$/u);
  await expect(page.getByRole('heading', { name: 'Unlock your wallet' })).toBeVisible();

  // The setting is kept for the next visit.
  await page.reload();
  expect(await page.evaluate(() => JSON.parse(localStorage.getItem('mmx-wallet/v1') ?? '{}').settings?.autoLockMinutes)).toBe(5);
});

test('renames a wallet, changes its address count and shows its recovery phrase', async ({ page, rpc }) => {
  const passphrase = 'river stone';
  const wallet = await openFundedWallet(
    page,
    async (address, all) => {
      await rpc.fund({ address, amount: '1000000' });
      await rpc.fund({ address: all[6]!, amount: '500000' });
    },
    { name: 'Main', passphrase },
  );
  await goTo(page, 'Settings');
  await capture(page, 'settings');

  await page.getByRole('button', { name: 'Rename' }).click();
  const rename = page.getByRole('dialog', { name: 'Rename wallet' });
  await rename.getByLabel('Name').fill('Farm');
  await rename.getByRole('button', { name: /Save|Rename/u }).click();
  await expect(rename).toBeHidden();
  await expect(walletSwitcher(page)).toContainText('Farm');

  // Fewer addresses: address 7 holds funds, so the app asks first.
  const count = page.getByRole('spinbutton', { name: 'Number of addresses' });
  await count.fill('5');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  const hide = page.getByRole('dialog', { name: 'Hide addresses with funds?' });
  await expect(hide).toBeVisible();
  await hide.getByRole('button', { name: 'Cancel' }).click();
  await count.fill('12');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(count).toHaveValue('12');
  await goTo(page, 'Receive');
  await expect(page.getByRole('list', { name: 'Your addresses' }).getByRole('listitem')).toHaveCount(12);

  await goTo(page, 'Settings');
  await page.getByRole('button', { name: 'Show', exact: true }).click();
  const ask = page.getByRole('dialog', { name: 'Show recovery phrase?' });
  await ask.locator('input[name=wallet-passphrase]').fill('wrong');
  await ask.getByRole('button', { name: 'Show phrase' }).click();
  await expect(ask.getByText('Wrong passphrase. Try again.')).toBeVisible();
  await ask.locator('input[name=wallet-passphrase]').fill(passphrase);
  await ask.getByRole('button', { name: 'Show phrase' }).click();
  const shown = page.getByRole('dialog', { name: 'Recovery phrase' });
  await expect(shown).toBeVisible();
  for (const [i, word] of wallet.words.split(' ').entries()) {
    await expect(shown.locator('.word-grid__item').nth(i)).toContainText(word);
  }
  await capture(page, 'settings-phrase');
  await shown.getByRole('button', { name: 'Done' }).click();
  await expect(shown).toBeHidden();
  await expect(page.locator('.word-grid')).toHaveCount(0);

  // Reopening keeps the name and the address count.
  await lockWallet(page);
  await unlockWithWords(page, wallet.words, passphrase);
  await expectOverview(page, 'Farm');
  await goTo(page, 'Receive');
  await expect(page.getByRole('list', { name: 'Your addresses' }).getByRole('listitem')).toHaveCount(12);
});

test('forgets a wallet on this device', async ({ page, rpc }) => {
  await openFundedWallet(page, (address) => rpc.fund({ address, amount: '1000000' }), { name: 'Old' });
  await goTo(page, 'Settings');
  await page.getByRole('button', { name: 'Forget', exact: true }).click();
  const confirm = page.getByRole('dialog', { name: 'Forget Old?' });
  const button = confirm.getByRole('button', { name: 'Forget wallet' });
  await expect(button).toBeDisabled();
  await confirm.getByLabel('Type “Old” to confirm').fill('old');
  await button.click();
  await expect(page).toHaveURL(/#\/welcome$/u);
  await expect(page.getByText('Wallet forgotten on this device.')).toBeVisible();
  await page.reload();
  await expect(page).toHaveURL(/#\/welcome$/u);
  await openApp(page);
});
