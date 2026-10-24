/**
 * Desktop: confirms a new recovery phrase by picking the right word for three
 * random positions from shuffled options. A wrong pick gets new options.
 */
import { useId, useRef, useState } from 'preact/hooks';
import { pickVerificationPositions, verificationOptions } from '../../../lib/mnemonic';
import { Banner, Button } from '../../components';
import { cx } from '../../components/internal/cx';
import './create.css';

export interface ConfirmWordsProps {
  words: string[];
  busy: boolean;
  error?: string;
  onConfirmed: () => void;
  onShowWords: () => void;
}

interface Challenge {
  position: number;
  options: string[];
}

function newChallenges(words: string[]): Challenge[] {
  return pickVerificationPositions(3, words.length).map((position) => ({
    position,
    options: verificationOptions(words, position),
  }));
}

export function ConfirmWords({ words, busy, error, onConfirmed, onShowWords }: ConfirmWordsProps) {
  const [challenges, setChallenges] = useState<Challenge[]>(() => newChallenges(words));
  const [picks, setPicks] = useState<Record<number, string>>({});
  const [wrong, setWrong] = useState<number[]>([]);
  const [message, setMessage] = useState<string | undefined>(undefined);
  const groupRefs = useRef<Record<number, HTMLFieldSetElement | null>>({});
  const auto = useId();

  const focusGroup = (position: number): void => {
    groupRefs.current[position]?.querySelector<HTMLInputElement>('input')?.focus();
  };

  const pick = (position: number, word: string): void => {
    setPicks((p) => ({ ...p, [position]: word }));
    setWrong((w) => w.filter((x) => x !== position));
    setMessage(undefined);
  };

  const submit = (event: Event): void => {
    event.preventDefault();
    if (busy) return;
    const missing = challenges.filter((c) => !picks[c.position]);
    if (missing.length > 0) {
      setMessage('Pick a word for each number.');
      focusGroup(missing[0]!.position);
      return;
    }
    const bad = challenges.filter((c) => picks[c.position] !== words[c.position]).map((c) => c.position);
    if (bad.length > 0) {
      setWrong(bad);
      setChallenges((list) =>
        list.map((c) => (bad.includes(c.position) ? { position: c.position, options: verificationOptions(words, c.position) } : c)),
      );
      setPicks((p) => {
        const next = { ...p };
        for (const position of bad) delete next[position];
        return next;
      });
      setMessage(
        bad.length === 1
          ? `Word ${bad[0]! + 1} doesn’t match. Check your list and try again.`
          : 'Some words don’t match. Check your list and try again.',
      );
      focusGroup(bad[0]!);
      return;
    }
    onConfirmed();
  };

  return (
    <form class="onboarding-form create-confirm" noValidate onSubmit={submit}>
      {challenges.map((c) => {
        const legendId = `${auto}-l${c.position}`;
        const isWrong = wrong.includes(c.position);
        return (
          <fieldset
            key={c.position}
            ref={(el) => {
              groupRefs.current[c.position] = el;
            }}
            class={cx('create-pick', isWrong && 'create-pick--wrong')}
            aria-labelledby={legendId}
            disabled={busy}
          >
            <legend id={legendId} class="create-pick__legend">
              Word {c.position + 1}
            </legend>
            <div class="create-pick__options">
              {c.options.map((word) => {
                const checked = picks[c.position] === word;
                return (
                  <label key={word} class={cx('create-pick__option', checked && 'create-pick__option--checked')}>
                    <input
                      type="radio"
                      class="create-pick__input"
                      name={`${auto}-word-${c.position}`}
                      value={word}
                      checked={checked}
                      onChange={() => pick(c.position, word)}
                    />
                    <span class="create-pick__label" translate={false}>
                      {word}
                    </span>
                  </label>
                );
              })}
            </div>
          </fieldset>
        );
      })}
      <p class="create-confirm__message" role="alert">
        {message ?? ''}
      </p>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <div class="onboarding-actions">
        <Button type="submit" variant="primary" size="lg" block loading={busy}>
          Confirm
        </Button>
        <div class="onboarding-actions__secondary">
          <Button variant="ghost" onClick={onShowWords} disabled={busy}>
            Show words again
          </Button>
        </div>
      </div>
    </form>
  );
}
