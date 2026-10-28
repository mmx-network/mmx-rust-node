/**
 * Backend contract shared by the desktop (Tauri), web (WASM + password manager)
 * and mock implementations. See docs/wallet-app.md §6.2.
 */

/** Desktop: key file name inside the wallet directory. Web: wallet fingerprint. */
export type WalletId = string;

export type Platform = 'desktop' | 'web';

export interface WalletInfo {
  id: WalletId;
  name: string;
  /** Decimal u32 string ("Wallet ID" in the UI). */
  fingerprint: string;
  withPassphrase: boolean;
  locked: boolean;
  /** Public addresses are cached, so the wallet can be observed while signing is locked. */
  canView?: boolean;
  addressCount: number;
  /** Set when the wallet cannot be read (desktop: corrupt key file). */
  error?: string;
  /**
   * Web only: false for a wallet that an earlier version stored before its
   * saved credential was confirmed. Missing means confirmed.
   */
  verified?: boolean;
  /** Web: a passphrase-protected wallet has its base seed stored locally. */
  hasStoredSeed?: boolean;
}

export interface Contact {
  id: string;
  name: string;
  address: string;
  note?: string;
}

export type ThemeSetting = 'system' | 'light' | 'dark';
export type FeeLevel = 1 | 2 | 3 | 5 | 10;

export interface Settings {
  theme: ThemeSetting;
  /** Minutes of inactivity before locking. 0 = never (desktop only). */
  autoLockMinutes: number;
  feeLevel: FeeLevel;
  /** Desktop only. Undefined = default public server. */
  rpcUrl?: string;
}

export interface AppState {
  activeWallet?: WalletId;
  /** Address labels per wallet, keyed by address index. */
  labels: Record<WalletId, Record<number, string>>;
  contacts: Contact[];
  settings: Settings;
}

export interface SendRequest {
  walletId: WalletId;
  to: string;
  /** Currency contract address; the native currency is the zero address. */
  contract: string;
  /** Atomic units as a decimal string. */
  amount: string;
  memo?: string;
  feeLevel: FeeLevel;
}

export interface PreparedSend {
  txid: string;
  to: string;
  contract: string;
  symbol: string;
  decimals: number;
  /** Atomic units as a decimal string. */
  amount: string;
  /** Network fee in native atomic units as a decimal string (validated by the node). */
  fee: string;
  /** Height after which the transaction can no longer be included. */
  expires: number;
  /** Chain height the transaction was prepared at. */
  height: number;
  memo?: string;
}

export interface CreateWalletRequest {
  name: string;
  /** Recovery words from `newRecoveryPhrase()`, already confirmed by the user. */
  mnemonic: string;
  passphrase?: string;
  addressCount: number;
}

export interface FingerprintRequest {
  mnemonic: string;
  passphrase?: string;
}

export interface LockOptions {
  /** Web: also lock the wallets open in other tabs and windows (explicit Lock only). */
  everywhere?: boolean;
}

export interface ImportWalletRequest {
  name: string;
  mnemonic: string;
  passphrase?: string;
  addressCount: number;
  /**
   * Web only: the fingerprint the phrase is expected to open (from a saved
   * credential's username). Enables passphrase detection (§6.3) on a device
   * that does not know the wallet yet.
   */
  expectedFingerprint?: string;
}

export interface UnlockRequest {
  passphrase?: string;
  /** Web only: the recovery words supplied by the password manager. */
  mnemonic?: string;
}

export interface Backend {
  readonly platform: Platform;

  loadAppState(): Promise<AppState>;
  saveAppState(s: AppState): Promise<void>;

  listWallets(): Promise<WalletInfo[]>;
  /** Desktop: choose and copy a seed file locally. No key material crosses IPC. */
  importSeedFile?(r: { name: string; addressCount: number }): Promise<WalletInfo | null>;
  /** 24 new recovery words (lowercase, single spaces). Stores nothing. */
  newRecoveryPhrase(): Promise<string>;
  /**
   * Stores a new wallet once its recovery words were confirmed, opens it and
   * makes it the active wallet. Fails with `wallet_exists` when the wallet is
   * already on this device.
   */
  createWallet(r: CreateWalletRequest): Promise<WalletInfo>;
  /**
   * Web only: the wallet ID (fingerprint) that the words open with this
   * passphrase, for the password manager username of a new wallet. Stores nothing.
   */
  walletFingerprint?(r: FingerprintRequest): Promise<string>;
  importWallet(r: ImportWalletRequest): Promise<WalletInfo>;
  unlockWallet(id: WalletId, r: UnlockRequest): Promise<WalletInfo>;
  lockWallet(id: WalletId): Promise<void>;
  /** Locks every wallet of this window; `everywhere` also signals other tabs (web). */
  lockAll(opts?: LockOptions): Promise<void>;
  renameWallet(id: WalletId, name: string): Promise<void>;
  /** Returns the wallet's addresses after the change. */
  setAddressCount(id: WalletId, n: number, passphrase?: string): Promise<string[]>;
  removeWallet(id: WalletId): Promise<void>;
  revealMnemonic(id: WalletId, passphrase?: string): Promise<string>;
  getAddresses(id: WalletId): Promise<string[]>;
  setActiveWallet(id: WalletId): Promise<void>;

  /** GET on the chain server. Resolves to the parsed JSON body (null for an empty body). */
  rpcGet(path: string): Promise<unknown>;

  maxSendable(r: Omit<SendRequest, 'amount'>): Promise<string>;
  prepareSend(r: SendRequest): Promise<PreparedSend>;
  broadcast(walletId: WalletId, txid: string): Promise<void>;
  discardPrepared(txid: string): Promise<void>;

  /**
   * sensitive → the clipboard is cleared after 60 s if it still holds the text
   * (desktop). Browsers allow that only around user actions, so the web clears
   * it on a best-effort basis.
   */
  copyText(text: string, sensitive?: boolean): Promise<void>;
  /** Explorer links only. */
  openExternal(url: string): Promise<void>;
  /** Resolves false when the user cancelled. */
  exportFile(suggestedName: string, contents: string): Promise<boolean>;
  /** JSON text ≤ 2 MiB, null when the user cancelled. */
  importFile(): Promise<string | null>;

  /**
   * Optional: notifies when persisted wallets/app state changed outside this
   * window (web: `storage` event from another tab). Returns an unsubscribe function.
   */
  onExternalChange?(listener: () => void): () => void;

  /**
   * Optional: notifies when another tab or window locked every wallet with an
   * explicit Lock (web). The keys of this window are already freed when the
   * listener runs. Returns an unsubscribe function.
   */
  onExternalLock?(listener: () => void): () => void;
}

export const ERROR_CODES = [
  'invalid_passphrase',
  'passphrase_required',
  'invalid_mnemonic',
  'invalid_key_file',
  'wallet_mismatch',
  'wallet_exists',
  'wallet_not_found',
  'locked',
  'invalid_address',
  'invalid_amount',
  'invalid_memo',
  'insufficient_funds',
  'insufficient_fee',
  'invalid_argument',
  'rpc_unreachable',
  'rpc_timeout',
  'rpc_not_synced',
  'rpc_network_mismatch',
  'rpc_http_error',
  'rpc_response_invalid',
  'tx_rejected',
  'tx_expired',
  'tx_not_sent',
  'prepared_not_found',
  'io_error',
  'internal',
] as const;

export type ErrorCode = (typeof ERROR_CODES)[number];

const CODE_SET: ReadonlySet<string> = new Set(ERROR_CODES);

export function isErrorCode(value: unknown): value is ErrorCode {
  return typeof value === 'string' && CODE_SET.has(value);
}

export class BackendError extends Error {
  readonly code: ErrorCode;
  constructor(code: ErrorCode, message?: string) {
    super(message ?? code);
    this.name = 'BackendError';
    this.code = code;
  }
}

export function isBackendError(e: unknown, code?: ErrorCode): e is BackendError {
  return e instanceof BackendError && (code === undefined || e.code === code);
}

/**
 * Normalizes anything thrown by a backend or transport into a BackendError.
 * Accepts `{code, message}` objects (Tauri command errors) and plain strings
 * that are error codes (WASM `JsError` messages). The original message of an
 * unknown error is never carried over, so secrets cannot leak through it.
 */
export function toBackendError(e: unknown): BackendError {
  if (e instanceof BackendError) return e;
  if (e && typeof e === 'object') {
    const code = (e as { code?: unknown }).code;
    if (isErrorCode(code)) {
      const message = (e as { message?: unknown }).message;
      return new BackendError(code, typeof message === 'string' ? message : code);
    }
    const message = (e as { message?: unknown }).message;
    if (isErrorCode(message)) return new BackendError(message);
  }
  if (isErrorCode(e)) return new BackendError(e);
  return new BackendError('internal');
}
