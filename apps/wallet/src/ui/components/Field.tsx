import { createContext, type ComponentChildren } from 'preact';
import { useCallback, useContext, useId, useLayoutEffect, useState } from 'preact/hooks';
import { cx } from './internal/cx';

export interface FieldContextValue {
  /** id for the control (the <label> points at it). */
  id: string;
  /** id of the visible label, for controls labelled with aria-labelledby. */
  labelId: string;
  /** ids of the hint / error text, for aria-describedby. */
  describedBy?: string;
  invalid: boolean;
  /** Called by grouped controls (radio groups) so the label is not tied to a single element. */
  registerGroup?: () => void;
}

export const FieldContext = createContext<FieldContextValue | null>(null);

/** Resolves id / aria-describedby / aria-invalid for a control rendered inside <Field>. */
export function useFieldControl(props: { id?: string; 'aria-describedby'?: string; invalid?: boolean }) {
  const field = useContext(FieldContext);
  const describedBy = [field?.describedBy, props['aria-describedby']].filter(Boolean).join(' ') || undefined;
  return {
    id: props.id ?? field?.id,
    labelId: field?.labelId,
    describedBy,
    invalid: Boolean(props.invalid ?? field?.invalid),
  };
}

/** For controls made of several elements: labels them via aria-labelledby instead of <label for>. */
export function useFieldGroup() {
  const field = useContext(FieldContext);
  useLayoutEffect(() => field?.registerGroup?.(), [field?.registerGroup]);
  return {
    labelId: field?.labelId,
    describedBy: field?.describedBy,
    invalid: Boolean(field?.invalid),
  };
}

export interface FieldProps {
  label: ComponentChildren;
  /** One short line under the control. Hidden while an error is shown. */
  hint?: ComponentChildren;
  /** Error text; marks the control invalid. */
  error?: ComponentChildren | null | false;
  /** Shown at the end of the label row, e.g. a character counter. */
  counter?: ComponentChildren;
  /** Element at the end of the label row (e.g. a small "Paste" button). */
  labelAction?: ComponentChildren;
  /** Adds a quiet "Optional" marker next to the label. */
  optional?: boolean;
  /** Keeps the label for assistive technology only. */
  hideLabel?: boolean;
  /** Explicit control id (defaults to a generated one). */
  id?: string;
  class?: string;
  children: ComponentChildren;
}

export function Field({
  label,
  hint,
  error,
  counter,
  labelAction,
  optional = false,
  hideLabel = false,
  id,
  class: className,
  children,
}: FieldProps) {
  const auto = useId();
  const [group, setGroup] = useState(false);
  const registerGroup = useCallback(() => setGroup(true), []);
  const controlId = id ?? `f${auto}`;
  const labelId = `${controlId}-label`;
  const messageId = `${controlId}-msg`;
  const hasError = error != null && error !== false && error !== '';
  const hasMessage = hasError || (hint != null && hint !== false && hint !== '');
  const value: FieldContextValue = {
    id: controlId,
    labelId,
    describedBy: hasMessage ? messageId : undefined,
    invalid: hasError,
    registerGroup,
  };
  return (
    <div class={cx('field', hasError && 'field--invalid', className)}>
      <div class={cx('field__top', hideLabel && 'visually-hidden')}>
        {group ? (
          <span class="field__label" id={labelId}>
            {label}
            {optional ? <span class="field__optional">Optional</span> : null}
          </span>
        ) : (
          <label class="field__label" id={labelId} for={controlId}>
            {label}
            {optional ? <span class="field__optional">Optional</span> : null}
          </label>
        )}
        {counter != null || labelAction != null ? (
          <span class="field__aside">
            {counter != null ? <span class="field__counter">{counter}</span> : null}
            {labelAction}
          </span>
        ) : null}
      </div>
      <FieldContext.Provider value={value}>{children}</FieldContext.Provider>
      <div class="field__message" id={messageId} aria-live="polite">
        {hasError ? <span class="field__error">{error}</span> : hasMessage ? <span class="field__hint">{hint}</span> : null}
      </div>
    </div>
  );
}
