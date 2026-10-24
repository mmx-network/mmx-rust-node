// Receiving and sending MMX in the web wallet.
import { capture, goTo, openFundedWallet, pollNow } from './app';
import { expect, test } from './fixtures';
import { NATIVE, testAddress } from './mock-rpc';

const MMX = 1_000_000n;

/** Atomic MMX of an amount typed as "12.5". */
function atomic(text: string): bigint {
  const [whole = '0', frac = ''] = text.split('.');
  return BigInt(whole) * MMX + BigInt(frac.padEnd(6, '0'));
}

/** "1,234.5" for atomic MMX, as the app shows it in en-US. */
function mmx(atomic: bigint | string): string {
  const value = BigInt(atomic);
  const whole = (value / MMX).toLocaleString('en-US');
  const frac = (value % MMX).toString().padStart(6, '0').replace(/0+$/u, '');
  return frac ? `${whole}.${frac}` : whole;
}

test('shows the receive address with its QR code and a label', async ({ page, rpc }) => {
  const wallet = await openFundedWallet(page, (address) => rpc.fund({ address, amount: '3000000' }));
  await goTo(page, 'Receive');
  const first = wallet.addresses[0]!;
  await expect(page.getByRole('img', { name: /QR code/u })).toBeVisible();
  await expect(page.locator('.receive-card__address')).toHaveText(first);
  await capture(page, 'receive');

  await page.getByRole('button', { name: 'Add label' }).click();
  await page.getByLabel('Label').fill('Farming');
  await page.getByRole('button', { name: 'Save label' }).click();
  await expect(page.getByLabel('Address', { exact: true })).toContainText('Farming');
  await expect(page.getByRole('list', { name: 'Your addresses' })).toContainText('Farming');

  // Another address of the list becomes the receive address.
  await page.getByRole('list', { name: 'Your addresses' }).getByRole('button', { name: /Address 3/u }).click();
  await expect(page.locator('.receive-card__address')).toHaveText(wallet.addresses[2]!);

  await page.getByRole('button', { name: 'Copy address' }).click();
  await expect(page.getByText('Copied').first()).toBeVisible();
});

test('sends MMX with a memo, once, and follows it until it is confirmed', async ({ page, rpc }) => {
  await openFundedWallet(page, (address) => rpc.fund({ address, amount: (25n * MMX).toString() }));
  const recipient = testAddress('e2e-bob');
  await goTo(page, 'Send');
  await expect(page.getByText('Available')).toContainText('25');

  await page.getByLabel('To').fill(recipient);
  await page.getByLabel('Amount').fill('2.5');
  await page.getByLabel('Memo').fill('Invoice 7');
  await capture(page, 'send-form');
  const beforeReview = (await rpc.requests()).length;
  await page.getByRole('button', { name: 'Review' }).click();

  const review = page.getByRole('region', { name: 'Transfer details' });
  await expect(review).toBeVisible();
  await expect(review.locator('.send-review__address')).toHaveText(recipient);
  await expect(review).toContainText('Invoice 7');
  await capture(page, 'send-review');

  // A higher priority prepares a new transfer with a higher fee.
  const standardFee = await review.locator('.kv__row', { hasText: 'Network fee' }).innerText();
  await review.getByRole('radio', { name: /Faster/u }).click();
  await expect(review.locator('.kv__row', { hasText: 'Network fee' })).not.toHaveText(standardFee);

  // Review signs and checks on this device: no request carried a transfer, nothing was submitted.
  const duringReview = (await rpc.requests()).slice(beforeReview);
  expect(duringReview.length).toBeGreaterThan(0);
  expect(duringReview.filter((r) => r.method !== 'GET' || r.bytes > 0 || r.path.startsWith('/transaction/'))).toEqual([]);
  expect(await rpc.broadcasts()).toHaveLength(0);

  const send = page.getByRole('button', { name: 'Send now' });
  await send.dblclick();
  await expect(page.getByRole('region', { name: 'Transfer sent' })).toBeVisible();
  await expect(page.getByText(`2.5 MMX to ${recipient.slice(0, 10)}`)).toBeVisible();
  await capture(page, 'send-result');

  const [sent] = await rpc.broadcasts();
  expect(await rpc.broadcasts()).toHaveLength(1);
  expect(sent).toMatchObject({ submissions: 1, status: 'pending' });
  expect(sent!.outputs).toEqual([{ address: recipient, contract: NATIVE, amount: '2500000', memo: 'Invoice 7' }]);
  expect(BigInt(sent!.fee)).toBeGreaterThan(0n);
  // "Send now" checked the signed transfer with the node's dry run, then posted it once.
  const posts = (await rpc.requests()).slice(beforeReview).filter((r) => r.method !== 'GET' && r.method !== 'OPTIONS');
  expect(posts.map((r) => r.path)).toEqual(['/transaction/validate', '/transaction/broadcast']);

  // Save the recipient from the result.
  await page.getByRole('button', { name: 'Save to contacts' }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByLabel('Name').fill('Bob');
  await dialog.getByRole('button', { name: /Save|Add/u }).click();
  await expect(dialog).toBeHidden();

  await page.getByRole('button', { name: 'Done' }).click();
  await expect(page).toHaveURL(/#\/overview$/u);
  const total = 2_500_000n + BigInt(sent!.fee);
  await expect(page.locator('.overview-hero__pending')).toContainText(mmx(total));

  await goTo(page, 'Activity');
  const row = page.locator('.activity-row', { hasText: 'Sent' }).first();
  await expect(row).toContainText('Pending');
  await expect(row).toContainText('Bob');
  await capture(page, 'activity-pending');

  await rpc.advance(1);
  await pollNow(page);
  await expect(row).not.toContainText('Pending', { timeout: 40_000 });
  await expect(row).toContainText('−2.5');

  await row.click();
  const detail = page.getByRole('dialog');
  await expect(detail.getByText('Confirmed')).toBeVisible();
  await expect(detail).toContainText('Invoice 7');
  await expect(detail).toContainText('Bob');
  await capture(page, 'activity-detail');
  await page.keyboard.press('Escape');
  await expect(detail).toBeHidden();
  await expect(page.locator('.overview-hero__pending')).toHaveCount(0);
  expect((await rpc.broadcasts())[0]).toMatchObject({ status: 'confirmed', submissions: 1 });
});

test('sends the maximum to a contact', async ({ page, rpc }) => {
  await openFundedWallet(page, (address) => rpc.fund({ address, amount: (10n * MMX).toString() }));
  const alice = testAddress('e2e-alice');

  await goTo(page, 'Contacts');
  await page.getByRole('button', { name: 'Add contact' }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByLabel('Name').fill('Alice');
  await dialog.getByLabel('Address').fill(alice);
  await dialog.getByRole('button', { name: /Save|Add/u }).click();
  await expect(dialog).toBeHidden();

  await goTo(page, 'Send');
  await page.getByLabel('To').fill('ali');
  await page.getByRole('option', { name: /Alice/u }).click();
  await expect(page.getByLabel('To')).toHaveValue(alice);
  await page.getByRole('button', { name: 'Max' }).click();
  await expect(page.getByLabel('Amount')).not.toHaveValue('');
  const amount = await page.getByLabel('Amount').inputValue();
  await page.getByRole('button', { name: 'Review' }).click();
  const review = page.getByRole('region', { name: 'Transfer details' });
  await expect(review).toContainText('Alice');
  await expect(review.locator('.kv__row', { hasText: 'Total' })).toHaveText(/^Total10\s*MMX$/u);
  await page.getByRole('button', { name: 'Send now' }).click();
  await expect(page.getByRole('region', { name: 'Transfer sent' })).toBeVisible();
  // A known recipient is not offered as a new contact.
  await expect(page.getByRole('button', { name: 'Save to contacts' })).toHaveCount(0);

  const [sent] = await rpc.broadcasts();
  expect(sent!.outputs).toEqual([{ address: alice, contract: NATIVE, amount: atomic(amount).toString() }]);
  expect(atomic(amount) + BigInt(sent!.fee)).toBe(10n * MMX);
});

test('a second send while the first is pending uses only the free funds', async ({ page, rpc }) => {
  const wallet = await openFundedWallet(page, (address) => rpc.fund({ address, amount: (10n * MMX).toString() }));
  await goTo(page, 'Send');
  await page.getByLabel('To').fill(testAddress('e2e-frank'));
  await page.getByLabel('Amount').fill('6');
  await page.getByRole('button', { name: 'Review' }).click();
  await page.getByRole('button', { name: 'Send now' }).click();
  await expect(page.getByRole('region', { name: 'Transfer sent' })).toBeVisible();
  const [first] = await rpc.broadcasts();
  const free = 10n * MMX - 6n * MMX - BigInt(first!.fee);
  await page.getByRole('button', { name: 'Done' }).click();

  // Not included yet: the balance still shows 10 MMX, but only the rest can be sent.
  await goTo(page, 'Send');
  await expect(page.getByText('Available')).toContainText(mmx(free));
  await page.getByLabel('To').fill(testAddress('e2e-grace'));
  await page.getByLabel('Amount').fill('5');
  await expect(page.getByText('This is more than your balance.')).toBeVisible();

  // Max leaves the first transfer's funds alone.
  await page.getByRole('button', { name: 'Max' }).click();
  await expect(page.getByLabel('Amount')).not.toHaveValue('5');
  const amount = atomic(await page.getByLabel('Amount').inputValue());
  await page.getByRole('button', { name: 'Review' }).click();
  await page.getByRole('button', { name: 'Send now' }).click();
  await expect(page.getByRole('region', { name: 'Transfer sent' })).toBeVisible();

  const broadcasts = await rpc.broadcasts();
  expect(broadcasts).toHaveLength(2);
  const second = broadcasts[1]!;
  expect(amount + BigInt(second.fee)).toBe(free);
  // The two transfers spend different funds: both are included.
  await rpc.advance(1);
  expect((await rpc.broadcasts()).map((b) => b.status)).toEqual(['confirmed', 'confirmed']);
  expect(wallet.addresses).toContain(second.sender);
});

test('reports a transfer whose result is unknown and does not send it again', async ({ page, rpc, consoleGuard }) => {
  consoleGuard.allow(/status of 502/u);
  await openFundedWallet(page, (address) => rpc.fund({ address, amount: (5n * MMX).toString() }));
  await goTo(page, 'Send');
  await page.getByLabel('To').fill(testAddress('e2e-carol'));
  await page.getByLabel('Amount').fill('1');
  await page.getByRole('button', { name: 'Review' }).click();
  await expect(page.getByRole('region', { name: 'Transfer details' })).toBeVisible();
  // Accepted by the network, but the reply is lost on the way back.
  await rpc.failNext({ path: '/transaction/broadcast', mode: 'status', status: 502, after: true });
  await page.getByRole('button', { name: 'Send now' }).click();

  await expect(page.getByRole('region', { name: 'Transfer status unknown' })).toBeVisible();
  await expect(page.getByText("We couldn't confirm the transfer. Check Activity before sending again.")).toBeVisible();
  await capture(page, 'send-uncertain');
  await page.getByRole('button', { name: 'Open Activity' }).click();
  await expect(page).toHaveURL(/#\/activity$/u);
  await expect(page.locator('.activity-row', { hasText: 'Sent' }).first()).toContainText('Pending');

  // It was accepted by the network: it confirms like any other transfer.
  await rpc.advance(1);
  await pollNow(page);
  await expect(page.locator('.activity-row', { hasText: 'Sent' }).first()).not.toContainText('Pending', { timeout: 40_000 });
  const broadcasts = await rpc.broadcasts();
  expect(broadcasts).toHaveLength(1);
  expect(broadcasts[0]!.submissions).toBe(1);
});

test('shows why a transfer cannot be sent', async ({ page, rpc, consoleGuard }) => {
  consoleGuard.allow(/status of 400/u);
  await openFundedWallet(page, (address) => rpc.fund({ address, amount: (2n * MMX).toString() }));
  await goTo(page, 'Send');
  const to = page.getByLabel('To');
  const amount = page.getByLabel('Amount');

  await to.fill('mmx1notanaddress');
  await amount.fill('1');
  await page.getByRole('button', { name: 'Review' }).click();
  await expect(page.getByText('Enter a valid MMX address.')).toBeVisible();

  await to.fill(testAddress('e2e-dave'));
  await amount.fill('1,5');
  await expect(page.getByText(/for decimals/u)).toBeVisible();
  await amount.fill('3');
  await expect(page.getByText('This is more than your balance.')).toBeVisible();
  await amount.fill('1.0000001');
  await expect(page.getByText('MMX allows up to 6 decimal places.')).toBeVisible();
  await amount.fill('1');
  await page.getByLabel('Memo').fill('x'.repeat(65));
  await expect(page.getByText('The memo is too long.')).toBeVisible();
  await page.getByLabel('Memo').fill('');
  await capture(page, 'send-errors');

  // The node refuses the transfer when it is sent (Review checks it on this device only).
  await page.getByRole('button', { name: 'Review' }).click();
  await expect(page.getByRole('region', { name: 'Transfer details' })).toBeVisible();
  await rpc.failNext({ path: '/transaction/broadcast', mode: 'status', status: 400, body: 'invalid transaction' });
  await page.getByRole('button', { name: 'Send now' }).click();
  await expect(page.getByText('The network rejected this transfer. Nothing was sent.')).toBeVisible();
  expect(await rpc.broadcasts()).toHaveLength(0);
  // A refused transfer holds no funds: the whole balance is available again.
  await expect(page.getByText('Available')).toContainText('2');

  // Not enough left for the fee.
  await amount.fill('2');
  await page.getByRole('button', { name: 'Review' }).click();
  await expect(page.getByText('Not enough funds for this amount and the network fee.')).toBeVisible();
  expect(await rpc.broadcasts()).toHaveLength(0);
});

test('shows received transfers with their sender', async ({ page, rpc }) => {
  const sender = testAddress('e2e-erin');
  const wallet = await openFundedWallet(page, (address) => rpc.fund({ address, amount: '1000000' }));
  await rpc.fund({ address: wallet.addresses[1]!, amount: '4200000', from: sender, memo: 'Thanks!' });
  await rpc.advance(1);
  await pollNow(page);
  await expect(page.getByText('Received 4.2 MMX')).toBeVisible({ timeout: 40_000 });
  await expect(page.locator('.overview-hero')).toContainText('5.2');

  await goTo(page, 'Activity');
  const row = page.locator('.activity-row', { hasText: 'Thanks!' });
  await expect(row).toContainText('+4.2');
  await row.click();
  const detail = page.getByRole('dialog');
  await detail.getByRole('button', { name: 'Save to contacts' }).click();
  const save = page.getByRole('dialog', { name: /contact/iu });
  await save.getByLabel('Name').fill('Erin');
  await save.getByRole('button', { name: /Save/u }).click();
  await expect(detail.getByText('Erin')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(row).toContainText('Erin');
});
