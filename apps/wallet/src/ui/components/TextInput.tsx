import type { ComponentChildren, InputHTMLAttributes, Ref } from 'preact';
import { useRef } from 'preact/hooks';
import { useFieldControl } from './Field';
import { cx } from './internal/cx';

export interface TextInputProps
  extends Omit<InputHTMLAttributes<HTMLInputElement>, 'value' | 'size' | 'class' | 'className' | 'id' | 'aria-describedby'> {
  id?: string;
  'aria-describedby'?: string;
  value: string;
  /** Called on input and change (including password manager autofill). */
  onValue?: (value: string) => void;
  /** Content before the text (icon, prefix). */
  leading?: ComponentChildren;
  /** Content after the text (unit, small button). */
  trailing?: ComponentChildren;
  /** Monospace text, for addresses and other codes. */
  mono?: boolean;
  /** Turns off spellcheck, autocorrect, autocapitalize and translation (secrets, codes). */
  sensitive?: boolean;
  /** Forces the invalid style (normally derived from the enclosing Field's error). */
  invalid?: boolean;
  size?: 'md' | 'lg';
  inputRef?: Ref<HTMLInputElement>;
  /** Class for the outer wrapper. */
  class?: string;
}

export function TextInput({
  value,
  onValue,
  onInput,
  onChange,
  leading,
  trailing,
  mono = false,
  sensitive = false,
  invalid,
  size = 'md',
  inputRef,
  class: className,
  id,
  disabled,
  readOnly,
  type = 'text',
  'aria-describedby': ariaDescribedBy,
  ...rest
}: TextInputProps) {
  const field = useFieldControl({ id, 'aria-describedby': ariaDescribedBy, invalid });
  const local = useRef<HTMLInputElement | null>(null);
  const setRef = (el: HTMLInputElement | null): void => {
    local.current = el;
    if (el && sensitive) el.setAttribute('autocorrect', 'off');
    if (typeof inputRef === 'function') inputRef(el);
    else if (inputRef) inputRef.current = el;
  };
  // autocorrect is set as an attribute: WebKit exposes it as a boolean
  // property, so a string value would turn it on.
  const secretProps = sensitive ? { spellcheck: false, autocomplete: 'off', autocapitalize: 'off' as const, translate: false } : {};
  return (
    <div
      class={cx(
        'input',
        `input--${size}`,
        mono && 'input--mono',
        trailing != null && 'input--trailing',
        field.invalid && 'input--invalid',
        disabled && 'input--disabled',
        readOnly && 'input--readonly',
        className,
      )}
      onMouseDown={(event) => {
        if (event.target === event.currentTarget && local.current) {
          event.preventDefault();
          local.current.focus();
        }
      }}
    >
      {leading != null ? <span class="input__adornment input__adornment--leading">{leading}</span> : null}
      <input
        {...secretProps}
        {...rest}
        ref={setRef}
        id={field.id}
        type={type}
        class="input__control"
        value={value}
        disabled={disabled}
        readOnly={readOnly}
        aria-invalid={field.invalid ? 'true' : undefined}
        aria-describedby={field.describedBy}
        onInput={(event) => {
          onInput?.(event);
          onValue?.(event.currentTarget.value);
        }}
        onChange={(event) => {
          onChange?.(event);
          onValue?.(event.currentTarget.value);
        }}
      />
      {trailing != null ? <span class="input__adornment input__adornment--trailing">{trailing}</span> : null}
    </div>
  );
}
