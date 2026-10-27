import { describe, expect, it } from 'vitest';
import type { Contact } from '../../src/backend/types';
import { encodeAddress as encode, NATIVE_CONTRACT } from '../../src/lib/address';
import {
  cleanLabel,
  defaultAppState,
  hasErrors,
  MAX_CONTACTS,
  mergeContacts,
  parseContactsFile,
  sanitizeAppState,
  sanitizeContacts,
  sanitizeLabels,
  sanitizeSettings,
  serializeContacts,
  validateContact,
} from '../../src/lib/appstate';

const A = 'mmx1ecntftefvza4k9kev5q66la87akr62sljsh28jtfxwyvasgjz8gswl046q';
const B = 'mmx1dp0rqfnjpvfyj9rn80qfcp7l6e6grme5uzm9202yskgdgp449a0snelu0z';
const C = 'mmx1c2sszjvfpf4qn8m57lsevths07mumt0fknq9jw07swr30z5qknpqp29vvx';

describe('sanitizeAppState', () => {
  it('returns defaults for garbage', () => {
    for (const raw of [null, undefined, 1, 'x', [], { labels: 5, contacts: {}, settings: [] }]) {
      expect(sanitizeAppState(raw, 'desktop')).toEqual(defaultAppState());
    }
  });

  it('keeps valid data', () => {
    const state = sanitizeAppState(
      {
        activeWallet: 'wallet.dat',
        labels: { 'wallet.dat': { 0: 'Main', 5: 'Rent' } },
        contacts: [{ id: 'c-1', name: 'Alice', address: A, note: 'Friend' }],
        settings: { theme: 'dark', autoLockMinutes: 30, feeLevel: 5, rpcUrl: 'https://node.example' },
      },
      'desktop',
    );
    expect(state).toEqual({
      activeWallet: 'wallet.dat',
      labels: { 'wallet.dat': { 0: 'Main', 5: 'Rent' } },
      contacts: [{ id: 'c-1', name: 'Alice', address: A, note: 'Friend' }],
      settings: { theme: 'dark', autoLockMinutes: 30, feeLevel: 5, rpcUrl: 'https://node.example' },
    });
  });

  it('applies platform rules to settings', () => {
    expect(sanitizeSettings({ autoLockMinutes: 0 }, 'desktop').autoLockMinutes).toBe(0);
    expect(sanitizeSettings({ autoLockMinutes: 0 }, 'web').autoLockMinutes).toBe(0);
    expect(sanitizeSettings({ rpcUrl: 'https://x.example' }, 'web').rpcUrl).toBeUndefined();
    expect(sanitizeSettings({ rpcUrl: 'https://x.example/ a' }, 'desktop').rpcUrl).toBeUndefined();
    expect(sanitizeSettings({ autoLockMinutes: -5, feeLevel: 4, theme: 'blue' }, 'desktop')).toEqual({
      theme: 'system',
      autoLockMinutes: 5,
      feeLevel: 1,
    });
    expect(sanitizeSettings({ autoLockMinutes: 1.5 }, 'desktop').autoLockMinutes).toBe(5);
    expect(sanitizeSettings({ autoLockMinutes: 100000 }, 'desktop').autoLockMinutes).toBe(5);
  });

  it('cleans labels', () => {
    expect(
      sanitizeLabels({
        w: { 0: '  Main  ', 1: '', 2: 'x'.repeat(40), 100: 'too high', '-1': 'neg', a: 'nan', 3: 'a\u{202e}b', 4: 7 },
        '': { 0: 'x' },
        empty: {},
        bad: 'x',
      }),
    ).toEqual({ w: { 0: 'Main', 2: 'x'.repeat(32), 3: 'ab' } });
    expect(cleanLabel('   ')).toBeUndefined();
  });

  it('drops invalid and duplicate contacts', () => {
    const list = sanitizeContacts([
      { id: 'c-1', name: 'Alice', address: A },
      { id: 'c-2', name: 'Alice again', address: A.toUpperCase() },
      { id: 'c-1', name: 'Bob', address: B },
      { id: '../evil', name: 'Carol', address: C },
      { name: '', address: B },
      { name: 'Zero', address: NATIVE_CONTRACT },
      { name: 'Bad', address: 'mmx1nope' },
      'junk',
    ]);
    expect(list.map((c) => c.name)).toEqual(['Alice', 'Bob', 'Carol']);
    expect(new Set(list.map((c) => c.id)).size).toBe(3);
    expect(list[0]!.id).toBe('c-1');
    expect(list[1]!.id).not.toBe('c-1');
    expect(list[2]!.id).toMatch(/^c-[0-9a-f]{16}$/);
  });

  it('caps the number of contacts', () => {
    const many = Array.from({ length: MAX_CONTACTS + 10 }, (_, i) => ({ name: `n${i}`, address: fakeAddress(i) }));
    const kept = sanitizeContacts(many);
    expect(kept).toHaveLength(MAX_CONTACTS);
    // A full address book takes no new contact, but its contacts can still be edited.
    expect(validateContact({ name: 'New', address: fakeAddress(MAX_CONTACTS + 5) }, kept).address).toBe('full');
    expect(validateContact({ name: 'Renamed', address: kept[0]!.address }, kept, kept[0]!.id)).toEqual({});
  });
});

describe('contacts', () => {
  const existing: Contact[] = [{ id: 'c-1', name: 'Alice', address: A }];

  it('validates input', () => {
    expect(validateContact({ name: 'Bob', address: B }, existing)).toEqual({});
    expect(validateContact({ name: '', address: '' }, existing)).toEqual({ name: 'empty', address: 'empty' });
    expect(validateContact({ name: 'x'.repeat(65), address: 'nope' }, existing)).toEqual({ name: 'too_long', address: 'invalid' });
    expect(validateContact({ name: 'Zero', address: NATIVE_CONTRACT }, existing).address).toBe('invalid');
    expect(validateContact({ name: 'Dup', address: A.toUpperCase() }, existing).address).toBe('duplicate');
    expect(validateContact({ name: 'Alice 2', address: A }, existing, 'c-1')).toEqual({});
    expect(validateContact({ name: 'Me', address: B }, existing, undefined, [B]).address).toBe('own');
    expect(validateContact({ name: 'N', address: B, note: 'x'.repeat(201) }, existing).note).toBe('too_long');
    expect(validateContact({ name: 'N', address: B, note: 'a\nb' }, existing).note).toBe('invalid');
    expect(hasErrors({})).toBe(false);
    expect(hasErrors({ name: undefined })).toBe(false);
    expect(hasErrors({ name: 'empty' })).toBe(true);
  });

  it('round-trips the file format', () => {
    const list: Contact[] = [
      { id: 'c-1', name: 'Alice', address: A, note: 'Friend' },
      { id: 'c-2', name: 'Bob', address: B },
    ];
    const text = serializeContacts(list);
    expect(JSON.parse(text)).toEqual({
      format: 'mmx-wallet-contacts',
      version: 1,
      contacts: [
        { name: 'Alice', address: A, note: 'Friend' },
        { name: 'Bob', address: B },
      ],
    });
    const parsed = parseContactsFile(text)!;
    expect(parsed.map(({ name, address, note }) => ({ name, address, note }))).toEqual([
      { name: 'Alice', address: A, note: 'Friend' },
      { name: 'Bob', address: B, note: undefined },
    ]);
  });

  it('parses plain arrays, rejects non-contact files and never trusts ids', () => {
    expect(parseContactsFile(JSON.stringify([{ id: 'c-1', name: 'X', address: C }]))![0]!.id).not.toBe('c-1');
    expect(parseContactsFile('not json')).toBeNull();
    expect(parseContactsFile('{"foo": 1}')).toBeNull();
    expect(parseContactsFile('42')).toBeNull();
    expect(parseContactsFile(' '.repeat(2 * 1024 * 1024 + 1))).toBeNull();
    expect(parseContactsFile('{"contacts": [{"name": "x", "address": "bad"}]}')).toEqual([]);
  });

  it('merges by address, keeping existing contacts', () => {
    const incoming: Contact[] = [
      { id: 'c-1', name: 'Alice (import)', address: A },
      { id: 'c-1', name: 'Bob', address: B },
      { id: 'c-9', name: 'Carol', address: C },
    ];
    const r = mergeContacts(existing, incoming);
    expect(r.added).toBe(2);
    expect(r.skipped).toBe(1);
    expect(r.full).toBe(0);
    expect(r.contacts.map((c) => c.name)).toEqual(['Alice', 'Bob', 'Carol']);
    expect(new Set(r.contacts.map((c) => c.id)).size).toBe(3);
  });

  it('counts contacts left out by the limit apart from those already saved', () => {
    const book: Contact[] = Array.from({ length: MAX_CONTACTS - 2 }, (_, i) => ({ id: `c-${i}`, name: `n${i}`, address: fakeAddress(i) }));
    const incoming: Contact[] = [
      { id: 'x-0', name: 'Saved', address: fakeAddress(0) },
      ...Array.from({ length: 5 }, (_, i) => ({ id: `x-${i + 1}`, name: `new${i}`, address: fakeAddress(MAX_CONTACTS + i) })),
    ];
    const r = mergeContacts(book, incoming);
    expect(r).toMatchObject({ added: 2, skipped: 1, full: 3 });
    expect(r.contacts).toHaveLength(MAX_CONTACTS);
    expect(mergeContacts(r.contacts, incoming)).toMatchObject({ added: 0, skipped: 3, full: 3 });
  });
});

function fakeAddress(i: number): string {
  // Valid distinct addresses from the encoder.
  const bytes = new Uint8Array(32);
  bytes[0] = i & 255;
  bytes[1] = (i >> 8) & 255;
  bytes[31] = 1;
  return encode(bytes);
}
