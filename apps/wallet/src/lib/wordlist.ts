/** BIP39 English word list, identical to crates/mmx-wallet/src/english.txt. */
import english from './english.txt?raw';

/**
 * Words of a word list file, one per line. Any line ending is accepted: a
 * Windows checkout may turn the file's LF into CRLF, and a word must never
 * keep the '\r'.
 */
export function parseWordList(text: string): string[] {
  return text
    .split(/\r\n|\r|\n/)
    .map((line) => line.trim())
    .filter((word) => word.length > 0);
}

export const WORDS: readonly string[] = Object.freeze(parseWordList(english));

const INDEX = new Map<string, number>(WORDS.map((w, i) => [w, i]));

export function isWord(word: string): boolean {
  return INDEX.has(word);
}
