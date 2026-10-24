import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { WORDS, isWord, parseWordList } from '../../src/lib/wordlist';

const here = (p: string) => fileURLToPath(new URL(p, import.meta.url));
const listText = () => readFileSync(here('../../src/lib/english.txt'), 'utf8');

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

describe('word list line endings', () => {
  afterEach(() => {
    vi.doUnmock('../../src/lib/english.txt?raw');
    vi.resetModules();
  });

  it('reads the same words from LF, CRLF and CR files', () => {
    const lf = listText();
    expect(lf).not.toContain('\r');
    const words = parseWordList(lf);
    expect(words).toEqual(WORDS);
    expect(parseWordList(lf.replace(/\n/g, '\r\n'))).toEqual(words);
    expect(parseWordList(lf.replace(/\n/g, '\r'))).toEqual(words);
    expect(parseWordList(`﻿${lf.replace(/\n/g, '\r\n')}\r\n\r\n`)).toEqual(words);
  });

  it('accepts recovery phrases when the list was checked out with CRLF (Windows)', async () => {
    const crlf = listText().replace(/\n/g, '\r\n');
    vi.resetModules();
    vi.doMock('../../src/lib/english.txt?raw', () => ({ default: crlf }));
    const wordlist = await import('../../src/lib/wordlist');
    const mnemonic = await import('../../src/lib/mnemonic');

    expect(wordlist.WORDS).toHaveLength(2048);
    expect(wordlist.WORDS.some((w) => w.includes('\r'))).toBe(false);
    expect(wordlist.isWord('abandon')).toBe(true);
    expect(mnemonic.wordState('abandon')).toBe('valid');
    expect(mnemonic.completeWord('aban')).toBe('abandon');
    expect(mnemonic.allWordsValid([...Array<string>(23).fill('abandon'), 'art'])).toBe(true);
    expect(mnemonic.suggestWords('zo')).toContain('zoo');
  });
});

describe('repository line endings', () => {
  it('checks text files out with LF on every system', () => {
    const rules = readFileSync(here('../../../../.gitattributes'), 'utf8')
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter((line) => line !== '' && !line.startsWith('#'));
    expect(rules).toContain('* text=auto eol=lf');
    expect(rules.filter((rule) => rule.includes('eol=crlf'))).toEqual([]);
  });
});
