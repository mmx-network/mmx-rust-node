import { expect, test } from './fixtures';

test('opens on the welcome screen with the security headers and no console errors', async ({ page }) => {
  const wasm = page.waitForResponse((r) => r.url().endsWith('.wasm'));
  const response = await page.goto('./');
  const csp = response?.headers()['content-security-policy'] ?? '';
  expect(csp).toContain("frame-ancestors 'none'");
  expect(csp).toContain("script-src 'self' 'wasm-unsafe-eval'");
  expect(await page.locator('meta[http-equiv="Content-Security-Policy"]').count()).toBe(1);

  await expect(page).toHaveURL(/#\/welcome$/u);
  await expect(page.locator('#app')).not.toBeEmpty();

  // The wallet core is preloaded from the site itself under the CSP.
  const wasmResponse = await wasm;
  expect(wasmResponse.status()).toBe(200);
  expect(wasmResponse.headers()['content-type']).toContain('application/wasm');
  const compiled = await page.evaluate(async (url) => {
    const module = await WebAssembly.compileStreaming(fetch(url));
    return WebAssembly.Module.exports(module).some((e) => e.name === 'vault_open');
  }, wasmResponse.url());
  expect(compiled).toBe(true);
  await page.waitForLoadState('networkidle');
});
