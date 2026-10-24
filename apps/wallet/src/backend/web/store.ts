/**
 * Web wallet metadata in localStorage (docs/wallet-app.md §6.6):
 *
 *   "mmx-wallet/v1" = { wallets: { [fingerprint]: { name, withPassphrase,
 *                       addressCount, verified, added } },
 *                       activeWallet, labels, contacts, settings }
 *   "mmx-wallet/lock" = changes when "Lock" was chosen in a tab, so the other
 *                       tabs lock too (the value itself means nothing)
 *
 * Never contains recovery words, passphrases or keys. Every read validates
 * the data; a damaged wallet entry is skipped without affecting the others
 * and is left untouched in storage. Every write re-reads the current value
 * first, so changes made by another tab are not overwritten.
 */
import { BackendError, type AppState, type WalletId } from '../types';
import { DEFAULT_ADDRESS_COUNT, MAX_ADDRESSES, sanitizeAppState, WALLET_NAME_MAX } from '../../lib/appstate';
import { sanitizeText } from '../../lib/text';

export const STORAGE_KEY = 'mmx-wallet/v1';
/** An unreadable value is moved here before it would be overwritten. */
export const DAMAGED_KEY = 'mmx-wallet/v1.damaged';
/** Written by an explicit "Lock" so that the other tabs lock as well. */
export const LOCK_SIGNAL_KEY = 'mmx-wallet/lock';

export interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

export interface WalletRecord {
  name: string;
  withPassphrase: boolean;
  addressCount: number;
  /**
   * False only for wallets stored by older versions before their recovery
   * phrase was confirmed; such a wallet asks to be opened once more.
   */
  verified: boolean;
  /** When the wallet was added (ms); orders the wallet list. */
  added: number;
}

type Root = Record<string, unknown>;

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

/** Canonical decimal u32 ("0", "42", … "4294967295"). */
export function isFingerprint(v: unknown): v is string {
  return typeof v === 'string' && /^(0|[1-9]\d{0,9})$/u.test(v) && Number(v) <= 0xffff_ffff;
}

/**
 * Validates one stored wallet entry. Returns null when it cannot be used.
 * Damaged fields fall back to safe values: an unknown passphrase state counts
 * as "with passphrase" (the user is asked rather than told the words are
 * wrong). Only an explicit `verified: false` marks an unconfirmed wallet:
 * wallets are stored after their recovery phrase was confirmed.
 */
export function parseWalletRecord(v: unknown): WalletRecord | null {
  if (!isRecord(v)) return null;
  const name = sanitizeText(v.name, WALLET_NAME_MAX).trim() || 'Wallet';
  const count = v.addressCount;
  const added = v.added;
  return {
    name,
    withPassphrase: typeof v.withPassphrase === 'boolean' ? v.withPassphrase : true,
    addressCount:
      typeof count === 'number' && Number.isInteger(count) && count >= 1 && count <= MAX_ADDRESSES
        ? count
        : DEFAULT_ADDRESS_COUNT,
    verified: v.verified !== false,
    added: typeof added === 'number' && Number.isFinite(added) && added >= 0 ? added : 0,
  };
}

interface Snapshot {
  root: Root;
  wallets: Map<WalletId, WalletRecord>;
  /** Raw value that was not a JSON object. */
  damaged?: string;
}

export class MetadataStore {
  constructor(private readonly storage: StorageLike) {}

  private read(): Snapshot {
    let raw: string | null;
    try {
      raw = this.storage.getItem(STORAGE_KEY);
    } catch {
      raw = null;
    }
    if (raw === null) return { root: {}, wallets: new Map() };
    let root: unknown;
    try {
      root = JSON.parse(raw);
    } catch {
      return { root: {}, wallets: new Map(), damaged: raw };
    }
    if (!isRecord(root)) return { root: {}, wallets: new Map(), damaged: raw };
    const wallets = new Map<WalletId, WalletRecord>();
    if (isRecord(root.wallets)) {
      for (const [id, value] of Object.entries(root.wallets)) {
        if (!isFingerprint(id)) continue;
        const record = parseWalletRecord(value);
        if (record) wallets.set(id, record);
      }
    }
    return { root, wallets };
  }

  /** Applies `change` to the current stored value and writes it back. Throws io_error. */
  private update(change: (root: Root, wallets: Record<string, unknown>) => void): void {
    const snap = this.read();
    if (snap.damaged !== undefined) {
      try {
        if (this.storage.getItem(DAMAGED_KEY) === null) this.storage.setItem(DAMAGED_KEY, snap.damaged);
      } catch {
        // Keeping the app usable matters more than the unreadable value.
      }
    }
    const root = snap.root;
    const wallets: Record<string, unknown> = isRecord(root.wallets) ? { ...root.wallets } : {};
    change(root, wallets);
    root.wallets = wallets;
    try {
      this.storage.setItem(STORAGE_KEY, JSON.stringify(root));
    } catch {
      throw new BackendError('io_error');
    }
  }

  /** Wallets in the order they were added. */
  list(): Array<{ id: WalletId; record: WalletRecord }> {
    return [...this.read().wallets]
      .map(([id, record]) => ({ id, record }))
      .sort((a, b) => a.record.added - b.record.added || Number(a.id) - Number(b.id));
  }

  get(id: WalletId): WalletRecord | undefined {
    return this.read().wallets.get(id);
  }

  has(id: WalletId): boolean {
    return this.read().wallets.has(id);
  }

  /**
   * Adds or replaces a wallet entry (unknown fields of an existing entry are
   * kept); with `active` it also becomes the active wallet, in the same write.
   */
  putWallet(id: WalletId, record: WalletRecord, options: { active?: boolean } = {}): void {
    if (!isFingerprint(id)) throw new BackendError('internal');
    this.update((root, wallets) => {
      const existing = wallets[id];
      wallets[id] = { ...(isRecord(existing) ? existing : {}), ...record };
      if (options.active === true) root.activeWallet = id;
    });
  }

  /** Changes fields of a valid wallet entry. Throws wallet_not_found. */
  patchWallet(id: WalletId, patch: Partial<WalletRecord>): WalletRecord {
    let result: WalletRecord | undefined;
    this.update((_, wallets) => {
      const existing = wallets[id];
      const current = parseWalletRecord(existing);
      if (!isFingerprint(id) || !current || !isRecord(existing)) throw new BackendError('wallet_not_found');
      result = { ...current, ...patch };
      wallets[id] = { ...existing, ...result };
    });
    return result!;
  }

  /** Forgets a wallet on this device (with its labels). */
  removeWallet(id: WalletId): void {
    this.update((root, wallets) => {
      delete wallets[id];
      if (root.activeWallet === id) delete root.activeWallet;
      if (isRecord(root.labels) && id in root.labels) {
        const labels = { ...root.labels };
        delete labels[id];
        root.labels = labels;
      }
    });
  }

  setActiveWallet(id: WalletId): void {
    this.update((root) => {
      root.activeWallet = id;
    });
  }

  /** Labels, contacts and settings, validated. */
  appState(): AppState {
    const snap = this.read();
    const state = sanitizeAppState(snap.root, 'web');
    if (state.activeWallet !== undefined && !snap.wallets.has(state.activeWallet)) delete state.activeWallet;
    return state;
  }

  saveAppState(s: AppState): void {
    const clean = sanitizeAppState(s, 'web');
    this.update((root) => {
      root.labels = clean.labels;
      root.contacts = clean.contacts;
      root.settings = clean.settings;
      if (clean.activeWallet !== undefined) root.activeWallet = clean.activeWallet;
      else delete root.activeWallet;
    });
  }
}
