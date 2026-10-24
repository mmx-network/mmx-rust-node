import { useEffect, useRef, useState } from 'preact/hooks';
import { IconButton } from './IconButton';
import { TextInput, type TextInputProps } from './TextInput';

export interface PasswordInputProps extends Omit<TextInputProps, 'type' | 'trailing' | 'sensitive'> {
  /** Shows the reveal toggle (default true). */
  revealable?: boolean;
}

/**
 * Password field with a reveal toggle. All other attributes (name, autocomplete,
 * data-* hints for password managers) are passed to the <input>. The field
 * switches back to type="password" right before its form submits so password
 * managers always see a password field.
 */
export function PasswordInput({ revealable = true, inputRef, mono = false, ...rest }: PasswordInputProps) {
  const [revealed, setRevealed] = useState(false);
  const local = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    const onSubmit = (event: Event): void => {
      const input = local.current;
      if (input && input.form && event.target === input.form) {
        input.type = 'password';
        setRevealed(false);
      }
    };
    window.addEventListener('submit', onSubmit, true);
    return () => window.removeEventListener('submit', onSubmit, true);
  }, []);

  const setRef = (el: HTMLInputElement | null): void => {
    local.current = el;
    if (typeof inputRef === 'function') inputRef(el);
    else if (inputRef) inputRef.current = el;
  };

  return (
    <TextInput
      {...rest}
      mono={mono}
      sensitive
      inputRef={setRef}
      type={revealed ? 'text' : 'password'}
      trailing={
        revealable ? (
          <IconButton
            icon={revealed ? 'eye-off' : 'eye'}
            label={revealed ? 'Hide' : 'Show'}
            size="sm"
            class="input__reveal"
            aria-pressed={revealed ? 'true' : 'false'}
            disabled={rest.disabled}
            onClick={() => {
              setRevealed((v) => !v);
              local.current?.focus();
            }}
          />
        ) : undefined
      }
    />
  );
}
