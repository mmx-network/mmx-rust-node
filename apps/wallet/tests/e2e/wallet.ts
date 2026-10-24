/**
 * Wallet helpers for end-to-end tests, using the same WASM wallet core as the
 * web app (src/backend/web/pkg, built by `npm run build:wasm`): new recovery
 * words, wallet IDs and addresses, so tests can fund a wallet before opening it.
 */
import { readFileSync } from 'node:fs';
import * as core from '../../src/backend/web/pkg/mmx_wallet_wasm.js';

let ready = false;

function wasm(): typeof core {
  if (!ready) {
    core.initSync({ module: readFileSync(new URL('../../src/backend/web/pkg/mmx_wallet_wasm_bg.wasm', import.meta.url)) });
    ready = true;
  }
  return core;
}

/** 24 new recovery words. */
export function newMnemonic(): string {
  return wasm().generate_mnemonic();
}

/** Wallet ID ("fingerprint") of recovery words, with an optional passphrase. */
export function walletId(words: string, passphrase?: string): string {
  return wasm().fingerprint(words, passphrase ? passphrase : null);
}

/** The first `count` receive addresses of a wallet. */
export function walletAddresses(words: string, passphrase?: string, count = 10): string[] {
  const vault = wasm().Vault.open(words, passphrase ? passphrase : null, null, count);
  try {
    return vault.addresses();
  } finally {
    vault.free();
  }
}

/** Username the app saves in the password manager. */
export function credentialName(name: string, id: string): string {
  return `${name} (${id})`;
}
