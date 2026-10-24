// The web backend inside the real page (CSP, CORS requests to the chain
// server, WASM loaded from the site), driven directly through its module.
import { readdirSync, readFileSync } from 'node:fs';
import { APP_URL } from './env';
import { expect, test } from './fixtures';
import { NATIVE, testAddress } from './mock-rpc';
import { walletAddresses } from './wallet';

function backendChunkUrl(): string {
  const dir = new URL('../../dist/e2e/assets/', import.meta.url);
  const file = readdirSync(dir).find((f) => f.endsWith('.js') && readFileSync(new URL(f, dir), 'utf8').includes(' as createBackend'));
  if (!file) throw new Error('web backend chunk not found in dist/e2e');
  return new URL(`assets/${file}`, APP_URL).href;
}

test('creates, opens, sends once and locks in the browser', async ({ page, rpc }) => {
  await page.goto('./');
  await expect(page).toHaveURL(/#\/welcome$/u);
  const chunk = backendChunkUrl();
  const recipient = testAddress('e2e-recipient');

  const created = await page.evaluate(async (url) => {
    const mod = (await import(url)) as { createBackend(): Promise<unknown> };
    const backend = (await mod.createBackend()) as {
      createWallet(r: object): Promise<{ wallet: { id: string; locked: boolean; verified: boolean }; mnemonic: string }>;
    };
    (globalThis as Record<string, unknown>).__testBackend = backend;
    return backend.createWallet({ name: 'Browser', addressCount: 10 });
  }, chunk);
  expect(created.wallet).toMatchObject({ locked: true, verified: false });
  const addresses = walletAddresses(created.mnemonic);
  await rpc.fund({ address: addresses[0]!, amount: '10000000' });

  const result = await page.evaluate(
    async ({ id, words, to, native }) => {
      type B = {
        unlockWallet(id: string, r: object): Promise<{ locked: boolean; verified: boolean }>;
        getAddresses(id: string): Promise<string[]>;
        prepareSend(r: object): Promise<{ txid: string; fee: string; amount: string }>;
        broadcast(id: string, txid: string): Promise<void>;
        lockAll(): Promise<void>;
      };
      const backend = (globalThis as Record<string, unknown>).__testBackend as B;
      const codeOf = (p: Promise<unknown>) => p.then(() => 'ok', (e: { code?: string }) => e.code ?? 'unknown');
      const wrong = await codeOf(backend.unlockWallet(id, { mnemonic: words.split(' ').reverse().join(' ') }));
      const opened = await backend.unlockWallet(id, { mnemonic: words });
      const list = await backend.getAddresses(id);
      const prepared = await backend.prepareSend({ walletId: id, to, contract: native, amount: '2500000', memo: 'e2e', feeLevel: 1 });
      await backend.broadcast(id, prepared.txid);
      const again = await codeOf(backend.broadcast(id, prepared.txid));
      await backend.lockAll();
      const locked = await codeOf(backend.getAddresses(id));
      return { wrong, opened, list, prepared, again, locked, stored: JSON.stringify({ ...localStorage }) };
    },
    { id: created.wallet.id, words: created.mnemonic, to: recipient, native: NATIVE },
  );

  expect(['invalid_mnemonic', 'wallet_mismatch']).toContain(result.wrong);
  expect(result.opened).toMatchObject({ locked: false, verified: true });
  expect(result.list).toEqual(addresses);
  expect(result.prepared.amount).toBe('2500000');
  expect(result.again).toBe('prepared_not_found');
  expect(result.locked).toBe('locked');
  for (const word of new Set(created.mnemonic.split(' '))) expect(result.stored).not.toMatch(new RegExp(`\\b${word}\\b`, 'u'));

  const broadcasts = await rpc.broadcasts();
  expect(broadcasts).toHaveLength(1);
  expect(broadcasts[0]).toMatchObject({ txid: result.prepared.txid, submissions: 1, fee: result.prepared.fee });
  expect(broadcasts[0]!.outputs).toEqual([{ address: recipient, contract: NATIVE, amount: '2500000', memo: 'e2e' }]);
  const requests = await rpc.requests();
  expect(requests.filter((r) => r.method === 'OPTIONS')).toEqual([]);
  expect(requests.filter((r) => r.method === 'POST').map((r) => r.path)).toEqual(['/transaction/validate', '/transaction/broadcast']);

  await rpc.advance(1);
  expect((await rpc.broadcasts())[0]!.status).toBe('confirmed');
});
