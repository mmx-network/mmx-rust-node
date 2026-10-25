import { defineConfig, devices } from '@playwright/test';

const appPort = Number(process.env.EXPLORER_TEST_APP_PORT ?? 5185);
const rpcPort = Number(process.env.EXPLORER_TEST_RPC_PORT ?? 5186);
export const RPC_URL = `http://127.0.0.1:${rpcPort}`;

export default defineConfig({
  testDir: 'tests/e2e',
  outputDir: 'test-results',
  fullyParallel: false,
  workers: 1,
  retries: 0,
  forbidOnly: !!process.env.CI,
  reporter: 'list',
  use: { baseURL: `http://127.0.0.1:${appPort}`, trace: 'retain-on-failure', screenshot: 'only-on-failure' },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: [
    {
      command: 'node tests/mock-rpc.ts',
      url: `${RPC_URL}/__state`,
      env: { EXPLORER_TEST_RPC_PORT: String(rpcPort) },
      reuseExistingServer: false,
    },
    {
      command: `npm run build -- --outDir dist/e2e && npx vite preview --outDir dist/e2e --host 127.0.0.1 --port ${appPort} --strictPort`,
      url: `http://127.0.0.1:${appPort}`,
      env: { VITE_RPC_URL: RPC_URL },
      reuseExistingServer: false,
    },
  ],
});
