/** BIP39 English word list, identical to crates/mmx-wallet/src/english.txt. */
import english from './english.txt?raw';

export const WORDS: readonly string[] = Object.freeze(english.split('\n').filter((w) => w.length > 0));

const INDEX = new Map<string, number>(WORDS.map((w, i) => [w, i]));

export function isWord(word: string): boolean {
  return INDEX.has(word);
}
