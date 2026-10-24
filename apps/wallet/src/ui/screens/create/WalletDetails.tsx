/**
 * First step of creating or importing a wallet: its name and an optional
 * passphrase, typed twice.
 */
import { useEffect, useRef, useState } from 'preact/hooks';
import { Banner, Button, Card, Field, Icon, OptionRow, PasswordInput, Switch, TextInput } from '../../components';
import { PASSPHRASE_FIELD, PASSWORD_MANAGER_IGNORE, useInitialFocus } from '../welcome/flow';
import { detailsErrors, type DetailsErrors, type DetailsValue } from './details';
import './create.css';

export interface WalletDetailsProps {
  mode: 'create' | 'import';
  value: DetailsValue;
  onChange: (value: DetailsValue) => void;
  busy?: boolean;
  /** Name problem reported when saving. */
  nameError?: string;
  /** Other problem reported when saving. */
  error?: string;
  submitLabel: string;
  onSubmit: () => void;
}

export function WalletDetails({
  mode,
  value,
  onChange,
  busy = false,
  nameError,
  error,
  submitLabel,
  onSubmit,
}: WalletDetailsProps) {
  const [attempted, setAttempted] = useState(false);
  const nameRef = useRef<HTMLInputElement>(null);
  const passphraseRef = useRef<HTMLInputElement>(null);
  const repeatRef = useRef<HTMLInputElement>(null);
  const toggled = useRef(false);
  const create = mode === 'create';
  useInitialFocus(nameRef);

  useEffect(() => {
    if (toggled.current && value.usePassphrase) passphraseRef.current?.focus();
  }, [value.usePassphrase]);

  const errors = detailsErrors(value);
  const shown: DetailsErrors = attempted ? errors : {};

  const set = (patch: Partial<DetailsValue>): void => onChange({ ...value, ...patch });

  const submit = (event: Event): void => {
    event.preventDefault();
    if (busy) return;
    setAttempted(true);
    if (errors.name) nameRef.current?.focus();
    else if (errors.passphrase) passphraseRef.current?.focus();
    else if (errors.repeat) repeatRef.current?.focus();
    else onSubmit();
  };

  return (
    <form class="onboarding-form create-details" autocomplete="off" noValidate onSubmit={submit}>
      <Field label="Wallet name" error={shown.name ?? nameError}>
        <TextInput
          name="wallet-name"
          {...PASSWORD_MANAGER_IGNORE}
          value={value.name}
          onValue={(name) => set({ name })}
          inputRef={nameRef}
          spellcheck={false}
          disabled={busy}
          enterKeyHint={value.usePassphrase ? 'next' : 'go'}
        />
      </Field>

      <Card padding="none" class="create-details__passphrase">
        <OptionRow
          title={create ? 'Add a passphrase' : 'This wallet has a passphrase'}
          description={create ? 'Asked each time you open this wallet' : 'Only if you set one for these words'}
        >
          <Switch
            checked={value.usePassphrase}
            onChange={(on) => {
              toggled.current = true;
              // Turning it off forgets what was typed.
              set(on ? { usePassphrase: true } : { usePassphrase: false, passphrase: '', repeat: '' });
            }}
            aria-label={create ? 'Add a passphrase' : 'This wallet has a passphrase'}
            disabled={busy}
          />
        </OptionRow>
        {value.usePassphrase ? (
          <div class="create-details__fields">
            <Field label="Passphrase" error={shown.passphrase}>
              <PasswordInput
                name="wallet-passphrase"
                {...PASSPHRASE_FIELD}
                value={value.passphrase}
                onValue={(passphrase) => set({ passphrase })}
                inputRef={passphraseRef}
                disabled={busy}
              />
            </Field>
            <Field label="Repeat passphrase" error={shown.repeat}>
              <PasswordInput
                name="wallet-passphrase-repeat"
                {...PASSPHRASE_FIELD}
                value={value.repeat}
                onValue={(repeat) => set({ repeat })}
                inputRef={repeatRef}
                disabled={busy}
              />
            </Field>
            {create ? (
              <p class="create-details__warning">
                <Icon name="warning" size="sm" class="create-details__warning-icon" />
                <span>A forgotten passphrase can’t be recovered.</span>
              </p>
            ) : null}
          </div>
        ) : null}
      </Card>

      {error ? <Banner tone="danger">{error}</Banner> : null}
      <Button type="submit" variant="primary" size="lg" block loading={busy}>
        {submitLabel}
      </Button>
    </form>
  );
}
