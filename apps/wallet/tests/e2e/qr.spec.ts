import type { Page } from '@playwright/test';
import { encode } from 'uqr';
import { capture, goTo, MOBILE, openFundedWallet } from './app';
import { expect, test } from './fixtures';
import { testAddress } from './mock-rpc';

const RECIPIENT = testAddress('qr-recipient');

async function camera(page: Page, mode: 'code' | 'blank' | 'denied' | 'pending' | 'unavailable') {
  await page.addInitScript(({ mode, matrix }) => {
    const state = { requests: [] as MediaStreamConstraints[], stopped: 0, grant: undefined as (() => void) | undefined };
    Object.defineProperty(window, '__qrCamera', { value: state });
    if (mode === 'unavailable') {
      Object.defineProperty(navigator, 'mediaDevices', { value: undefined });
      return;
    }
    const stream = () => {
      const canvas = document.createElement('canvas');
      canvas.width = canvas.height = (matrix.length + 8) * 8;
      const context = canvas.getContext('2d')!;
      context.fillStyle = '#fff';
      context.fillRect(0, 0, canvas.width, canvas.height);
      if (mode === 'code') {
        context.fillStyle = '#000';
        matrix.forEach((row, y) => row.forEach((value, x) => { if (value) context.fillRect((x + 4) * 8, (y + 4) * 8, 8, 8); }));
      }
      const media = canvas.captureStream(5);
      for (const track of media.getTracks()) {
        const stop = track.stop.bind(track);
        track.stop = () => { state.stopped++; stop(); };
      }
      return media;
    };
    Object.defineProperty(navigator.mediaDevices, 'getUserMedia', {
      value: async (constraints: MediaStreamConstraints) => {
        state.requests.push(constraints);
        if (mode === 'denied') throw new DOMException('denied', 'NotAllowedError');
        if (mode === 'pending') return new Promise<MediaStream>((resolve) => { state.grant = () => resolve(stream()); });
        return stream();
      },
    });
  }, { mode, matrix: encode(RECIPIENT, { border: 0 }).data });
}

function cameraState(page: Page) {
  return page.evaluate(() => {
    const state = (window as unknown as { __qrCamera: { stopped: number; requests: MediaStreamConstraints[] } }).__qrCamera;
    return { stopped: state.stopped, requests: state.requests };
  });
}

async function photo(page: Page, text: string | null) {
  const matrix = text === null ? [] : encode(text, { border: 0 }).data;
  const data = await page.evaluate((matrix) => {
    const canvas = document.createElement('canvas');
    canvas.width = canvas.height = Math.max(64, (matrix.length + 8) * 8);
    const context = canvas.getContext('2d')!;
    context.fillStyle = '#fff';
    context.fillRect(0, 0, canvas.width, canvas.height);
    context.fillStyle = '#000';
    matrix.forEach((row, y) => row.forEach((value, x) => { if (value) context.fillRect((x + 4) * 8, (y + 4) * 8, 8, 8); }));
    return canvas.toDataURL('image/png').split(',')[1]!;
  }, matrix);
  return { name: 'qr.png', mimeType: 'image/png', buffer: Buffer.from(data, 'base64') };
}

test('scans camera frames locally on mobile and fills only the recipient', async ({ page, rpc }) => {
  await page.setViewportSize(MOBILE);
  await camera(page, 'code');
  await openFundedWallet(page, (address) => rpc.fund({ address, amount: '9000000' }));
  await goTo(page, 'Send');
  await page.getByLabel('Amount').fill('1.25');
  await page.getByLabel('Memo').fill('Keep this memo');
  await page.getByRole('button', { name: 'Scan QR code' }).click();
  await expect(page.getByLabel('To', { exact: true })).toHaveValue(RECIPIENT);
  await expect(page.getByRole('dialog', { name: 'Scan QR code' })).toBeHidden();
  await expect(page.getByLabel('Amount')).toHaveValue('1.25');
  await expect(page.getByLabel('Memo')).toHaveValue('Keep this memo');
  expect((await cameraState(page)).stopped).toBe(1);
  expect((await cameraState(page)).requests[0]).toMatchObject({ audio: false, video: { facingMode: { ideal: 'environment' } } });
  expect(await rpc.broadcasts()).toEqual([]);
  expect((await rpc.requests()).filter((request) => request.method === 'POST')).toEqual([]);
  await expect(page.getByRole('button', { name: 'Review', exact: true })).toBeVisible();
  await capture(page, 'qr-mobile-recipient');
});

test('uses a photo after camera denial, rejects other codes and keeps the draft', async ({ page, rpc }) => {
  await page.setViewportSize(MOBILE);
  await camera(page, 'denied');
  await openFundedWallet(page, (address) => rpc.fund({ address, amount: '9000000' }));
  await goTo(page, 'Send');
  await page.getByLabel('To', { exact: true }).fill(RECIPIENT);
  await page.getByRole('button', { name: 'Scan QR code' }).click();
  const dialog = page.getByRole('dialog', { name: 'Scan QR code' });
  await expect(dialog).toContainText('Camera access was denied.');
  await capture(page, 'qr-camera-denied');
  await dialog.getByLabel('QR code photo').setInputFiles(await photo(page, `https://example.com/${RECIPIENT}`));
  await expect(dialog).toContainText('does not contain a valid MMX address');
  await expect(page.getByLabel('To', { exact: true })).toHaveValue(RECIPIENT);
  await dialog.getByLabel('QR code photo').setInputFiles(await photo(page, null));
  await expect(dialog).toContainText('No QR code found.');
  await dialog.getByLabel('QR code photo').setInputFiles(await photo(page, `mmx:${testAddress('qr-other')}?amount=100&memo=ignore`));
  await expect(dialog).toBeHidden();
  await expect(page.getByLabel('To', { exact: true })).toHaveValue(testAddress('qr-other'));
  await expect(page.getByLabel('Amount')).toHaveValue('');
  await expect(page.getByLabel('Memo')).toHaveValue('');
  expect(await rpc.broadcasts()).toEqual([]);
});

for (const reason of ['close', 'hide', 'lock'] as const) {
  test(`stops the camera on ${reason}`, async ({ page, rpc }) => {
    await camera(page, 'blank');
    await openFundedWallet(page, (address) => rpc.fund({ address, amount: '1000000' }));
    await goTo(page, 'Send');
    await page.getByRole('button', { name: 'Scan QR code' }).click();
    const dialog = page.getByRole('dialog', { name: 'Scan QR code' });
    await expect(dialog).toContainText('Point the camera');
    await capture(page, 'qr-camera-preview');
    if (reason === 'close') await dialog.getByRole('button', { name: 'Close', exact: true }).click();
    else if (reason === 'hide') await page.evaluate(() => {
      Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'hidden' });
      document.dispatchEvent(new Event('visibilitychange'));
    });
    else {
      // A lock in another tab reaches the scanner through the storage event.
      const other = await page.context().newPage();
      await other.goto('./');
      await other.getByRole('button', { name: 'Unlock', exact: true }).first().waitFor();
      await other.evaluate(() => localStorage.setItem('mmx-wallet/lock', String(Date.now())));
      await other.close();
    }
    await expect(dialog).toBeHidden();
    await expect.poll(async () => (await cameraState(page)).stopped).toBe(1);
    await expect(page.getByLabel('To', { exact: true })).toHaveValue('');
  });
}

test('stops a camera permission request that resolves after closing', async ({ page, rpc }) => {
  await camera(page, 'pending');
  await openFundedWallet(page, (address) => rpc.fund({ address, amount: '1000000' }));
  await goTo(page, 'Send');
  await page.getByRole('button', { name: 'Scan QR code' }).click();
  const dialog = page.getByRole('dialog', { name: 'Scan QR code' });
  await expect(dialog).toContainText('Starting camera');
  await dialog.getByRole('button', { name: 'Close', exact: true }).click();
  await page.evaluate(() => (window as unknown as { __qrCamera: { grant: () => void } }).__qrCamera.grant());
  await expect.poll(async () => (await cameraState(page)).stopped).toBe(1);
  await expect(page.getByLabel('To', { exact: true })).toHaveValue('');
});

test('accepts a screenshot without browser camera support', async ({ page, rpc }) => {
  await camera(page, 'unavailable');
  await openFundedWallet(page, (address) => rpc.fund({ address, amount: '1000000' }));
  await goTo(page, 'Send');
  await page.getByRole('button', { name: 'Scan QR code' }).click();
  const dialog = page.getByRole('dialog', { name: 'Scan QR code' });
  await expect(dialog).toContainText('Choose a photo or screenshot');
  await dialog.getByLabel('QR code photo').setInputFiles(await photo(page, RECIPIENT));
  await expect(dialog).toBeHidden();
  await expect(page.getByLabel('To', { exact: true })).toHaveValue(RECIPIENT);
  expect((await cameraState(page)).requests).toEqual([]);
});
