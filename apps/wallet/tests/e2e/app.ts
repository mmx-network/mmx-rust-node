/**
 * UI helpers for the web wallet end-to-end tests: onboarding, unlocking and
 * navigation through the real interface (no shortcuts through app internals).
 *
 * Set E2E_SCREENSHOTS=<dir> to save a screenshot at every `capture()` call
 * (for reviewing the screens; nothing is compared).
 */
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { expect, type Locator, type Page } from '@playwright/test';
import { newMnemonic, walletAddresses, walletId } from './wallet';

export const MOBILE = { width: 390, height: 844 } as const;

/** Saves a full-page screenshot when E2E_SCREENSHOTS is set. */
export async function capture(page: Page, name: string): Promise<void> {
  const dir = process.env.E2E_SCREENSHOTS;
  if (!dir) return;
  mkdirSync(dir, { recursive: true });
  const size = page.viewportSize();
  const tag = size && size.width < 720 ? 'mobile' : 'desktop';
  await page.screenshot({ path: join(dir, `${name}-${tag}.png`), fullPage: true, animations: 'disabled' });
}

/** Opens the app and waits for the first screen. */
export async function openApp(page: Page, hash = ''): Promise<void> {
  await page.goto(`./${hash}`);
  await expect(page.locator('#app')).not.toBeEmpty();
  await expect(page.locator('.app-loading')).toHaveCount(0);
}

/** The main screen of an open wallet. */
export async function expectOverview(page: Page, walletName?: string): Promise<void> {
  await expect(page).toHaveURL(/#\/overview$/u);
  await expect(page.getByText('Total balance')).toBeVisible();
  if (walletName) await expectActiveWallet(page, walletName);
}

/** The active wallet's name, as the switcher exposes it (full button text, or the label of the compact rail button). */
export async function expectActiveWallet(page: Page, walletName: string): Promise<void> {
  const escaped = walletName.replace(/[.*+?^${}()|[\]\\]/gu, '\\$&');
  await expect(walletSwitcher(page)).toHaveAccessibleName(new RegExp(escaped, 'u'));
}

/** The wallet menu button in the shell (sidebar or mobile top bar). */
export function walletSwitcher(page: Page): Locator {
  return page.locator('button.switcher:visible').first();
}

/** Main navigation (sidebar, or the bottom tabs and "More" on phones). */
export async function goTo(page: Page, name: 'Overview' | 'Send' | 'Receive' | 'Activity' | 'Contacts' | 'Settings'): Promise<void> {
  const direct = page.getByRole('navigation', { name: 'Main' }).getByRole('button', { name, exact: true }).locator('visible=true');
  if ((await direct.count()) > 0) {
    await direct.first().click();
  } else {
    await page.getByRole('button', { name: 'More', exact: true }).click();
    await page.getByRole('menuitemradio', { name }).click();
  }
  await expect(page).toHaveURL(new RegExp(`#/${name.toLowerCase()}$`, 'u'));
}

export interface CreatedWallet {
  words: string;
  id: string;
}

/**
 * Web: creates a wallet, saves it (the save form is submitted; headless
 * Chromium has no password manager) and opens it once with the words, as a
 * password manager would fill them in.
 */
export async function createWallet(page: Page, options: { name: string; passphrase?: string }): Promise<CreatedWallet> {
  await page.getByRole('button', { name: /Create a new wallet/u }).click();
  await page.getByLabel('Wallet name').fill(options.name);
  if (options.passphrase) {
    await page.getByRole('switch', { name: 'Add a passphrase' }).click();
    await page.locator('input[name=wallet-passphrase]').fill(options.passphrase);
    await page.locator('input[name=wallet-passphrase-repeat]').fill(options.passphrase);
  }
  await page.getByRole('button', { name: 'Continue' }).click();

  await expect(page.getByRole('heading', { name: 'Save your recovery phrase' })).toBeVisible();
  const words = await page.locator('form.create-save input[name=password]').inputValue();
  expect(words.split(' ')).toHaveLength(24);
  const id = walletId(words, options.passphrase);
  await expect(page.locator('form.create-save input[name=username]')).toHaveValue(`${options.name} (${id})`);
  await capture(page, 'create-save');
  await page.getByRole('button', { name: 'Save to password manager' }).click();

  await expect(page.getByRole('heading', { name: 'Open your new wallet' })).toBeVisible();
  await expect(page.locator('input[name=username]')).toHaveValue(`${options.name} (${id})`);
  await page.locator('input[name=password]').fill(words);
  await capture(page, 'create-verify');
  await page.getByRole('button', { name: 'Open wallet' }).click();
  await expectOverview(page, options.name);
  return { words, id };
}

/** Web: imports a wallet with its words; "Not now" on the save step unless `save`. */
export async function importWallet(
  page: Page,
  options: { name: string; words: string; passphrase?: string; save?: boolean },
): Promise<void> {
  const welcome = page.getByRole('button', { name: /Import with recovery phrase/u });
  await welcome.click();
  await page.getByLabel('Wallet name').fill(options.name);
  if (options.passphrase) {
    await page.getByRole('switch', { name: 'This wallet has a passphrase' }).click();
    await page.locator('input[name=wallet-passphrase]').fill(options.passphrase);
    await page.locator('input[name=wallet-passphrase-repeat]').fill(options.passphrase);
  }
  await page.getByRole('button', { name: 'Continue' }).click();
  await page.getByLabel('Word 1', { exact: true }).fill(options.words);
  for (const [i, word] of options.words.split(' ').entries()) {
    await expect(page.getByLabel(`Word ${i + 1}`, { exact: true })).toHaveValue(word);
  }
  await capture(page, 'import-words');
  await page.getByRole('button', { name: 'Import wallet' }).click();
  await expect(page.getByRole('heading', { name: 'Save it in your password manager' })).toBeVisible({ timeout: 20_000 });
  await page.getByRole('button', { name: options.save ? 'Save to password manager' : 'Not now' }).click();
  await expectOverview(page, options.name);
}

/** Web: unlocks the selected wallet with its words (and passphrase when asked). */
export async function unlockWithWords(page: Page, words: string, passphrase?: string): Promise<void> {
  await expect(page.getByRole('heading', { name: 'Unlock your wallet' })).toBeVisible();
  await page.locator('input[name=password]').fill(words);
  await page.getByRole('button', { name: 'Unlock', exact: true }).click();
  if (passphrase !== undefined) {
    await expect(page.getByRole('heading', { name: 'Enter your passphrase' })).toBeVisible();
    await page.locator('input[name=wallet-passphrase]').fill(passphrase);
    await page.getByRole('button', { name: 'Unlock', exact: true }).click();
  }
}

/** Locks the wallet from the shell. */
export async function lockWallet(page: Page): Promise<void> {
  await page.getByRole('button', { name: 'Lock', exact: true }).first().click();
  await expect(page).toHaveURL(/#\/unlock$/u);
}

/** Everything this origin keeps in localStorage, as one string. */
export function storedText(page: Page): Promise<string> {
  return page.evaluate(() => JSON.stringify({ ...localStorage }));
}

/**
 * Fails when the stored text contains the recovery words. Checks each pair of
 * consecutive words: single words (such as "wallet") also occur in ordinary
 * stored text.
 */
export function expectNoWords(stored: string, words: string): void {
  const list = words.split(' ');
  // Words are lowercase letters only; any short separator (space, '","', …) counts.
  for (let i = 0; i + 1 < list.length; i++) expect(stored).not.toMatch(new RegExp(`\\b${list[i]}[^a-z]{1,4}${list[i + 1]}\\b`, 'u'));
}

/** Makes the app check the chain server now instead of at its next poll. */
export async function pollNow(page: Page): Promise<void> {
  await page.evaluate(() => window.dispatchEvent(new Event('online')));
}

export interface FundedWallet {
  words: string;
  id: string;
  addresses: string[];
}

/**
 * New recovery words, funded by `fund` (called with the first address and the
 * first ten) before they are imported through the interface.
 */
export async function openFundedWallet(
  page: Page,
  fund: (address: string, all: string[]) => Promise<unknown>,
  options: { name?: string; passphrase?: string } = {},
): Promise<FundedWallet> {
  const words = newMnemonic();
  const addresses = walletAddresses(words, options.passphrase, 10);
  await fund(addresses[0]!, addresses);
  await openApp(page);
  const name = options.name ?? 'Main';
  await importWallet(page, { name, words, ...(options.passphrase ? { passphrase: options.passphrase } : {}) });
  return { words, id: walletId(words, options.passphrase), addresses };
}
