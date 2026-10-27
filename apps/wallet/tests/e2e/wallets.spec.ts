// Several wallets, locking and settings of the web wallet.
import { capture, expectPassphrasePrivacy, expectActiveWallet, expectOverview, goTo, importWallet, lockWallet, openApp, openFundedWallet, unlockWithWords, walletSwitcher } from './app';
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
  await expectActiveWallet(page, 'Spending');
  await expect(page.locator('.overview-hero')).toContainText('7');
  await goTo(page, 'Receive');
  await expect(page.locator('.receive-card__address')).toHaveText(first.addresses[0]!);

  // The page stays when switching.
  await walletSwitcher(page).click();
  await page.getByRole('menuitemradio', { name: /Savings/u }).click();
  await expect(page).toHaveURL(/#\/receive$/u);
  await expect(page.locator('.receive-card__address')).toHaveText(walletAddresses(words)[0]!);

  // Locking purges signing sessions and preserves the current public view.
  await lockWallet(page);
  await expect(page).toHaveURL(/#\/receive$/u);
  await expectActiveWallet(page, 'Savings');
  await walletSwitcher(page).click();
  await page.getByRole('menuitemradio', { name: /Spending/u }).click();
  await unlockWithWords(page, first.words);
  await expect(page).toHaveURL(/#\/receive$/u);
  await expectActiveWallet(page, 'Spending');
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
  await expect(other).toHaveURL(/#\/send$/u);
  await expect(other.getByRole('button', { name: 'Unlock', exact: true }).first()).toBeVisible();
  // The other tab dropped its keys too: it needs the words again.
  await unlockWithWords(other, wallet.words);
  await expect(other).toHaveURL(/#\/send$/u);
  await expectActiveWallet(other, 'Shared');
  // Unlocking there leaves this tab locked.
  await expect(page.getByRole('button', { name: 'Unlock', exact: true }).first()).toBeVisible();
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
  await expect(page.getByRole('button', { name: 'Unlock', exact: true }).first()).toBeVisible();
  await expect(page).toHaveURL(/#\/settings$/u);

  // The setting is kept for the next visit.
  await page.reload();
  expect(await page.evaluate(() => JSON.parse(localStorage.getItem('mmx-wallet/v1') ?? '{}').settings?.autoLockMinutes)).toBe(5);
});

test('keeps address-count controls in place while editing, cancelling and saving', async ({ page, rpc }) => {
  await openFundedWallet(page, (address) => rpc.fund({ address, amount: '1000000' }));
  await goTo(page, 'Settings');
  await page.evaluate(() => document.fonts.ready);
  const row = page.getByRole('group', { name: 'Addresses', exact: true });
  const count = row.getByRole('spinbutton', { name: 'Number of addresses' });
  const increase = row.getByRole('button', { name: 'Increase' });
  const cancel = row.getByRole('button', { name: 'Cancel', exact: true });
  const save = row.getByRole('button', { name: 'Save', exact: true });
  const position = () => row.evaluate((element) => {
    const stepper = element.querySelector('.stepper')!.getBoundingClientRect();
    return { x: stepper.x, y: stepper.y, width: stepper.width, height: element.getBoundingClientRect().height };
  });
  for (const width of [1440, 768, 390, 320]) {
    await page.setViewportSize({ width, height: 900 });
    await row.scrollIntoViewIfNeeded();
    await expect(cancel).toBeDisabled();
    await expect(save).toBeDisabled();
    const before = await position();
    const plus = (await increase.boundingBox())!;
    await increase.click();
    await expect(count).toHaveValue('4');
    await expect(cancel).toBeEnabled();
    await expect(save).toBeEnabled();
    expect(await position()).toEqual(before);
    // A second click at the same coordinates still hits Increase.
    await page.mouse.click(plus.x + plus.width / 2, plus.y + plus.height / 2);
    await expect(count).toHaveValue('5');
    await capture(page, `settings-count-edit-${width}`);
    await cancel.click();
    await expect(count).toHaveValue('3');
    await expect(save).toBeDisabled();
    expect(await position()).toEqual(before);
    await capture(page, `settings-count-idle-${width}`);
  }
  await increase.click();
  const beforeSave = await position();
  await save.click();
  await expect(save).toBeDisabled();
  await expect(cancel).toBeDisabled();
  await expect(count).toHaveValue('4');
  expect(await position()).toEqual(beforeSave);
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
  await expectActiveWallet(page, 'Farm');

  // Fewer addresses: address 7 holds funds, so the app asks first.
  const count = page.getByRole('spinbutton', { name: 'Number of addresses' });
  await count.fill('5');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  const hide = page.getByRole('dialog', { name: 'Hide addresses with funds?' });
  await expect(hide).toBeVisible();
  await hide.getByRole('button', { name: 'Cancel' }).click();
  await count.fill('12');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  // Import already cached all 100 scanned addresses: these need no derivation.
  await expect(page.getByRole('dialog', { name: 'Unlock for spending' })).toBeHidden();
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
  await expect(page).toHaveURL(/#\/settings$/u);
  await expectActiveWallet(page, 'Farm');
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

test('keeps a passphrase signing session for multiple transfers, then locks without hiding funds', async ({ page, rpc }) => {
  await page.clock.install();
  const passphrase = 'private session phrase';
  const wallet = await openFundedWallet(page, (address) => rpc.fund({ address, amount: '10000000' }), { passphrase });
  await page.reload();
  await expectOverview(page);
  await expect(page.locator('.overview-hero')).toContainText('10');
  for (const amount of ['1', '2']) {
    await goTo(page, 'Send');
    await page.getByLabel('To').fill(wallet.addresses[1]!);
    await page.getByLabel('Amount').fill(amount);
    await page.getByRole('button', { name: 'Review', exact: true }).click();
    if (amount === '1') {
      const authorize = page.getByRole('dialog', { name: 'Unlock for spending' });
      await expect(authorize).toBeVisible();
      await expectPassphrasePrivacy(page);
      await unlockWithWords(page, wallet.words, passphrase);
    }
    await expect(page.getByRole('region', { name: 'Transfer details' })).toBeVisible();
    await expect(page.getByRole('dialog', { name: 'Unlock for spending' })).toBeHidden();
    await page.getByRole('button', { name: 'Send now' }).click();
    await expect(page.getByRole('region', { name: 'Transfer sent' })).toBeVisible();
    await page.getByRole('button', { name: 'Done', exact: true }).click();
    await expectOverview(page);
    await page.clock.runFor(1000);
    await page.clock.fastForward('04:00');
    await expect(page.getByRole('button', { name: 'Lock', exact: true }).first()).toBeVisible();
  }
  expect(await rpc.broadcasts()).toHaveLength(2);
  await page.clock.fastForward('01:30');
  await expect(page.getByRole('button', { name: 'Unlock', exact: true }).first()).toBeVisible();
  await expect(page.getByText('Total balance')).toBeVisible();
  await goTo(page, 'Receive');
  await expect(page.locator('.receive-card__address')).toHaveText(wallet.addresses[0]!);
});

test('locking closes a displayed recovery phrase and preserves the public wallet screen', async ({ page, rpc }) => {
  const wallet = await openFundedWallet(page, (address) => rpc.fund({ address, amount: '1000000' }));
  await goTo(page, 'Settings');
  await page.getByRole('button', { name: 'Show', exact: true }).click();
  await page.getByRole('button', { name: 'Show phrase', exact: true }).click();
  const phrase = page.getByRole('dialog', { name: 'Recovery phrase', exact: true });
  await expect(phrase).toBeVisible();
  // A second tab's explicit lock must clear this dialog even though Settings stays mounted.
  const other = await page.context().newPage();
  await openApp(other);
  await unlockWithWords(other, wallet.words);
  await lockWallet(other);
  await expect(phrase).toBeHidden();
  await expect(page.locator('.word-grid')).toHaveCount(0);
  await expect(page).toHaveURL(/#\/settings$/u);
  await other.close();
  // A regular web wallet needs its recovery credential again to reveal the words.
  await page.getByRole('button', { name: 'Show', exact: true }).click();
  await unlockWithWords(page, wallet.words);
  await expect(phrase).toBeVisible();
  await phrase.getByRole('button', { name: 'Done', exact: true }).click();
});
