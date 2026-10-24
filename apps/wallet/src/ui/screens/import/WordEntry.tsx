/**
 * 24 recovery word inputs with per-word validation and suggestions from the
 * word list. Pasting or typing a whole phrase into any box fills the boxes.
 */
import type { MutableRef } from 'preact/hooks';
import { useId, useRef, useState } from 'preact/hooks';
import { completeWord, MNEMONIC_WORDS, normalizeWord, splitWords, suggestWords } from '../../../lib/mnemonic';
import { fillWords, problemIndices, wordsMessage } from './words';
import { Combobox, type ComboboxOption } from '../../components';
import { cx } from '../../components/internal/cx';
import './import.css';

export interface WordEntryProps {
  words: string[];
  onWords: (words: string[]) => void;
  /** Show every problem (after a submit attempt), not only in boxes already left. */
  showErrors: boolean;
  disabled?: boolean;
  inputs: MutableRef<(HTMLInputElement | null)[]>;
  /** Enter in the last box ('enter'), or the last box filled by a pick or a paste ('filled'), with the newest words. */
  onDone: (words: string[], reason: 'enter' | 'filled') => void;
}

export function WordEntry({ words, onWords, showErrors, disabled = false, inputs, onDone }: WordEntryProps) {
  const [touched, setTouched] = useState<boolean[]>(() => words.map((w) => w !== ''));
  const [focused, setFocused] = useState(-1);
  // Handlers can run back to back before a re-render (pick, then blur): always work on the newest words.
  const latest = useRef(words);
  latest.current = words;
  const auto = useId();

  const commit = (next: string[]): void => {
    latest.current = next;
    onWords(next);
  };

  const setWord = (i: number, word: string): void => {
    if (latest.current[i] === word) return;
    const next = latest.current.slice();
    next[i] = word;
    commit(next);
  };

  const markTouched = (from: number, to = from + 1): void => {
    setTouched((t) => t.map((v, k) => v || (k >= from && k < to)));
  };

  const focusAt = (i: number, caretAtEnd = false): void => {
    const el = inputs.current[Math.max(0, Math.min(MNEMONIC_WORDS - 1, i))];
    if (!el) return;
    el.focus();
    if (caretAtEnd) el.setSelectionRange(el.value.length, el.value.length);
  };

  const advance = (i: number, reason: 'enter' | 'filled'): void => {
    if (i >= MNEMONIC_WORDS - 1) onDone(latest.current, reason);
    else focusAt(i + 1);
  };

  const fill = (i: number, list: string[]): void => {
    const { words: next, start, end } = fillWords(latest.current, i, list);
    commit(next);
    markTouched(start, end);
    const empty = next.findIndex((w, k) => k >= end && !w);
    const firstEmpty = empty >= 0 ? empty : next.findIndex((w) => !w);
    requestAnimationFrame(() => {
      if (firstEmpty >= 0) focusAt(firstEmpty);
      else onDone(latest.current, 'filled');
    });
  };

  const onInput = (i: number, value: string): void => {
    if (/\s/u.test(value)) {
      const list = splitWords(value);
      if (list.length > 1) {
        fill(i, list);
        return;
      }
      // A single word followed by a space: complete it and go to the next box.
      const word = list[0] ?? '';
      setWord(i, completeWord(word) ?? word);
      if (word) {
        markTouched(i);
        focusAt(i + 1);
      }
      return;
    }
    setWord(i, normalizeWord(value));
  };

  const finishWord = (i: number): void => {
    const word = latest.current[i] ?? '';
    const done = completeWord(word);
    if (done && done !== word) setWord(i, done);
  };

  const onKeyDown = (i: number, event: KeyboardEvent): void => {
    const word = latest.current[i] ?? '';
    if (event.key === ' ') {
      event.preventDefault();
      if (!word) return;
      finishWord(i);
      markTouched(i);
      focusAt(i + 1);
    } else if (event.key === 'Enter') {
      event.preventDefault();
      finishWord(i);
      markTouched(i);
      advance(i, 'enter');
    } else if (event.key === 'Backspace' && !word && i > 0) {
      event.preventDefault();
      focusAt(i - 1, true);
    }
  };

  const onPaste = (i: number, event: ClipboardEvent): void => {
    const text = event.clipboardData?.getData('text/plain') ?? '';
    const list = splitWords(text);
    if (list.length <= 1) return;
    event.preventDefault();
    fill(i, list);
  };

  const problems = problemIndices(words, touched, focused, showErrors);
  const message = wordsMessage(words, problems, showErrors);
  const messageId = `${auto}-msg`;

  return (
    <div class="import-words">
      <ol class="import-words__grid">
        {words.map((word, i) => {
          const invalid = problems.includes(i);
          let options: ComboboxOption[] = [];
          if (i === focused && word) {
            const list = suggestWords(word, 4);
            if (!(list.length === 1 && list[0] === word)) {
              options = list.map((w) => ({
                id: w,
                label: (
                  <span class="import-suggestion" translate={false}>
                    <span class="import-suggestion__typed">{w.slice(0, word.length)}</span>
                    {w.slice(word.length)}
                  </span>
                ),
              }));
            }
          }
          return (
            <li key={i} class={cx('import-word', invalid && 'import-word--invalid')}>
              <Combobox
                id={`${auto}-w${i}`}
                aria-label={`Word ${i + 1}`}
                aria-describedby={invalid && message ? messageId : undefined}
                value={word}
                onValue={(v) => onInput(i, v)}
                options={options}
                onSelect={(o) => {
                  setWord(i, o.id);
                  markTouched(i);
                  advance(i, 'filled');
                }}
                listLabel={`Suggestions for word ${i + 1}`}
                autoHighlight
                sensitive
                invalid={invalid}
                disabled={disabled}
                leading={
                  <span class="import-word__number" aria-hidden="true">
                    {i + 1}
                  </span>
                }
                inputRef={(el: HTMLInputElement | null) => {
                  inputs.current[i] = el;
                }}
                onFocus={() => setFocused(i)}
                onBlur={() => {
                  setFocused((f) => (f === i ? -1 : f));
                  finishWord(i);
                  if (latest.current[i]) markTouched(i);
                }}
                onKeyDown={(e) => onKeyDown(i, e)}
                onPaste={(e) => onPaste(i, e)}
                enterKeyHint={i === MNEMONIC_WORDS - 1 ? 'done' : 'next'}
                class="import-word__box"
              />
            </li>
          );
        })}
      </ol>
      <p id={messageId} class="import-words__message" aria-live="polite">
        {message ?? ''}
      </p>
    </div>
  );
}
