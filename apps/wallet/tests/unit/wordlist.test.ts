import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { WORDS, isWord } from '../../src/lib/wordlist';

const here = (p: string) => fileURLToPath(new URL(p, import.meta.url));

describe('wordlist', () => {
  it('is byte-identical to the wallet core list', () => {
    const app = readFileSync(here('../../src/lib/english.txt'));
    const core = readFileSync(here('../../../../crates/mmx-wallet/src/english.txt'));
    expect(app.equals(core)).toBe(true);
  });

  it('has 2048 unique lowercase words in order', () => {
    expect(WORDS).toHaveLength(2048);
    expect(new Set(WORDS).size).toBe(2048);
    expect(WORDS[0]).toBe('abandon');
    expect(WORDS[2047]).toBe('zoo');
    expect(WORDS.every((w) => /^[a-z]{3,8}$/.test(w))).toBe(true);
    expect([...WORDS].sort()).toEqual(WORDS);
  });

  it('looks up words', () => {
    expect(isWord('abandon')).toBe(true);
    expect(isWord('Abandon')).toBe(false);
    expect(isWord('')).toBe(false);
    expect(WORDS[2047]).toBe('zoo');
  });
});
