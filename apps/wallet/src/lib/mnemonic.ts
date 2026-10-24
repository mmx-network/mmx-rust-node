/**
 * Helpers for entering recovery phrases. Checksum and seed validation are done
 * by the wallet core (backend); this only handles words.
 */
import { WORDS, isWord } from './wordlist';

export const MNEMONIC_WORDS = 24;

/** Lowercases and strips everything but a–z from a single word. */
export function normalizeWord(word: string): string {
  return word
    .normalize('NFKD')
    .toLowerCase()
    .replace(/[^a-z]/g, '');
}

/**
 * Splits pasted or typed text into words. Handles any whitespace, commas,
 * semicolons and numbered lists ("1. abandon 2) ability", "1:abandon").
 */
export function splitWords(text: string): string[] {
  return text
    .split(/[\s,;]+/u)
    .map((token) => normalizeWord(token.replace(/^\d+[.):-]?/, '')))
    .filter((w) => w.length > 0);
}

/** Single-space, lowercase phrase from a list of words. */
export function joinWords(words: readonly string[]): string {
  return words.map(normalizeWord).filter(Boolean).join(' ');
}

/** Up to `limit` list words starting with the prefix (empty prefix → none). */
export function suggestWords(prefix: string, limit = 4): string[] {
  const p = normalizeWord(prefix);
  if (!p) return [];
  const out: string[] = [];
  for (const w of WORDS) {
    if (w.startsWith(p)) {
      out.push(w);
      if (out.length >= limit) break;
    }
  }
  return out;
}

/**
 * The single list word matching a prefix of at least 3 letters, if exactly
 * one exists (BIP39 words are unique in their first four letters).
 */
export function completeWord(prefix: string): string | null {
  const p = normalizeWord(prefix);
  if (p.length < 3) return null;
  if (isWord(p)) return p;
  const matches = suggestWords(p, 2);
  return matches.length === 1 ? matches[0]! : null;
}

export type WordState = 'empty' | 'valid' | 'partial' | 'invalid';

/** Validation state of one input box: partial = a prefix of some list word. */
export function wordState(word: string): WordState {
  const w = normalizeWord(word);
  if (!w) return 'empty';
  if (isWord(w)) return 'valid';
  return suggestWords(w, 1).length > 0 ? 'partial' : 'invalid';
}

/** True when all 24 words are list words (the checksum is verified by the backend). */
export function allWordsValid(words: readonly string[]): boolean {
  return words.length === MNEMONIC_WORDS && words.every((w) => isWord(normalizeWord(w)));
}

/**
 * Picks `count` distinct positions (0-based, sorted) to confirm a newly shown
 * phrase, using a cryptographic RNG.
 */
export function pickVerificationPositions(count = 3, total = MNEMONIC_WORDS): number[] {
  const chosen = new Set<number>();
  while (chosen.size < Math.min(count, total)) chosen.add(randomInt(total));
  return [...chosen].sort((a, b) => a - b);
}

/**
 * Options for confirming the word at one position: the correct word plus
 * `decoys` other list words not in the phrase, shuffled.
 */
export function verificationOptions(phraseWords: readonly string[], position: number, decoys = 5): string[] {
  const correct = phraseWords[position];
  if (correct === undefined) throw new RangeError('position out of range');
  const used = new Set(phraseWords);
  const options = new Set<string>([correct]);
  while (options.size < decoys + 1) {
    const w = WORDS[randomInt(WORDS.length)]!;
    if (!used.has(w)) options.add(w);
  }
  return shuffle([...options]);
}

/** Uniform random integer in [0, max) from crypto.getRandomValues. */
export function randomInt(max: number): number {
  if (!Number.isInteger(max) || max <= 0 || max > 0x1_0000_0000) throw new RangeError('invalid range');
  const limit = Math.floor(0x1_0000_0000 / max) * max;
  const buf = new Uint32Array(1);
  for (;;) {
    crypto.getRandomValues(buf);
    if (buf[0]! < limit) return buf[0]! % max;
  }
}

export function shuffle<T>(items: T[]): T[] {
  const a = items.slice();
  for (let i = a.length - 1; i > 0; i--) {
    const j = randomInt(i + 1);
    [a[i], a[j]] = [a[j]!, a[i]!];
  }
  return a;
}
