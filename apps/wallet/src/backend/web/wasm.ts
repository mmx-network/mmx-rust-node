/**
 * The wallet core compiled to WebAssembly (crates/mmx-wallet-wasm, built into
 * ./pkg by scripts/build-wasm.sh). Loaded on demand; the .wasm file is a
 * separate asset fetched from this site.
 */

/** An opened seed inside WASM memory. `free()` clears it. */
export interface VaultHandle {
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
  /** Signs a transfer; throws insufficient_fee when the fee payer cannot cover the fee (fee_payer_covered). */
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
}

/** The part of the WASM API the web backend uses. */
export interface WasmApi {
  generate_mnemonic(): string;
  normalize_mnemonic(words: string): string;
  fingerprint(words: string, passphrase?: string | null): string;
  validate_address(address: string): boolean;
  /** Checks hashes, cost and signatures of a signed transfer; throws when they do not hold. */
  verify_transaction(txJson: string, paramsJson: string): void;
  Vault: {
    open(
      words: string,
      passphrase: string | null | undefined,
      expectedFingerprint: string | null | undefined,
      count: number,
    ): VaultHandle;
  };
}

let loading: Promise<WasmApi> | null = null;

/** Loads and instantiates the WASM module once; a failed load is retried on the next call. */
export function loadBrowserWasm(): Promise<WasmApi> {
  if (!loading) {
    loading = (async () => {
      const [glue, asset] = await Promise.all([
        import('./pkg/mmx_wallet_wasm.js'),
        import('./pkg/mmx_wallet_wasm_bg.wasm?url'),
      ]);
      await glue.default({ module_or_path: asset.default });
      return glue;
    })();
    loading.catch(() => {
      loading = null;
    });
  }
  return loading;
}
