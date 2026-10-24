import { describe, expect, it } from 'vitest';
import {
  allWordsValid,
  completeWord,
  joinWords,
  normalizeWord,
  pickVerificationPositions,
  randomInt,
  shuffle,
  splitWords,
  suggestWords,
  verificationOptions,
  wordState,
} from '../../src/lib/mnemonic';
import { WORDS } from '../../src/lib/wordlist';

const PHRASE =
  'cactus wedding mansion deal random arrive gate tissue bench party setup captain abstract mandate crouch lift anxiety embark achieve army expect account amount claim';

describe('word input', () => {
  it('splits pasted phrases in any common format', () => {
    expect(splitWords(PHRASE)).toHaveLength(24);
    expect(splitWords(`  ${PHRASE.toUpperCase().replace(/ /g, '\n\t ')}  `)).toEqual(PHRASE.split(' '));
    expect(splitWords('1. cactus 2. wedding 3) mansion 4:deal')).toEqual(['cactus', 'wedding', 'mansion', 'deal']);
    expect(splitWords('cactus,wedding;mansion')).toEqual(['cactus', 'wedding', 'mansion']);
    expect(splitWords('1 cactus 2 wedding')).toEqual(['cactus', 'wedding']);
    expect(splitWords('')).toEqual([]);
  });

  it('normalizes words', () => {
    expect(normalizeWord(' Abandon! ')).toBe('abandon');
    expect(normalizeWord('café')).toBe('cafe');
    expect(joinWords(['Cactus', ' wedding ', ''])).toBe('cactus wedding');
  });

  it('suggests and completes words', () => {
    expect(suggestWords('ab')).toEqual(['abandon', 'ability', 'able', 'about']);
    expect(suggestWords('zoo')).toEqual(['zoo']);
    expect(suggestWords('')).toEqual([]);
    expect(suggestWords('qqq')).toEqual([]);
    expect(completeWord('aban')).toBe('abandon');
    expect(completeWord('ab')).toBeNull();
    expect(completeWord('abo')).toBeNull();
    expect(completeWord('zoo')).toBe('zoo');
    expect(completeWord('wedd')).toBe('wedding');
    expect(completeWord('xyzzy')).toBeNull();
  });

  it('reports word states', () => {
    expect(wordState('')).toBe('empty');
    expect(wordState('cactus')).toBe('valid');
    expect(wordState('CACTUS')).toBe('valid');
    expect(wordState('cac')).toBe('partial');
    expect(wordState('cactusx')).toBe('invalid');
  });

  it('checks complete phrases', () => {
    const words = PHRASE.split(' ');
    expect(allWordsValid(words)).toBe(true);
    expect(allWordsValid(words.slice(1))).toBe(false);
    const broken = [...words];
    broken[3] = 'nope';
    broken[7] = '';
    expect(allWordsValid(broken)).toBe(false);
  });
});

describe('verification', () => {
  it('picks distinct sorted positions', () => {
    for (let i = 0; i < 100; i++) {
      const p = pickVerificationPositions(3);
      expect(p).toHaveLength(3);
      expect(new Set(p).size).toBe(3);
      expect([...p].sort((a, b) => a - b)).toEqual(p);
      expect(p.every((x) => x >= 0 && x < 24)).toBe(true);
    }
  });

  it('offers the right word among decoys from the list but not from the phrase', () => {
    const words = PHRASE.split(' ');
    for (let pos = 0; pos < 24; pos++) {
      const options = verificationOptions(words, pos);
      expect(options).toHaveLength(6);
      expect(new Set(options).size).toBe(6);
      expect(options).toContain(words[pos]);
      for (const o of options) {
        expect(WORDS).toContain(o);
        if (o !== words[pos]) expect(words).not.toContain(o);
      }
    }
    expect(() => verificationOptions(words, 24)).toThrow();
  });

  it('draws uniform integers in range', () => {
    const counts = new Array(6).fill(0);
    for (let i = 0; i < 6000; i++) counts[randomInt(6)]++;
    for (const c of counts) expect(c).toBeGreaterThan(800);
    expect(() => randomInt(0)).toThrow();
    expect(() => randomInt(1.5)).toThrow();
    expect(randomInt(1)).toBe(0);
  });

  it('shuffles without losing items', () => {
    const items = Array.from({ length: 50 }, (_, i) => i);
    const s = shuffle(items);
    expect([...s].sort((a, b) => a - b)).toEqual(items);
    expect(items[0]).toBe(0);
  });
});
