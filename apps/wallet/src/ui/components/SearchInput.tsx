import { useRef } from 'preact/hooks';
import { Icon } from './Icon';
import { IconButton } from './IconButton';
import { TextInput, type TextInputProps } from './TextInput';

export interface SearchInputProps extends Omit<TextInputProps, 'type' | 'leading' | 'trailing' | 'onValue'> {
  onValue: (value: string) => void;
  /** Accessible name when there is no visible label. */
  label: string;
}

/** Search field with a clear button; Escape clears a non-empty value. */
export function SearchInput({ value, onValue, label, onKeyDown, inputRef, ...rest }: SearchInputProps) {
  const local = useRef<HTMLInputElement | null>(null);
  const setRef = (el: HTMLInputElement | null): void => {
    local.current = el;
    if (typeof inputRef === 'function') inputRef(el);
    else if (inputRef) inputRef.current = el;
  };
  return (
    <TextInput
      autocomplete="off"
      spellcheck={false}
      enterkeyhint="search"
      {...rest}
      class={['search-input', rest.class].filter(Boolean).join(' ')}
      type="search"
      aria-label={rest['aria-label'] ?? label}
      value={value}
      onValue={onValue}
      inputRef={setRef}
      leading={<Icon name="search" />}
      trailing={
        value ? (
          <IconButton
            icon="close"
            label="Clear"
            size="sm"
            onClick={() => {
              onValue('');
              local.current?.focus();
            }}
          />
        ) : undefined
      }
      onKeyDown={(event) => {
        onKeyDown?.(event);
        if (event.key === 'Escape' && value) {
          event.preventDefault();
          event.stopPropagation();
          onValue('');
        }
      }}
    />
  );
}
