import { useEffect, useRef, useState } from 'preact/hooks';
import { Button, type ButtonVariant } from './Button';
import { IconButton } from './IconButton';

export interface CopyButtonProps {
  /** Performs the copy (e.g. backend.copyText). A rejected promise shows nothing. */
  onCopy: () => void | Promise<unknown>;
  label?: string;
  copiedLabel?: string;
  /** icon: compact icon button (default); button: labelled button. */
  variant?: 'icon' | 'button';
  buttonVariant?: ButtonVariant;
  size?: 'sm' | 'md';
  disabled?: boolean;
  class?: string;
}

const RESET_MS = 1600;

export function CopyButton({
  onCopy,
  label = 'Copy',
  copiedLabel = 'Copied',
  variant = 'icon',
  buttonVariant = 'secondary',
  size = 'md',
  disabled,
  class: className,
}: CopyButtonProps) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current);
    },
    [],
  );

  const run = async (): Promise<void> => {
    try {
      await onCopy();
    } catch {
      return;
    }
    setCopied(true);
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(() => setCopied(false), RESET_MS);
  };

  const status = (
    <span class="visually-hidden" role="status">
      {copied ? copiedLabel : ''}
    </span>
  );

  if (variant === 'button') {
    return (
      <>
        <Button
          variant={buttonVariant}
          size={size}
          icon={copied ? 'check' : 'copy'}
          class={['copy-btn', copied ? 'copy-btn--copied' : '', className ?? ''].filter(Boolean).join(' ')}
          disabled={disabled}
          onClick={run}
        >
          {copied ? copiedLabel : label}
        </Button>
        {status}
      </>
    );
  }
  return (
    <>
      <IconButton
        icon={copied ? 'check' : 'copy'}
        label={copied ? copiedLabel : label}
        size={size}
        class={['copy-btn', copied ? 'copy-btn--copied' : '', className ?? ''].filter(Boolean).join(' ')}
        disabled={disabled}
        onClick={run}
      />
      {status}
    </>
  );
}
