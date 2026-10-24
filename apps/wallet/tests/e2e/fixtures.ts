/**
 * Playwright fixtures for the web wallet.
 *
 *   import { test, expect } from './fixtures';
 *   test('send', async ({ page, rpc }) => {
 *     const words = newMnemonic();                       // ./wallet.ts
 *     const [first] = walletAddresses(words);
 *     await rpc.fund({ address: first!, amount: '25000000' });   // 25 MMX
 *     await page.goto('./');
 *     …
 *     await rpc.advance(1);                              // include broadcasts
 *     expect(await rpc.broadcasts()).toHaveLength(1);
 *   });
 *
 * - `rpc` controls the mock chain server (tests/e2e/mock-rpc.ts); it is reset
 *   before every test.
 * - `consoleGuard` (automatic) fails the test on console errors, uncaught page
 *   errors and Content-Security-Policy violations. Call
 *   `consoleGuard.allow(/pattern/)` for an expected error.
 * - Each test runs in a fresh browser context (empty localStorage).
 */
import { test as base, expect, type Page } from '@playwright/test';
import { RPC_URL } from './env';
import type { Broadcast, FailRule, FundRequest } from './mock-rpc';

export { expect };

export class MockRpcControl {
  constructor(readonly url: string) {}

  private async call<T>(method: 'GET' | 'POST', path: string, body?: unknown): Promise<T> {
    const res = await fetch(`${this.url}${path}`, {
      method,
      ...(body === undefined ? {} : { body: JSON.stringify(body), headers: { 'Content-Type': 'application/json' } }),
    });
    const text = await res.text();
    if (!res.ok) throw new Error(`mock ${path}: HTTP ${res.status} ${text}`);
    return JSON.parse(text) as T;
  }

  /** Clears all chain data; optional start height and confirmation delay. */
  reset(options: { height?: number; network?: string; confirmBlocks?: number } = {}): Promise<{ height: number }> {
    return this.call('POST', '/__mock/reset', options);
  }

  /** Credits an address (received transfer by default, or a reward). Amount in atomic units. */
  fund(request: FundRequest): Promise<{ txid: string; height: number }> {
    return this.call('POST', '/__mock/fund', request);
  }

  /** Registers a token so it can be funded. */
  token(contract: string, symbol: string, options: { decimals?: number } = {}): Promise<void> {
    return this.call('POST', '/__mock/token', { contract, symbol, ...options });
  }

  /** Mines blocks; broadcast transfers are included after `confirmBlocks`. */
  async advance(blocks = 1): Promise<number> {
    return (await this.call<{ height: number }>('POST', '/__mock/advance', { blocks })).height;
  }

  /** Node state: synced flag, network name, confirmation delay. */
  node(options: { synced?: boolean; network?: string; confirmBlocks?: number }): Promise<void> {
    return this.call('POST', '/__mock/node', options);
  }

  /** Makes the next matching request(s) fail (`after: true` = process it first, then fail the reply). */
  failNext(rule: FailRule): Promise<void> {
    return this.call('POST', '/__mock/fail', rule);
  }

  /** Transfers received by /transaction/broadcast, oldest first. */
  broadcasts(): Promise<Broadcast[]> {
    return this.call('GET', '/__mock/broadcasts');
  }

  /** Chain requests made by the app (method, path with query, status, request body size). */
  requests(): Promise<Array<{ method: string; path: string; status: number; bytes: number }>> {
    return this.call('GET', '/__mock/requests');
  }

  state(): Promise<{ height: number; network: string; synced: boolean; confirmBlocks: number; pending: number; broadcasts: number }> {
    return this.call('GET', '/__mock/state');
  }
}

export class ConsoleGuard {
  readonly problems: string[] = [];
  private readonly allowed: RegExp[] = [];

  allow(pattern: RegExp): void {
    this.allowed.push(pattern);
  }

  add(problem: string): void {
    if (!this.allowed.some((p) => p.test(problem))) this.problems.push(problem);
  }

  async attach(page: Page): Promise<void> {
    await page.addInitScript(() => {
      document.addEventListener('securitypolicyviolation', (e) => {
        console.error(`CSP violation: ${e.violatedDirective} ${e.blockedURI}`);
      });
    });
    page.on('console', (msg) => {
      const text = msg.text();
      if (msg.type() === 'error') this.add(`console error: ${text}`);
      else if (msg.type() === 'warning' && /content.security.policy|permissions.policy|trusted.type|webassembly|wasm/iu.test(text)) {
        this.add(`console warning: ${text}`);
      }
    });
    page.on('pageerror', (e) => this.add(`page error: ${e.message}`));
  }
}

export const test = base.extend<{ rpc: MockRpcControl; consoleGuard: ConsoleGuard }>({
  rpc: async ({}, use) => {
    const control = new MockRpcControl(RPC_URL);
    await control.reset();
    await use(control);
  },
  consoleGuard: [
    async ({ page }, use) => {
      const guard = new ConsoleGuard();
      await guard.attach(page);
      await use(guard);
      expect(guard.problems, 'console errors, page errors or CSP violations').toEqual([]);
    },
    { auto: true },
  ],
});
