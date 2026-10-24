/**
 * Web: saves a wallet in the password manager. A real sign-up form (username =
 * wallet name and ID, password = recovery words); on submit the browser is
 * asked to store it (Chromium) and the caller moves on, which removes the form
 * so Safari and password manager extensions offer to save it.
 */
import type { ComponentChildren } from 'preact';
import { useRef, useState } from 'preact/hooks';
import { copyToClipboard, saveWalletCredential } from '../../../state';
import { Button, CopyButton, Field, PasswordInput, TextInput } from '../../components';
import { useMounted } from '../welcome/flow';
import './create.css';

export interface SaveCredentialProps {
  username: string;
  /** Recovery words, lowercase, single spaces. */
  words: string;
  submitLabel: string;
  /** Called after the save request; the caller replaces this form. */
  onSaved: () => void;
  /** Content below the submit button. */
  children?: ComponentChildren;
}

export function SaveCredential({ username, words, submitLabel, onSaved, children }: SaveCredentialProps) {
  const [busy, setBusy] = useState(false);
  const saving = useRef(false);
  const wordsRef = useRef<HTMLInputElement>(null);
  const mounted = useMounted();

  const submit = async (event: Event): Promise<void> => {
    event.preventDefault();
    if (saving.current) return;
    saving.current = true;
    setBusy(true);
    try {
      await saveWalletCredential(username, words);
    } finally {
      saving.current = false;
    }
    if (!mounted.current) return;
    setBusy(false);
    onSaved();
  };

  const copy = (text: string, sensitive: boolean) => async (): Promise<void> => {
    const ok = await copyToClipboard(text, { sensitive });
    if (!ok) throw new Error('copy failed');
  };

  return (
    <form class="onboarding-form create-save" method="post" action="#" onSubmit={(e) => void submit(e)}>
      <Field label="Wallet" labelAction={<CopyButton size="sm" label="Copy wallet" onCopy={copy(username, false)} />}>
        <TextInput name="username" autocomplete="username" value={username} readOnly class="create-save__username" />
      </Field>
      <Field label="Recovery phrase" labelAction={<CopyButton size="sm" label="Copy recovery phrase" onCopy={copy(words, true)} />}>
        <PasswordInput
          name="password"
          autocomplete="new-password"
          value={words}
          inputRef={wordsRef}
          revealable={false}
          // The saved value must stay exactly the words: undo any edit.
          onValue={() => {
            if (wordsRef.current && wordsRef.current.value !== words) wordsRef.current.value = words;
          }}
        />
      </Field>
      <div class="onboarding-actions">
        <Button type="submit" variant="primary" size="lg" block icon="key" loading={busy}>
          {submitLabel}
        </Button>
        {children}
      </div>
    </form>
  );
}
