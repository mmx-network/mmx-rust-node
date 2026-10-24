/**
 * End-to-end tests of the web wallet: a production web build (VITE_RPC_URL =
 * the local mock chain server) served by `vite preview` with the deployment
 * security headers, driven by the locally cached Chromium.
 *
 * Needs the WASM package: run `npm run build:wasm` first.
 */
import { existsSync } from 'node:fs';
import { defineConfig, devices } from '@playwright/test';
import { APP_PORT, APP_URL, RPC_PORT, RPC_URL } from './tests/e2e/env';

if (!existsSync(new URL('./src/backend/web/pkg/mmx_wallet_wasm_bg.wasm', import.meta.url))) {
  throw new Error('WASM package missing: run `npm run build:wasm` first');
}

const OUT_DIR = 'dist/e2e';

export default defineConfig({
  testDir: 'tests/e2e',
  testMatch: '**/*.spec.ts',
  outputDir: 'test-results',
  // One mock chain server is shared by all tests: run them one at a time.
  fullyParallel: false,
  workers: 1,
  forbidOnly: !!process.env.CI,
  retries: 0,
  timeout: 60_000,
  expect: { timeout: 10_000 },
  reporter: process.env.CI ? [['list'], ['html', { open: 'never' }]] : 'list',
  use: {
    baseURL: APP_URL,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    serviceWorkers: 'block',
    // E2E_COLOR_SCHEME=dark runs the suite in dark mode (with E2E_SCREENSHOTS for reviewing the screens).
    colorScheme: process.env.E2E_COLOR_SCHEME === 'dark' ? 'dark' : 'light',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'], browserName: 'chromium' } }],
  webServer: [
    {
      command: `node tests/e2e/mock-rpc.ts --port ${RPC_PORT}`,
      url: `${RPC_URL}/__mock/state`,
      reuseExistingServer: false,
      timeout: 30_000,
      stdout: 'ignore',
      stderr: 'pipe',
    },
    {
      command: `npx vite build --mode web --outDir ${OUT_DIR} --emptyOutDir --logLevel warn && npx vite preview --mode web --outDir ${OUT_DIR} --host 127.0.0.1 --port ${APP_PORT} --strictPort`,
      url: APP_URL,
      env: { VITE_RPC_URL: RPC_URL },
      reuseExistingServer: false,
      timeout: 180_000,
      stdout: 'ignore',
      stderr: 'pipe',
    },
  ],
});
