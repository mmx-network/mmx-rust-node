// The web wallet on a phone: bottom tabs, "More" menu, sheets and the top bar lock.
import { capture, createWallet, expectOverview, goTo, MOBILE, openApp, openFundedWallet, pollNow, unlockWithWords } from './app';
import { expect, test } from './fixtures';
import { testAddress } from './mock-rpc';

test.use({ viewport: MOBILE, isMobile: true, hasTouch: true });

async function expectNoSideScroll(page: import('@playwright/test').Page): Promise<void> {
  const widths = await page.evaluate(() => ({ scroll: document.documentElement.scrollWidth, client: document.documentElement.clientWidth }));
  expect(widths.scroll).toBeLessThanOrEqual(widths.client);
}

test('creates a wallet and moves through every screen', async ({ page }) => {
  await openApp(page);
  await capture(page, 'welcome');
  await expectNoSideScroll(page);
  const { words } = await createWallet(page, { name: 'Phone' });
  await expect(page.getByRole('navigation', { name: 'Main' }).locator('visible=true')).toHaveCount(1);
  await capture(page, 'overview');

  for (const name of ['Send', 'Receive', 'Activity', 'Contacts', 'Settings', 'Overview'] as const) {
    await goTo(page, name);
    await expectNoSideScroll(page);
    await capture(page, `screen-${name.toLowerCase()}`);
  }

  // Narrow phones: the Overview action labels are not cut off.
  for (const width of [360, 320]) {
    await page.setViewportSize({ width, height: MOBILE.height });
    await expectNoSideScroll(page);
    const labels = page.locator('.overview-hero__actions .btn__label');
    expect(await labels.count()).toBeGreaterThan(0);
    for (const fit of await labels.evaluateAll((els) => els.map((el) => el.scrollWidth <= el.clientWidth))) expect(fit).toBe(true);
  }
  await page.setViewportSize(MOBILE);

  await page.getByRole('button', { name: 'Lock', exact: true }).click();
  await expect(page).toHaveURL(/#\/unlock$/u);
  await expectNoSideScroll(page);
  await unlockWithWords(page, words);
  await expectOverview(page, 'Phone');
});

test('sends and opens the details as a sheet', async ({ page, rpc }) => {
  await openFundedWallet(page, (address) => rpc.fund({ address, amount: '9000000', from: testAddress('e2e-mobile-sender') }));
  await goTo(page, 'Send');
  await page.getByLabel('To').fill(testAddress('e2e-mobile-recipient'));
  await page.getByLabel('Amount').fill('1.25');
  await page.getByRole('button', { name: 'Review' }).click();
  const review = page.getByRole('region', { name: 'Transfer details' });
  await expect(review).toBeVisible();
  await expectNoSideScroll(page);
  await capture(page, 'send-review');
  await review.getByRole('radio', { name: '2×' }).click();
  await page.getByRole('button', { name: 'Send now' }).click();
  await expect(page.getByRole('region', { name: 'Transfer sent' })).toBeVisible();
  await capture(page, 'send-result');
  await page.getByRole('button', { name: 'Done' }).click();

  await goTo(page, 'Activity');
  await rpc.advance(1);
  await pollNow(page);
  const received = page.locator('.activity-row', { hasText: 'Received' });
  await received.click();
  const sheet = page.getByRole('dialog');
  await expect(sheet).toContainText('Confirmed');
  await expectNoSideScroll(page);
  await capture(page, 'activity-sheet');
  await sheet.getByRole('button', { name: 'Close' }).click();
  await expect(sheet).toBeHidden();
});
