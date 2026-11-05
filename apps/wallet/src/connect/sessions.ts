/** Public connection grants, never signing sessions. A grant pins an approved
 * account to the actual website origin across short-lived popup windows. */
import { BackendError } from '../backend/types';
import type { StorageLike } from '../backend/web/store';
import { normalizeAddress } from '../lib/address';
import { origin, object, randomToken, token, type Account } from './protocol';

export const WEBSITE_SESSIONS_KEY = 'mmx-wallet/website-sessions/v1';
export const WEBSITE_SESSION_TTL = 60 * 60_000;
const MAX_SESSIONS = 32;
export interface WebsiteSession {
  id: string;
  origin: string;
  account: Account;
  walletName: string;
  expires: number;
}
export function validAccount(v: unknown): v is Account {
  return (
    object(v) &&
    typeof v.walletId === 'string' &&
    v.walletId.length <= 64 &&
    typeof v.address === 'string' &&
    normalizeAddress(v.address) === v.address &&
    Array.isArray(v.addresses) &&
    v.addresses.length >= 1 &&
    v.addresses.length <= 100 &&
    v.addresses[0] === v.address &&
    v.addresses.every((a) => typeof a === 'string' && normalizeAddress(a) === a) &&
    typeof v.network === 'string' &&
    v.network.length > 0 &&
    v.network.length <= 256 &&
    (v.decimals === undefined || (Number.isInteger(v.decimals) && Number(v.decimals) >= 0 && Number(v.decimals) <= 18))
  );
}
export class WebsiteSessions {
  constructor(
    private readonly storage: StorageLike,
    private readonly now = () => Date.now(),
  ) {}
  private read(includeExpired = false): WebsiteSession[] {
    try {
      const raw = this.storage.getItem(WEBSITE_SESSIONS_KEY);
      if (!raw || raw.length > 512_000) return [];
      const value: unknown = JSON.parse(raw);
      if (!Array.isArray(value)) return [];
      return value
        .slice(0, MAX_SESSIONS)
        .filter(
          (v): v is WebsiteSession =>
            object(v) &&
            token(v.id) &&
            origin(v.origin) !== null &&
            validAccount(v.account) &&
            typeof v.walletName === 'string' &&
            v.walletName.length <= 64 &&
            typeof v.expires === 'number' && Number.isSafeInteger(v.expires) && v.expires >= 0 &&
            (includeExpired || (v.expires > this.now() && v.expires <= this.now() + WEBSITE_SESSION_TTL)),
        );
    } catch {
      return [];
    }
  }
  private save(sessions: WebsiteSession[]) {
    try {
      if (sessions.length) this.storage.setItem(WEBSITE_SESSIONS_KEY, JSON.stringify(sessions));
      else this.storage.removeItem(WEBSITE_SESSIONS_KEY);
    } catch {
      throw new BackendError('io_error');
    }
  }
  create(site: string, account: Account, walletName: string): WebsiteSession {
    if (!origin(site) || !validAccount(account)) throw new BackendError('invalid_argument');
    const session = {
      id: randomToken(),
      origin: site,
      account,
      walletName: walletName.slice(0, 64),
      expires: this.now() + WEBSITE_SESSION_TTL,
    };
    this.save([...this.read().slice(-(MAX_SESSIONS - 1)), session]);
    return session;
  }
  get(id: string, site: string): WebsiteSession | null {
    return token(id) && origin(site) ? (this.read().find((s) => s.id === id && s.origin === site) ?? null) : null;
  }
  revoke(site?: string): void {
    const before = this.read(true);
    const after = site ? before.filter((s) => s.origin !== site) : [];
    if (!site || before.length !== after.length) this.save(after);
  }
}
