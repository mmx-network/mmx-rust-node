import { cx } from './internal/cx';

export interface ProgressStepsProps {
  steps: string[];
  /** Index of the current step (0-based). */
  current: number;
  /** Accessible name (default "Progress"). */
  label?: string;
  class?: string;
}

/** Wizard progress: "Step 2 of 4 · Passphrase" above a segmented bar. */
export function ProgressSteps({ steps, current, label = 'Progress', class: className }: ProgressStepsProps) {
  const index = Math.min(Math.max(current, 0), Math.max(steps.length - 1, 0));
  return (
    <div class={cx('steps', className)}>
      <p class="steps__meta" aria-hidden="true">
        <span class="steps__count">
          Step {index + 1} of {steps.length}
        </span>
        <span class="steps__current">{steps[index]}</span>
      </p>
      <ol class="steps__bars" aria-label={label}>
        {steps.map((step, i) => {
          const state = i < index ? 'done' : i === index ? 'current' : 'upcoming';
          return (
            <li key={i} class={cx('steps__item', `steps__item--${state}`)} aria-current={state === 'current' ? 'step' : undefined}>
              <span class="visually-hidden">
                {step}
                {state === 'done' ? ', done' : ''}
              </span>
            </li>
          );
        })}
      </ol>
    </div>
  );
}
