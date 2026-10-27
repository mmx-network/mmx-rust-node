/**
 * Forms of the unlock flows: the password manager credential form (web),
 * the passphrase step and the desktop passphrase prompt.
 */
import type { ComponentChildren } from 'preact';
import { useEffect, useRef, useState } from 'preact/hooks';
import type { WalletInfo } from '../../../backend/types';
import { credentialUsername } from '../../../state';
import { Banner, Button, Field, Identicon, PasswordInput, TextInput } from '../../components';
import { PASSPHRASE_FIELD, PASSWORD_MANAGER_IGNORE, useInitialFocus } from '../welcome/flow';
import type { OpenWallet } from './useOpenWallet';
import './unlock.css';

export interface CredentialFormProps {
  open: OpenWallet;
  /** The wallet being opened; omitted for "Open another wallet". */
  wallet?: WalletInfo;
  submitLabel: string;
  /** Shows "Change" next to the wallet. */
  onChangeWallet?: () => void;
  /** Focus the first empty field on mount. */
  autoFocus?: boolean;
  /** Content below the submit button. */
  children?: ComponentChildren;
}

/**
 * Username + password form that password managers recognise as a sign-in:
 * the username is the saved wallet name with its ID, the password the
 * recovery words.
 */
export function CredentialForm({ open, wallet, submitLabel, onChangeWallet, autoFocus = true, children }: CredentialFormProps) {
  const usernameRef = useRef<HTMLInputElement>(null);
  const wordsRef = useRef<HTMLInputElement>(null);
  useInitialFocus(wallet ? wordsRef : usernameRef, autoFocus);
  const wordsError = open.errors.words;
  useEffect(() => {
    if (wordsError) wordsRef.current?.focus();
  }, [wordsError]);

  const syncInputs = (): void => {
    // Either field can notify first after autofill. Capture both before a
    // render can replace a silently filled value with the previous state.
    if (!wallet && usernameRef.current) open.setUsername(usernameRef.current.value);
    if (wordsRef.current) open.setWords(wordsRef.current.value);
  };

  const onSubmit = (event: Event): void => {
    event.preventDefault();
    if (open.busy) return;
    // Read the inputs directly: password managers may fill them without input events.
    const username = usernameRef.current?.value ?? open.username;
    const words = wordsRef.current?.value ?? open.words;
    open.submitWords(username, words);
  };

  return (
    <form class="onboarding-form unlock-credentials" method="post" action="#" noValidate onSubmit={onSubmit}>
      {wallet ? (
        <Field
          label="Wallet"
          labelAction={
            onChangeWallet ? (
              <Button size="sm" variant="ghost" class="unlock-credentials__change" onClick={onChangeWallet} disabled={open.busy}>
                Change
              </Button>
            ) : undefined
          }
        >
          <TextInput
            name="username"
            autocomplete="username"
            value={credentialUsername(wallet.name, wallet.fingerprint)}
            readOnly
            inputRef={usernameRef}
            leading={<Identicon value={wallet.fingerprint || wallet.id} size={24} />}
            class="unlock-credentials__wallet"
          />
        </Field>
      ) : (
        <Field label="Wallet">
          <TextInput
            name="username"
            autocomplete="username"
            value={open.username}
            onValue={syncInputs}
            inputRef={usernameRef}
            spellcheck={false}
            autocapitalize="off"
            disabled={open.busy}
          />
        </Field>
      )}
      <Field label="Recovery phrase" hint="Use your password manager to fill this in." error={open.errors.words}>
        <PasswordInput
          name="password"
          autocomplete="current-password"
          value={open.words}
          onValue={syncInputs}
          inputRef={wordsRef}
          disabled={open.busy}
        />
      </Field>
      {open.errors.general ? <Banner tone="danger">{open.errors.general}</Banner> : null}
      <div class="onboarding-actions">
        <Button type="submit" variant="primary" size="lg" block loading={open.busy}>
          {submitLabel}
        </Button>
        {children}
      </div>
    </form>
  );
}

export interface PassphraseFormProps {
  open: OpenWallet;
}

/** Passphrase step after the recovery words (kept away from password managers). */
export function PassphraseForm({ open }: PassphraseFormProps) {
  const [value, setValue] = useState('');
  const ref = useRef<HTMLInputElement>(null);
  useInitialFocus(ref);
  const optional = open.passphraseOptional;
  const error = open.errors.passphrase;
  useEffect(() => {
    if (error && ref.current) {
      ref.current.focus();
      ref.current.select();
    }
  }, [error]);

  const onSubmit = (event: Event): void => {
    event.preventDefault();
    if (open.busy) return;
    open.submitPassphrase(ref.current?.value ?? value);
  };

  return (
    <form class="onboarding-form" {...PASSWORD_MANAGER_IGNORE} noValidate onSubmit={onSubmit}>
      <Field
        label="Passphrase"
        optional={optional}
        hint={optional ? 'Leave empty if this wallet has none.' : undefined}
        error={open.errors.passphrase}
      >
        <PasswordInput
          name="wallet-passphrase"
          {...PASSPHRASE_FIELD}
          value={value}
          onValue={setValue}
          inputRef={ref}
          disabled={open.busy}
        />
      </Field>
      {open.errors.general ? <Banner tone="danger">{open.errors.general}</Banner> : null}
      <div class="onboarding-actions">
        <Button type="submit" variant="primary" size="lg" block loading={open.busy}>
          {optional && !value ? 'Continue' : 'Unlock'}
        </Button>
      </div>
    </form>
  );
}

export interface DesktopPassphraseFormProps {
  busy: boolean;
  error?: string;
  generalError?: string;
  onSubmit: (passphrase: string) => void;
}

/** Desktop: passphrase prompt of a locked wallet. */
export function DesktopPassphraseForm({ busy, error, generalError, onSubmit }: DesktopPassphraseFormProps) {
  const [value, setValue] = useState('');
  const ref = useRef<HTMLInputElement>(null);
  useInitialFocus(ref);
  // After a wrong passphrase, select it so it can be typed again right away.
  useEffect(() => {
    if (error && ref.current) {
      ref.current.focus();
      ref.current.select();
    }
  }, [error]);

  const submit = (event: Event): void => {
    event.preventDefault();
    if (busy) return;
    onSubmit(ref.current?.value ?? value);
  };

  return (
    <form class="onboarding-form" {...PASSWORD_MANAGER_IGNORE} noValidate onSubmit={submit}>
      <Field label="Passphrase" error={error}>
        <PasswordInput
          name="wallet-passphrase"
          {...PASSPHRASE_FIELD}
          value={value}
          onValue={setValue}
          inputRef={ref}
          disabled={busy}
        />
      </Field>
      {generalError ? <Banner tone="danger">{generalError}</Banner> : null}
      <Button type="submit" variant="primary" size="lg" block loading={busy}>
        Unlock
      </Button>
    </form>
  );
}
