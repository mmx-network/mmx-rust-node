import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  canUseCredentialApi,
  credentialUsername,
  getCredential,
  parseCredentialUsername,
  preventSilentAccess,
  requestCredential,
  storeCredential,
} from '../../src/backend/web/credentials';

const WORDS = 'abandon '.repeat(23) + 'art';

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('credential usernames', () => {
  it('round-trips name and wallet ID', () => {
    const u = credentialUsername('Savings', '3291348052');
    expect(u).toBe('Savings (3291348052)');
    expect(parseCredentialUsername(u)).toEqual({ name: 'Savings', fingerprint: '3291348052' });
  });

  it('keeps names with parentheses and trims', () => {
    expect(parseCredentialUsername('  My (old) wallet (42)  ')).toEqual({ name: 'My (old) wallet', fingerprint: '42' });
    expect(parseCredentialUsername('Wallet (0)')).toEqual({ name: 'Wallet', fingerprint: '0' });
  });

  it('accepts only canonical u32 wallet IDs', () => {
    for (const bad of ['Main (4294967296)', 'Main (0042)', 'Main (12345678901)', 'Main (-1)', 'Main (1.5)', 'Main ()']) {
      expect(parseCredentialUsername(bad).fingerprint, bad).toBeUndefined();
    }
    expect(parseCredentialUsername('Main (4294967295)').fingerprint).toBe('4294967295');
  });

  it('returns display-safe names', () => {
    expect(parseCredentialUsername('evil\u{202e}gnp.exe (7)')).toEqual({ name: 'evilgnp.exe', fingerprint: '7' });
    expect(parseCredentialUsername('a\nb (7)').name).toBe('a b');
    expect(parseCredentialUsername('x'.repeat(100)).name).toHaveLength(64);
    expect(parseCredentialUsername('').name).toBe('Wallet');
    expect(parseCredentialUsername(' (7)')).toEqual({ name: 'Wallet', fingerprint: '7' });
    expect(parseCredentialUsername(undefined as unknown as string)).toEqual({ name: 'Wallet' });
    expect(credentialUsername('  \u{200b} ', '9')).toBe('Wallet (9)');
  });
});

describe('Credential Management API', () => {
  it('is reported unavailable without throwing where unsupported', async () => {
    expect(canUseCredentialApi()).toBe(false);
    expect(await storeCredential('Main (1)', WORDS)).toBe(false);
    expect(await getCredential()).toBeNull();
    await expect(preventSilentAccess()).resolves.toBeUndefined();
    vi.stubGlobal('window', {});
    vi.stubGlobal('navigator', {});
    expect(canUseCredentialApi()).toBe(false);
    expect(await getCredential()).toBeNull();
    await expect(preventSilentAccess()).resolves.toBeUndefined();
  });

  function install(get: () => Promise<unknown>) {
    const stored: unknown[] = [];
    class PasswordCredential {
      readonly type = 'password';
      readonly id: string;
      readonly password: string;
      readonly name: string | undefined;
      constructor(data: { id: string; password: string; name?: string }) {
        if (!data.id) throw new TypeError('id required');
        this.id = data.id;
        this.password = data.password;
        this.name = data.name;
      }
    }
    const credentials = {
      store: vi.fn(async (c: unknown) => {
        stored.push(c);
        return c;
      }),
      get: vi.fn(get),
      preventSilentAccess: vi.fn(async () => undefined),
      create: vi.fn(),
    };
    vi.stubGlobal('window', { PasswordCredential });
    vi.stubGlobal('navigator', { credentials });
    return { stored, credentials, PasswordCredential };
  }

  it('stores a password credential', async () => {
    const { stored, PasswordCredential } = install(async () => null);
    expect(canUseCredentialApi()).toBe(true);
    expect(await storeCredential('Main (1)', WORDS)).toBe(true);
    expect(stored).toHaveLength(1);
    expect(stored[0]).toBeInstanceOf(PasswordCredential);
    expect(stored[0]).toMatchObject({ id: 'Main (1)', password: WORDS });
    expect(await storeCredential('', WORDS)).toBe(false);
    expect(await storeCredential('Main (1)', '')).toBe(false);
  });

  it('reports a declined save as false', async () => {
    const { credentials } = install(async () => null);
    credentials.store.mockRejectedValueOnce(new DOMException('denied', 'NotAllowedError'));
    expect(await storeCredential('Main (1)', WORDS)).toBe(false);
  });

  it('gets a password credential with optional mediation', async () => {
    const { credentials } = install(async () => ({ type: 'password', id: 'Main (1)', password: WORDS }));
    expect(await getCredential()).toEqual({ username: 'Main (1)', words: WORDS });
    expect(credentials.get).toHaveBeenCalledWith({ password: true, mediation: 'optional' });
    expect(requestCredential).toBe(getCredential);
  });

  it('ignores cancelled, failing and non-password results', async () => {
    for (const result of [null, { type: 'public-key', id: 'x' }, { type: 'password', id: 'x', password: '' }, { type: 'password', id: 5, password: WORDS }]) {
      install(async () => result);
      expect(await getCredential()).toBeNull();
    }
    install(async () => {
      throw new DOMException('aborted', 'AbortError');
    });
    expect(await getCredential()).toBeNull();
  });

  it('prevents silent access when supported', async () => {
    const { credentials } = install(async () => null);
    await preventSilentAccess();
    expect(credentials.preventSilentAccess).toHaveBeenCalledOnce();
    credentials.preventSilentAccess.mockRejectedValueOnce(new Error('nope'));
    await expect(preventSilentAccess()).resolves.toBeUndefined();
  });
});
