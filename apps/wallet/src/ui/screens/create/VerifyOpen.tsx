/**
 * Web: completes a new wallet. The recovery phrase is filled in again from
 * the password manager (or typed) and compared with the new words, which
 * proves the credential was saved. The wallet is stored only after that.
 */
import { useEffect, useRef, useState } from 'preact/hooks';
import { passwordManagerApi, requestWalletCredential } from '../../../state';
import { Banner, Button, Field, Identicon, PasswordInput, TextInput } from '../../components';
import { useInitialFocus, useMounted } from '../welcome/flow';
import { ENTER_WORDS, verifyProblem } from './details';
import '../unlock/unlock.css';

export interface VerifyOpenProps {
  /** Username saved with the credential ("<name> (<wallet ID>)"). */
  username: string;
  fingerprint: string;
  /** The new recovery words, lowercase, single spaces. */
  words: string;
  busy: boolean;
  /** Problem reported while storing the wallet. */
  error?: string;
  onVerified: () => void;
  onShowAgain: () => void;
}

export function VerifyOpen({ username, fingerprint, words, busy, error, onVerified, onShowAgain }: VerifyOpenProps) {
  const [value, setValue] = useState('');
  const [problem, setProblem] = useState<string | undefined>(undefined);
  const wordsRef = useRef<HTMLInputElement>(null);
  /** The browser's chooser is open. */
  const requesting = useRef(false);
  const mounted = useMounted();
  useInitialFocus(wordsRef);
  useEffect(() => {
    if (problem) wordsRef.current?.focus();
  }, [problem]);

  const check = (typed: string): void => {
    const found = verifyProblem(typed, words);
    setProblem(found);
    if (!found) onVerified();
  };

  const submit = (event: Event): void => {
    event.preventDefault();
    if (busy || requesting.current) return;
    // Read the input directly: password managers may fill it without input events.
    const typed = wordsRef.current?.value ?? value;
    if (typed.trim() || !passwordManagerApi()) {
      check(typed);
      return;
    }
    // Empty: offer the browser's saved wallets (Chromium).
    requesting.current = true;
    requestWalletCredential().then(
      (credential) => {
        requesting.current = false;
        if (!mounted.current) return;
        if (credential) check(credential.words);
        else setProblem(ENTER_WORDS);
      },
      () => {
        requesting.current = false;
        if (mounted.current) setProblem(ENTER_WORDS);
      },
    );
  };

  return (
    <form class="onboarding-form unlock-credentials" method="post" action="#" noValidate onSubmit={submit}>
      <Field label="Wallet">
        <TextInput
          name="username"
          autocomplete="username"
          value={username}
          readOnly
          leading={<Identicon value={fingerprint} size={24} />}
          class="unlock-credentials__wallet"
        />
      </Field>
      <Field label="Recovery phrase" hint="Use your password manager to fill this in." error={problem}>
        <PasswordInput
          name="password"
          autocomplete="current-password"
          value={value}
          onValue={(next) => {
            setValue(next);
            if (problem) setProblem(undefined);
          }}
          inputRef={wordsRef}
          disabled={busy}
        />
      </Field>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <div class="onboarding-actions">
        <Button type="submit" variant="primary" size="lg" block loading={busy}>
          Open wallet
        </Button>
        <div class="onboarding-actions__secondary">
          <Button variant="ghost" onClick={onShowAgain} disabled={busy}>
            Show recovery phrase again
          </Button>
        </div>
      </div>
    </form>
  );
}
