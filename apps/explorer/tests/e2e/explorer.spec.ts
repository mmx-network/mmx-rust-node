import { test, expect } from '@playwright/test';
import { ADDRESS, FARMER, GENESIS, hash } from '../fixtures';

const RPC = `http://127.0.0.1:${process.env.EXPLORER_TEST_RPC_PORT ?? 5186}`;
test.beforeEach(async ({ request }) => {
  await request.get(`${RPC}/__state?reset=1`);
});

test('pages blocks with stable heights and visits genesis', async ({ page, request }) => {
  await page.goto('/');
  await expect(page.locator('tbody tr')).toHaveCount(20);
  await expect(page.locator('tbody tr').first()).toContainText('#65');
  await page.getByRole('link', { name: 'Next', exact: true }).click();
  await expect(page).toHaveURL(/page=2&at=65/);
  await expect(page.locator('tbody tr').first()).toContainText('#45');
  await request.get(`${RPC}/__state?tip=70`);
  await page.getByRole('button', { name: 'Refresh data' }).click();
  await expect(page.locator('tbody tr').first()).toContainText('#45');
  await page.getByRole('link', { name: 'Next', exact: true }).click();
  await expect(page.locator('tbody tr').first()).toContainText('#25');
  await page.getByRole('link', { name: 'Next', exact: true }).click();
  await expect(page.locator('tbody tr')).toHaveCount(6);
  await expect(page.locator('tbody tr').last()).toContainText('#0');
  await expect(page.getByRole('button', { name: 'Next', exact: true })).toBeDisabled();
  await page.getByRole('link', { name: '#0', exact: true }).click();
  await expect(page.getByLabel('block hash', { exact: true })).toHaveText(GENESIS);
  await expect(page.getByRole('link', { name: 'Previous block', exact: true })).toHaveCount(0);
});

test('pages transactions without another list request and opens full transaction details', async ({
  page,
  request,
}) => {
  await page.goto('/#/explore/transactions');
  await expect(page.locator('tbody tr')).toHaveCount(20);
  await page.getByRole('link', { name: 'Next', exact: true }).click();
  await expect(page.getByRole('navigation', { name: 'transactions pagination' })).toContainText('Page 2 of 3');
  const state = (await (await request.get(`${RPC}/__state`)).json()) as { requests: string[] };
  expect(state.requests.filter((path) => path.startsWith('/transactions?'))).toHaveLength(1);
  await page.locator('tbody tr').first().getByRole('link').first().click();
  await expect(page.getByRole('heading', { name: 'Transaction', exact: true })).toBeVisible();
  await expect(page.getByLabel('transaction ID', { exact: true })).toHaveText(hash(1020));
  const outputs = page.getByRole('region', { name: 'Outputs', exact: true });
  await expect(outputs.locator('tbody tr')).toHaveCount(20);
  await outputs.getByRole('button', { name: 'Next', exact: true }).click();
  await expect(outputs.locator('tbody tr')).toHaveCount(3);
  await page.getByText('Operation 1 · mmx.operation.Execute', { exact: true }).click();
  await expect(page.getByText('must stay hidden', { exact: true })).toHaveCount(0);
});

test('search validates inputs and distinguishes block hashes from transactions', async ({ page }) => {
  await page.goto('/');
  const search = page.getByRole('searchbox', { name: 'Search the blockchain' });
  await search.fill('12junk');
  await search.press('Enter');
  await expect(page.getByRole('alert')).toContainText('Enter a valid block height');
  await search.fill(hash(5065));
  await search.press('Enter');
  await expect(page).toHaveURL(/block\/hash\//);
  await expect(page.getByRole('heading', { name: 'Block #65', exact: true })).toBeVisible();
  await search.fill(hash(1000));
  await search.press('Enter');
  await expect(page).toHaveURL(/transaction\//);
  await expect(page.getByLabel('transaction ID', { exact: true })).toHaveText(hash(1000));
});

test('legacy address links show balances, contract data and paged activity', async ({ page }) => {
  await page.goto(`/#/explore/address/${ADDRESS}`);
  await expect(page.getByLabel('address', { exact: true })).toHaveText(ADDRESS);
  await expect(page.getByRole('region', { name: 'Token balances', exact: true })).toContainText('12.345678');
  const history = page.getByRole('region', { name: 'Activity', exact: true });
  await expect(history.locator('tbody tr')).toHaveCount(20);
  await history.getByRole('link', { name: 'Next', exact: true }).click();
  await expect(history).toContainText('Page 2 of 3');
  await expect(history.locator('tbody tr').first()).toContainText('Entry 21');
  await page.getByText('mmx.contract.PubKey', { exact: true }).click();
  await expect(page.getByRole('region', { name: 'Contract', exact: true })).toContainText(FARMER);
});

test('farmer rankings and farmer block history are paged', async ({ page }) => {
  await page.goto('/#/explore/farmers');
  await expect(page.locator('tbody tr')).toHaveCount(20);
  await page.getByRole('link', { name: 'Next', exact: true }).click();
  await expect(page.locator('tbody tr').first().locator('td').first()).toHaveText('21');
  await page.goto(`/#/explore/farmer/${FARMER}`);
  await expect(page.getByLabel('farmer public key', { exact: true })).toHaveText(FARMER);
  await expect(page.getByText('123,456,789,123.456789').first()).toBeVisible();
  const blocks = page.getByRole('region', { name: 'Farmed blocks', exact: true });
  await blocks.getByRole('link', { name: 'Next', exact: true }).click();
  await expect(blocks).toContainText('Page 2 of 3');
});

test('shows a server failure and recovers on retry', async ({ page, request }) => {
  await request.get(`${RPC}/__state?fail=/headers`);
  await page.goto('/');
  await expect(page.getByRole('alert')).toContainText('The chain server is syncing');
  await request.get(`${RPC}/__state?fail=`);
  await page.getByRole('button', { name: 'Try again' }).click();
  await expect(page.locator('tbody tr')).toHaveCount(20);
  await expect(page.getByRole('alert')).toHaveCount(0);
});

test('mobile dark mode keeps tables inside the viewport and preserves appearance', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto('/');
  await page.getByRole('button', { name: 'Appearance', exact: true }).click();
  await page.getByRole('menuitemradio', { name: 'Dark mode' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-scheme', 'dark');
  await expect(page.getByRole('navigation', { name: 'Mobile navigation' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Search', exact: true })).toBeVisible();
  await expect(page.locator('tbody tr')).toHaveCount(20);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('data-scheme', 'dark');
  await page.getByRole('navigation', { name: 'Mobile navigation' }).getByRole('link', { name: 'Transactions' }).click();
  await expect(page.getByRole('heading', { name: 'Transactions', exact: true })).toBeVisible();
});
