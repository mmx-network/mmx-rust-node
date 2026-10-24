/** Recovery word entry rules for the import screen (no UI). */
import { MNEMONIC_WORDS, wordState } from '../../../lib/mnemonic';

/** Fills `list` into the boxes from `index` (a full phrase always starts at the first box). */
export function fillWords(
  current: readonly string[],
  index: number,
  list: readonly string[],
): { words: string[]; start: number; end: number } {
  const words = current.slice();
  const start = list.length >= MNEMONIC_WORDS ? 0 : Math.max(0, Math.min(index, MNEMONIC_WORDS - 1));
  const take = list.slice(0, MNEMONIC_WORDS - start);
  take.forEach((w, k) => {
    words[start + k] = w;
  });
  return { words, start, end: start + take.length };
}

/** Indices whose content is shown as a problem. */
export function problemIndices(
  words: readonly string[],
  touched: readonly boolean[],
  focused: number,
  showErrors: boolean,
): number[] {
  const out: number[] = [];
  words.forEach((word, i) => {
    const state = wordState(word);
    if (state === 'invalid' || (state === 'partial' && i !== focused && (touched[i] || showErrors))) out.push(i);
  });
  return out;
}

/** The box to go to after a failed submit: the first problem, else the first empty box (-1 = none). */
export function firstToFix(words: readonly string[]): number {
  const bad = words.findIndex((w) => w !== '' && wordState(w) !== 'valid');
  return bad >= 0 ? bad : words.findIndex((w) => !w);
}

/** One line describing what to fix, or undefined. */
export function wordsMessage(words: readonly string[], problems: readonly number[], showErrors: boolean): string | undefined {
  if (problems.length === 1) return `Word ${problems[0]! + 1} isn’t on the word list.`;
  if (problems.length > 1) {
    const numbers = problems.map((i) => String(i + 1));
    return `Words ${new Intl.ListFormat('en', { type: 'conjunction' }).format(numbers)} aren’t on the word list.`;
  }
  if (showErrors && words.some((w) => !w)) return `Enter all ${MNEMONIC_WORDS} words.`;
  return undefined;
}
