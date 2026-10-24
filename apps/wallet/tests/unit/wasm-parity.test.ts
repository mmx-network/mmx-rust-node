// WASM build of the wallet core under Node: identical keys, fingerprints and
// byte-identical signed transfers compared with native Rust fixtures.
// Requires `bash scripts/build-wasm.sh --test` (writes tests/.wasm-test/pkg).
import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { beforeAll, describe, expect, it } from 'vitest';

interface Vault {
  free(): void;
  fingerprint(): string;
  with_passphrase(): boolean;
  addresses(): string[];
  set_count(count: number): void;
  mnemonic(): string;
  max_amount(
    balancesJson: string,
    paramsJson: string,
    currency: string,
    memo: string | null | undefined,
    feeLevel: number,
  ): string;
  sign_transfer(
    balancesJson: string,
    paramsJson: string,
    height: number,
    to: string,
    currency: string,
    amountAtomic: string,
    memo: string | null | undefined,
    feeLevel: number,
    expireDelta: number,
  ): string;
  sign_transfer_with_nonce(
    balancesJson: string,
    paramsJson: string,
    height: number,
    to: string,
    currency: string,
    amountAtomic: string,
    memo: string | null | undefined,
    feeLevel: number,
    expireDelta: number,
    nonce: bigint,
  ): string;
}

interface WasmApi {
  initSync(input: { module: BufferSource }): unknown;
  Vault: {
    open(
      words: string,
      passphrase: string | null | undefined,
      expectedFingerprint: string | null | undefined,
      count: number,
    ): Vault;
  };
  generate_mnemonic(): string;
  normalize_mnemonic(words: string): string;
  fingerprint(words: string, passphrase?: string | null): string;
  validate_address(address: string): boolean;
  parse_amount(text: string, decimals: number): string;
  format_amount(atomic: string, decimals: number): string;
  static_fee(
    paramsJson: string,
    inputs: number,
    owners: number,
    memo: string | null | undefined,
    feeLevel: number,
  ): string;
  verify_transaction(txJson: string, paramsJson: string): void;
  derive_addresses(
    words: string,
    passphrase: string | null | undefined,
    account: number,
    count: number,
  ): string[];
}

interface Reference {
  mnemonic: string;
  fingerprint: string;
  fingerprint_empty_pass: string;
  fingerprint_pass: string;
  addresses: string[];
}

interface Parity {
  mnemonic: string;
  passphrase: string;
  fingerprint: string;
  fingerprint_empty_pass: string;
  fingerprint_pass: string;
  count: number;
  addresses: string[];
  addresses_no_passphrase: string[];
  params: Record<string, unknown>;
  balances: { address: string; contract: string; amount: string }[];
  token: string;
  recipient: string;
  max: { currency: string; memo: string | null; fee_level: number; amount: string }[];
  transfers: {
    height: number;
    to: string;
    currency: string;
    amount: string;
    memo: string | null;
    fee_level: number;
    expire_delta: number;
    nonce: string;
    tx: string;
  }[];
  static_fee: {
    inputs: number;
    owners: number;
    memo: string | null;
    fee_level: number;
    fee: string;
  }[];
}

const pkgDir = fileURLToPath(new URL('../.wasm-test/pkg/', import.meta.url));
const repoRoot = fileURLToPath(new URL('../../../../', import.meta.url));
const NATIVE = 'mmx1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqdgytev';

const readJson = <T>(path: string): T => JSON.parse(readFileSync(repoRoot + path, 'utf8')) as T;
const reference = readJson<Reference>('crates/mmx-wallet/tests/fixtures/reference.json');
const parity = readJson<Parity>('crates/mmx-wallet-wasm/tests/fixtures/parity.json');
const balancesJson = JSON.stringify(parity.balances);
const paramsJson = JSON.stringify(parity.params);

let wasm: WasmApi;

beforeAll(async () => {
  const glue = `${pkgDir}mmx_wallet_wasm.js`;
  if (!existsSync(glue)) {
    throw new Error('WASM test build missing: run `bash scripts/build-wasm.sh --test` first');
  }
  wasm = (await import(/* @vite-ignore */ glue)) as WasmApi;
  wasm.initSync({ module: readFileSync(`${pkgDir}mmx_wallet_wasm_bg.wasm`) });
});

/** The stable error code thrown by `f`, or 'ok'. */
function errorCode(f: () => unknown): string {
  try {
    f();
  } catch (e) {
    return e instanceof Error ? e.message : String(e);
  }
  return 'ok';
}

function withVault<T>(vault: Vault, f: (v: Vault) => T): T {
  try {
    return f(vault);
  } finally {
    vault.free();
  }
}

const openParity = (): Vault =>
  wasm.Vault.open(parity.mnemonic, parity.passphrase, parity.fingerprint_pass, parity.count);

describe('reference derivation', () => {
  it('matches the C++ fingerprints, phrase and addresses', () => {
    const m = reference.mnemonic;
    expect(wasm.fingerprint(m)).toBe(reference.fingerprint);
    expect(wasm.fingerprint(m, null)).toBe(reference.fingerprint);
    expect(wasm.fingerprint(m, '')).toBe(reference.fingerprint_empty_pass);
    expect(wasm.fingerprint(m, parity.passphrase)).toBe(reference.fingerprint_pass);
    expect(wasm.derive_addresses(m, parity.passphrase, 7, 3)).toEqual(reference.addresses);
    expect(parity.mnemonic).toBe(m);
  });

  it('normalizes pasted phrases', () => {
    const messy = `  ${reference.mnemonic.toUpperCase().split(' ').join(' \t \n')}  `;
    expect(wasm.normalize_mnemonic(messy)).toBe(reference.mnemonic);
    expect(wasm.fingerprint(messy)).toBe(reference.fingerprint);
  });

  it('matches native account addresses', () => {
    withVault(openParity(), (v) => {
      expect(v.addresses()).toEqual(parity.addresses);
      expect(v.fingerprint()).toBe(parity.fingerprint_pass);
      expect(v.with_passphrase()).toBe(true);
      expect(v.mnemonic()).toBe(parity.mnemonic);
    });
  });
});

describe('passphrase detection', () => {
  const m = parity.mnemonic;

  it('never asks for a passphrase the wallet does not have', () => {
    for (const p of [undefined, null, '', 'typed anyway']) {
      withVault(wasm.Vault.open(m, p, parity.fingerprint, 3), (v) => {
        expect(v.with_passphrase()).toBe(false);
        expect(v.fingerprint()).toBe(parity.fingerprint);
        expect(v.addresses()).toEqual(parity.addresses_no_passphrase);
      });
    }
  });

  it('opens an empty-passphrase wallet silently', () => {
    withVault(wasm.Vault.open(m, undefined, parity.fingerprint_empty_pass, 3), (v) => {
      expect(v.with_passphrase()).toBe(false);
      expect(v.fingerprint()).toBe(parity.fingerprint_empty_pass);
      expect(v.addresses()).toEqual(parity.addresses_no_passphrase);
    });
  });

  it('requires and checks the passphrase of a locked wallet', () => {
    const fp = parity.fingerprint_pass;
    expect(errorCode(() => wasm.Vault.open(m, undefined, fp, 3))).toBe('passphrase_required');
    expect(errorCode(() => wasm.Vault.open(m, '', fp, 3))).toBe('passphrase_required');
    for (const wrong of ['wrong', 'test 世界', `${parity.passphrase} `]) {
      expect(errorCode(() => wasm.Vault.open(m, wrong, fp, 3))).toBe('invalid_passphrase');
    }
    withVault(wasm.Vault.open(m, parity.passphrase, fp, 3), (v) => {
      expect(v.addresses()).toEqual(parity.addresses);
    });
  });

  it('derives a new fingerprint on first import', () => {
    withVault(wasm.Vault.open(m, undefined, undefined, 1), (v) => {
      expect(v.fingerprint()).toBe(parity.fingerprint);
      expect(v.with_passphrase()).toBe(false);
    });
    withVault(wasm.Vault.open(m, '', undefined, 1), (v) => {
      expect(v.fingerprint()).toBe(parity.fingerprint);
    });
    withVault(wasm.Vault.open(m, parity.passphrase, undefined, 1), (v) => {
      expect(v.fingerprint()).toBe(parity.fingerprint_pass);
      expect(v.with_passphrase()).toBe(true);
    });
  });

  it('rejects invalid phrases, counts and fingerprints', () => {
    const words = m.split(' ');
    const invalid = [
      '',
      words.slice(0, 23).join(' '),
      `${m} abandon`,
      [...words.slice(0, 23), 'notaword'].join(' '),
    ];
    for (const bad of invalid) {
      expect(errorCode(() => wasm.Vault.open(bad, undefined, undefined, 1))).toBe('invalid_mnemonic');
      expect(errorCode(() => wasm.normalize_mnemonic(bad))).toBe('invalid_mnemonic');
    }
    for (const count of [0, 101, -1]) {
      expect(errorCode(() => wasm.Vault.open(m, undefined, undefined, count))).toBe('invalid_argument');
    }
    for (const fp of ['', 'abc', '012', '4294967296']) {
      expect(errorCode(() => wasm.Vault.open(m, undefined, fp, 1))).toBe('invalid_argument');
    }
  });
});

describe('vault', () => {
  it('re-derives addresses when the count changes', () => {
    withVault(openParity(), (v) => {
      v.set_count(10);
      const ten = v.addresses();
      expect(ten).toHaveLength(10);
      expect(ten.slice(0, 3)).toEqual(parity.addresses);
      v.set_count(1);
      expect(v.addresses()).toEqual(parity.addresses.slice(0, 1));
      expect(errorCode(() => v.set_count(0))).toBe('invalid_argument');
      expect(errorCode(() => v.set_count(101))).toBe('invalid_argument');
      expect(v.addresses()).toHaveLength(1);
      v.set_count(100);
      expect(v.addresses()).toHaveLength(100);
    });
  });

  it('cannot be used after free()', () => {
    const v = openParity();
    v.free();
    expect(errorCode(() => v.addresses())).not.toBe('ok');
    expect(errorCode(() => v.mnemonic())).not.toBe('ok');
  });

  it('creates new recovery phrases', () => {
    const a = wasm.generate_mnemonic();
    const b = wasm.generate_mnemonic();
    expect(a).not.toBe(b);
    expect(a.split(' ')).toHaveLength(24);
    expect(wasm.normalize_mnemonic(a)).toBe(a);
    withVault(wasm.Vault.open(a, undefined, undefined, 2), (v) => {
      expect(v.mnemonic()).toBe(a);
      expect(v.addresses()).toHaveLength(2);
    });
  });
});

describe('signing parity', () => {
  it('produces byte-identical transactions to native Rust', () => {
    withVault(openParity(), (v) => {
      for (const t of parity.transfers) {
        const tx = v.sign_transfer_with_nonce(
          balancesJson,
          paramsJson,
          t.height,
          t.to,
          t.currency,
          t.amount,
          t.memo,
          t.fee_level,
          t.expire_delta,
          BigInt(t.nonce),
        );
        expect(tx).toBe(t.tx);
        expect(errorCode(() => wasm.verify_transaction(tx, paramsJson))).toBe('ok');
      }
    });
  });

  it('computes the same maximum amounts and fees', () => {
    withVault(openParity(), (v) => {
      for (const c of parity.max) {
        expect(v.max_amount(balancesJson, paramsJson, c.currency, c.memo, c.fee_level)).toBe(c.amount);
      }
    });
    for (const f of parity.static_fee) {
      expect(wasm.static_fee(paramsJson, f.inputs, f.owners, f.memo, f.fee_level)).toBe(f.fee);
    }
  });

  it('signs the maximum but not one unit more', () => {
    withVault(openParity(), (v) => {
      for (const currency of [NATIVE, parity.token]) {
        for (const memo of [undefined, 'x', 'é'.repeat(32)]) {
          for (const level of [1, 2, 3, 5, 10]) {
            const max = v.max_amount(balancesJson, paramsJson, currency, memo, level);
            const sign = (amount: string) =>
              v.sign_transfer(balancesJson, paramsJson, 1000, parity.recipient, currency, amount, memo, level, 100);
            const tx = sign(max);
            expect(errorCode(() => wasm.verify_transaction(tx, paramsJson))).toBe('ok');
            expect(['insufficient_fee', 'insufficient_funds']).toContain(
              errorCode(() => sign((BigInt(max) + 1n).toString())),
            );
          }
        }
      }
    });
  });

  it('uses a fresh nonce for every signature', () => {
    withVault(openParity(), (v) => {
      type Signed = { nonce: number; expires: number; fee_ratio: number };
      const sign = () =>
        JSON.parse(
          v.sign_transfer(balancesJson, paramsJson, 1000, parity.recipient, NATIVE, '1000', undefined, 1, 100),
        ) as Signed;
      const a = sign();
      const b = sign();
      expect(a.nonce).not.toBe(b.nonce);
      expect(a.expires).toBe(1100);
      expect(a.fee_ratio).toBe(1024);
    });
  });

  it('rejects tampered or foreign-network transactions', () => {
    const tx = parity.transfers[0]!.tx;
    const tampered = tx.replace('"amount":"15000000"', '"amount":"15000001"');
    expect(tampered).not.toBe(tx);
    expect(errorCode(() => wasm.verify_transaction(tampered, paramsJson))).toBe('tx_rejected');
    const mainnet = JSON.stringify({ ...parity.params, network: 'mainnet' });
    expect(errorCode(() => wasm.verify_transaction(tx, mainnet))).toBe('tx_rejected');
    expect(errorCode(() => wasm.verify_transaction('{}', paramsJson))).toBe('invalid_argument');
  });
});

describe('validation and error codes', () => {
  interface Overrides {
    balances: string;
    params: string;
    height: number;
    to: string;
    currency: string;
    amount: string;
    memo: string;
    level: number;
    expire: number;
  }
  const sign = (v: Vault, over: Partial<Overrides>) =>
    errorCode(() =>
      v.sign_transfer(
        over.balances ?? balancesJson,
        over.params ?? paramsJson,
        over.height ?? 1000,
        over.to ?? parity.recipient,
        over.currency ?? NATIVE,
        over.amount ?? '1000',
        over.memo,
        over.level ?? 1,
        over.expire ?? 100,
      ),
    );

  it('validates every transfer field', () => {
    withVault(openParity(), (v) => {
      expect(sign(v, {})).toBe('ok');
      for (const to of ['', 'MMX', NATIVE, parity.recipient.toUpperCase(), parity.recipient.slice(0, 61)]) {
        expect(sign(v, { to })).toBe('invalid_address');
      }
      for (const amount of ['', '0', '-1', '1.5', ' 1', '1e3', '0x1', '340282366920938463463374607431768211456']) {
        expect(sign(v, { amount })).toBe('invalid_amount');
      }
      expect(sign(v, { memo: 'é'.repeat(33) })).toBe('invalid_memo');
      expect(sign(v, { memo: 'é'.repeat(32) })).toBe('ok');
      for (const level of [0, 4, 6, 100]) {
        expect(sign(v, { level })).toBe('invalid_argument');
      }
      expect(sign(v, { expire: 0 })).toBe('invalid_argument');
      expect(sign(v, { currency: 'MMX' })).toBe('invalid_argument');
      expect(sign(v, { params: '{}' })).toBe('invalid_argument');
      expect(sign(v, { balances: '{}' })).toBe('invalid_argument');
      expect(sign(v, { balances: JSON.stringify([{ ...parity.balances[0], amount: 1 }]) })).toBe('invalid_argument');
      expect(sign(v, { amount: '21001501' })).toBe('insufficient_funds');
      expect(sign(v, { amount: '21001500' })).toBe('insufficient_fee');
      const tokenOnly = JSON.stringify([{ address: parity.addresses[0], contract: parity.token, amount: '10' }]);
      expect(sign(v, { balances: tokenOnly, currency: parity.token, amount: '10' })).toBe('insufficient_fee');
      expect(errorCode(() => v.max_amount(tokenOnly, paramsJson, parity.token, undefined, 1))).toBe('insufficient_fee');
      expect(v.max_amount('[]', paramsJson, NATIVE, undefined, 1)).toBe('0');
      expect(errorCode(() => v.max_amount(balancesJson, paramsJson, NATIVE, 'x'.repeat(65), 1))).toBe('invalid_memo');
    });
  });

  it('parses and formats amounts exactly', () => {
    expect(wasm.parse_amount('1.5', 6)).toBe('1500000');
    expect(wasm.parse_amount('0.000001', 6)).toBe('1');
    expect(wasm.parse_amount('340282366920938463463374607431768.211455', 6)).toBe(
      '340282366920938463463374607431768211455',
    );
    for (const bad of ['', '0', '-1', '1e3', '1,5', '1 000', '1.0000001', '.']) {
      expect(errorCode(() => wasm.parse_amount(bad, 6))).toBe('invalid_amount');
    }
    expect(errorCode(() => wasm.parse_amount('1', 19))).toBe('invalid_argument');
    expect(wasm.format_amount('1500000', 6)).toBe('1.5');
    expect(wasm.format_amount('0', 6)).toBe('0');
    expect(errorCode(() => wasm.format_amount('1.5', 6))).toBe('invalid_amount');
  });

  it('accepts canonical addresses only', () => {
    expect(wasm.validate_address(parity.addresses[0]!)).toBe(true);
    expect(wasm.validate_address(NATIVE)).toBe(true);
    const address = parity.addresses[0]!;
    for (const bad of ['', 'MMX', address.toUpperCase(), ` ${address}`, `${address.slice(0, 61)}q`]) {
      expect(wasm.validate_address(bad)).toBe(false);
    }
  });

  it('prices fees by priority level', () => {
    expect(wasm.static_fee(paramsJson, 1, 1, undefined, 1)).toBe('1440');
    expect(wasm.static_fee(paramsJson, 1, 1, '', 10)).toBe('14400');
    expect(errorCode(() => wasm.static_fee(paramsJson, 1, 1, undefined, 4))).toBe('invalid_argument');
    expect(errorCode(() => wasm.static_fee(paramsJson, 0, 1, undefined, 1))).toBe('invalid_argument');
    expect(errorCode(() => wasm.static_fee('{"network":""}', 1, 1, undefined, 1))).toBe('invalid_argument');
  });
});
