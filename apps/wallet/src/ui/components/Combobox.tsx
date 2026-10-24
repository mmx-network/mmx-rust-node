import type { ComponentChildren } from 'preact';
import { useEffect, useId, useRef, useState } from 'preact/hooks';
import { TextInput, type TextInputProps } from './TextInput';
import { cx } from './internal/cx';
import { useFloatingPosition, useOutsidePress } from './internal/floating';

export interface ComboboxOption {
  /** Stable key; passed back through onSelect. */
  id: string;
  label: ComponentChildren;
  description?: ComponentChildren;
  leading?: ComponentChildren;
  /** Right-aligned detail. */
  trailing?: ComponentChildren;
}

export interface ComboboxProps extends Omit<TextInputProps, 'role' | 'onSelect'> {
  /** Suggestions for the current text (filtered by the caller). Empty hides the list. */
  options: ComboboxOption[];
  onSelect: (option: ComboboxOption) => void;
  /** Accessible name of the suggestion list. */
  listLabel: string;
  /** Highlight the first suggestion so Enter picks it (word entry). Default false. */
  autoHighlight?: boolean;
}

/**
 * Text input with a suggestion list (ARIA combobox). The caller owns the text
 * and the filtering; ↑ ↓ move, Enter picks, Esc closes.
 */
export function Combobox({
  options,
  onSelect,
  listLabel,
  autoHighlight = false,
  value,
  onValue,
  onKeyDown,
  onFocus,
  onBlur,
  class: className,
  ...rest
}: ComboboxProps) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(autoHighlight ? 0 : -1);
  const rootRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLUListElement>(null);
  const auto = useId();
  const listId = `cb${auto}`;
  const optionId = (i: number): string => `${listId}-${i}`;
  const visible = open && options.length > 0;

  useFloatingPosition(visible, rootRef, panelRef, 'bottom-start', true);
  useOutsidePress(visible, [rootRef, panelRef], () => setOpen(false));

  useEffect(() => {
    setActive(autoHighlight && options.length > 0 ? 0 : -1);
  }, [options.length, autoHighlight, value]);

  useEffect(() => {
    if (!visible || active < 0) return;
    panelRef.current?.querySelector<HTMLElement>(`#${CSS.escape(optionId(active))}`)?.scrollIntoView({ block: 'nearest' });
  }, [active, visible]);

  const pick = (option: ComboboxOption): void => {
    setOpen(false);
    onSelect(option);
  };

  return (
    <div ref={rootRef} class={cx('combobox', className)}>
      <TextInput
        autocomplete="off"
        {...rest}
        value={value}
        role="combobox"
        aria-autocomplete="list"
        aria-expanded={visible ? 'true' : 'false'}
        aria-controls={visible ? listId : undefined}
        aria-activedescendant={visible && active >= 0 ? optionId(active) : undefined}
        onValue={(v) => {
          setOpen(true);
          onValue?.(v);
        }}
        onFocus={(event) => {
          setOpen(true);
          onFocus?.(event);
        }}
        onBlur={(event) => {
          setOpen(false);
          onBlur?.(event);
        }}
        onKeyDown={(event) => {
          if (visible) {
            if (event.key === 'ArrowDown') {
              event.preventDefault();
              setActive((i) => (i + 1) % options.length);
              return;
            }
            if (event.key === 'ArrowUp') {
              event.preventDefault();
              setActive((i) => (i <= 0 ? options.length - 1 : i - 1));
              return;
            }
            if (event.key === 'Enter' && active >= 0 && options[active]) {
              event.preventDefault();
              pick(options[active]!);
              return;
            }
            if (event.key === 'Escape') {
              event.preventDefault();
              event.stopPropagation();
              setOpen(false);
              return;
            }
            if (event.key === 'Tab') setOpen(false);
          } else if (event.key === 'ArrowDown' && options.length > 0) {
            event.preventDefault();
            setOpen(true);
            return;
          }
          onKeyDown?.(event);
        }}
      />
      {visible ? (
        <ul ref={panelRef} id={listId} class="combobox__list" role="listbox" aria-label={listLabel}>
          {options.map((o, i) => (
            <li
              key={o.id}
              id={optionId(i)}
              role="option"
              aria-selected={i === active ? 'true' : 'false'}
              class={cx('combobox__option', i === active && 'combobox__option--active')}
              onPointerDown={(event) => event.preventDefault()}
              onPointerMove={() => setActive(i)}
              onClick={() => pick(o)}
            >
              {o.leading != null ? <span class="combobox__leading">{o.leading}</span> : null}
              <span class="combobox__text">
                <span class="combobox__label">{o.label}</span>
                {o.description != null ? <span class="combobox__description">{o.description}</span> : null}
              </span>
              {o.trailing != null ? <span class="combobox__trailing">{o.trailing}</span> : null}
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}
