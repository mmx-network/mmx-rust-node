import { capture, goTo, lockWallet, MOBILE, openFundedWallet, unlockWithWords } from './app';
import { expect, test } from './fixtures';

test('refreshes unused addresses less often and reuses the public view on unlock', async ({ page, rpc }) => {
  await page.clock.install();
  const wallet = await openFundedWallet(page, (address) => rpc.fund({ address, amount: '1000000' }));
  await goTo(page, 'Receive');
  await expect(page.getByRole('list', { name: 'Your addresses' }).getByRole('listitem')).toHaveCount(3);
  await page.waitForLoadState('networkidle');
  const before = (await rpc.requests()).length;
  for (let i = 0; i < 6; i++) {
    await rpc.advance(2);
    const poll = page.waitForResponse((response) => new URL(response.url()).pathname === '/node/info');
    await page.clock.fastForward(21_000);
    await poll;
    await page.waitForLoadState('networkidle');
  }
  const paths = (await rpc.requests()).slice(before).map((request) => request.path);
  const counts = wallet.addresses.slice(0, 3).map((address) => paths.filter((path) => path.startsWith(`/address/history?id=${address}&`)).length);
  await test.info().attach('history-request-counts', {
    body: JSON.stringify({ seconds: 126, counts }), contentType: 'application/json',
  });
  expect(counts[0]).toBeGreaterThanOrEqual(4);
  expect(counts[0]).toBeLessThanOrEqual(6);
  for (const count of counts.slice(1)) {
    expect(count).toBeGreaterThanOrEqual(1);
    expect(count).toBeLessThanOrEqual(2);
  }
  expect(counts.reduce((sum, count) => sum + count, 0)).toBeLessThanOrEqual(10);

  await lockWallet(page);
  const lockedAt = (await rpc.requests()).length;
  await unlockWithWords(page, wallet.words);
  await expect(page.getByRole('dialog', { name: 'Unlock for spending' })).toBeHidden();
  await page.waitForLoadState('networkidle');
  expect((await rpc.requests()).slice(lockedAt).filter((request) => request.path.startsWith('/address'))).toEqual([]);

  // Simulate visibility deterministically; headless tabs do not reliably hide.
  const hiddenAt = (await rpc.requests()).length;
  await page.evaluate(() => {
    Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'hidden' });
    document.dispatchEvent(new Event('visibilitychange'));
  });
  await rpc.fund({ address: wallet.addresses[2]!, amount: '2000000' });
  await page.clock.fastForward(90_000);
  await page.waitForLoadState('networkidle');
  expect((await rpc.requests()).length).toBe(hiddenAt);
  await page.evaluate(() => {
    Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'visible' });
    document.dispatchEvent(new Event('visibilitychange'));
  });
  await page.clock.runFor(1000);
  await goTo(page, 'Overview');
  await expect(page.locator('.overview-hero')).toContainText('3');
});

for (const passphrase of [undefined, 'harbor lock']) {
  test(`unlock dialog spacing and lock state with ${passphrase ? 'a passphrase' : 'recovery words'}`, async ({ page, rpc }) => {
    const wallet = await openFundedWallet(page, (address) => rpc.fund({ address, amount: '1000000' }), { passphrase });
    for (const viewport of [{ width: 1280, height: 800 }, MOBILE]) {
      await page.setViewportSize(viewport);
      const lock = page.getByRole('button', { name: 'Lock', exact: true }).locator('visible=true').first();
      await expect(lock.locator('[data-icon]')).toHaveAttribute('data-icon', 'unlock');
      await lock.click();
      const unlock = page.getByRole('button', { name: 'Unlock', exact: true }).locator('visible=true').first();
      await expect(unlock.locator('[data-icon]')).toHaveAttribute('data-icon', 'lock');
      await unlock.click();
      const dialog = page.getByRole('dialog', { name: 'Unlock for spending' });
      const input = dialog.getByLabel(passphrase ? 'Passphrase' : 'Recovery phrase', { exact: true });
      await input.fill(passphrase ?? wallet.words);
      await dialog.locator('.dialog__panel').evaluate(async (panel) => {
        await Promise.all(panel.getAnimations().map((animation) => animation.finished));
      });
      const field = await input.locator('xpath=ancestor::div[@class="field"]').boundingBox();
      const button = await dialog.getByRole('button', { name: 'Unlock', exact: true }).boundingBox();
      expect(field).not.toBeNull();
      expect(button!.y - field!.y - field!.height).toBeGreaterThanOrEqual(15);
      if (!passphrase) {
        const username = await dialog.getByLabel('Wallet', { exact: true }).locator('xpath=ancestor::div[@class="field"]').boundingBox();
        expect(field!.y - username!.y - username!.height).toBeGreaterThanOrEqual(15);
      }
      await capture(page, passphrase ? 'unlock-passphrase-spacing' : 'unlock-recovery-spacing');
      await dialog.getByRole('button', { name: 'Unlock', exact: true }).click();
      await expect(dialog).toBeHidden();
      await expect(lock.locator('[data-icon]')).toHaveAttribute('data-icon', 'unlock');
    }
  });
}
