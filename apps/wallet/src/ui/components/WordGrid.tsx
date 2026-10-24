import { Button } from './Button';
import { cx } from './internal/cx';

export interface WordGridProps {
  /** Recovery words in order. Not rendered at all while `hidden`. */
  words: string[];
  /** Shows placeholders and a reveal button instead of the words. */
  hidden?: boolean;
  onReveal?: () => void;
  revealLabel?: string;
  /** Number shown for the first word (default 1). */
  start?: number;
  class?: string;
}

const PLACEHOLDER_WIDTHS = ['62%', '78%', '54%', '70%', '84%', '58%'];

/** Numbered recovery-phrase grid. Selection is disabled; offer an explicit copy action instead. */
export function WordGrid({ words, hidden = false, onReveal, revealLabel = 'Show words', start = 1, class: className }: WordGridProps) {
  return (
    <div class={cx('word-grid', hidden && 'word-grid--hidden', className)}>
      <ol class="word-grid__list" translate={false} aria-hidden={hidden ? 'true' : undefined}>
        {words.map((word, i) => (
          <li key={i} class="word-grid__item">
            <span class="word-grid__index">{start + i}</span>
            {hidden ? (
              <span class="word-grid__placeholder" style={{ width: PLACEHOLDER_WIDTHS[i % PLACEHOLDER_WIDTHS.length] }} />
            ) : (
              <span class="word-grid__word">{word}</span>
            )}
          </li>
        ))}
      </ol>
      {hidden && onReveal ? (
        <div class="word-grid__overlay">
          <Button variant="secondary" icon="eye" class="word-grid__reveal" onClick={onReveal}>
            {revealLabel}
          </Button>
        </div>
      ) : null}
    </div>
  );
}
