import { describe, expect, it } from 'vitest';
import { encodeAddress } from '../../src/lib/address';
import { contactErrorMessages, importSummary } from '../../src/state/actions/contacts';
import { recipientFromText } from '../../src/state/actions/send';
import { autoLockLabel, fundedMessage, isUnencryptedRemote } from '../../src/ui/screens/settings/text';

const A = encodeAddress(new Uint8Array(32).fill(7));
const B = encodeAddress(new Uint8Array(32).fill(9));

describe('recipientFromText', () => {
  it('keeps an address and trims whitespace', () => {
    expect(recipientFromText(`  ${A}\n`)).toBe(A);
    expect(recipientFromText(A.toUpperCase())).toBe(A.toUpperCase());
  });

  it('finds a single address inside text', () => {
    expect(recipientFromText(`Please send to ${A}. Thanks`)).toBe(A);
    expect(recipientFromText(`${A} or ${A}`)).toBe(A);
  });

  it('does not guess between several addresses', () => {
    const text = `${A} ${B}`;
    expect(recipientFromText(text)).toBe(text);
  });

  it('caps long non-address text', () => {
    expect(Array.from(recipientFromText('x'.repeat(500)))).toHaveLength(128);
    expect(recipientFromText(' alice ')).toBe('alice');
  });

  it('ignores invalid address-like strings', () => {
    const broken = A.slice(0, -1) + (A.endsWith('q') ? 'p' : 'q');
    expect(recipientFromText(`to ${broken}`)).toBe(`to ${broken}`);
  });
});

describe('importSummary', () => {
  it('reports added and already saved contacts', () => {
    expect(importSummary({ status: 'imported', added: 3, skipped: 2 })).toEqual({
      message: 'Added 3 contacts. 2 were already saved.',
      kind: 'success',
    });
    expect(importSummary({ status: 'imported', added: 1, skipped: 1 })!.message).toBe('Added 1 contact. 1 was already saved.');
    expect(importSummary({ status: 'imported', added: 2, skipped: 0 })!.message).toBe('Added 2 contacts.');
  });

  it('handles nothing new, invalid files and cancel', () => {
    expect(importSummary({ status: 'imported', added: 0, skipped: 4 })!.kind).toBe('info');
    expect(importSummary({ status: 'imported', added: 0, skipped: 0 })!.message).toBe('This file has no contacts to import.');
    expect(importSummary({ status: 'invalid' })!.kind).toBe('error');
    expect(importSummary({ status: 'cancelled' })).toBeNull();
  });
});

describe('contactErrorMessages', () => {
  it('maps every error to one line', () => {
    expect(contactErrorMessages({})).toEqual({});
    const all = contactErrorMessages({ name: 'too_long', address: 'own', note: 'invalid' });
    expect(all.name).toBe('Use up to 64 characters.');
    expect(all.address).toBe('This is one of your addresses.');
    expect(all.note).toBe('Remove line breaks and special characters.');
    expect(contactErrorMessages({ name: 'empty', address: 'invalid' })).toEqual({ name: 'Enter a name.', address: 'Enter a valid MMX address.' });
    expect(contactErrorMessages({ address: 'duplicate' }, A).address).toBe('This address is already saved.');
  });
});

describe('settings text', () => {
  it('lists funded addresses with 1-based numbers', () => {
    expect(fundedMessage([])).toBe('');
    expect(fundedMessage([11])).toBe('Address 12 holds funds.');
    expect(fundedMessage([10, 11])).toBe('Addresses 11 and 12 hold funds.');
    expect(fundedMessage([10, 11, 13])).toBe('Addresses 11, 12 and 14 hold funds.');
    expect(fundedMessage([1, 2, 3, 4, 5, 6, 7])).toBe('Addresses 2, 3, 4, 5, 6 and 2 more hold funds.');
  });

  it('labels auto-lock delays', () => {
    expect(autoLockLabel(0)).toBe('Never');
    expect(autoLockLabel(5)).toBe('5 minutes');
    expect(autoLockLabel(60)).toBe('1 hour');
    expect(autoLockLabel(120)).toBe('2 hours');
    expect(autoLockLabel(1)).toBe('1 minute');
  });

  it('flags unencrypted remote servers only', () => {
    expect(isUnencryptedRemote('http://rpc.example.org')).toBe(true);
    expect(isUnencryptedRemote('http://192.168.1.4:11380')).toBe(true);
    expect(isUnencryptedRemote('https://rpc.mmx.network')).toBe(false);
    expect(isUnencryptedRemote('http://localhost:11380')).toBe(false);
    expect(isUnencryptedRemote('http://127.0.0.1:11380')).toBe(false);
    expect(isUnencryptedRemote('http://[::1]:11380')).toBe(false);
    expect(isUnencryptedRemote('not a url')).toBe(false);
  });
});
