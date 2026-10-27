/** Opens a timed signing session without leaving the current wallet screen. */
import { useEffect, useRef, useState } from 'preact/hooks';
import type { UnlockRequest, WalletInfo } from '../../../backend/types';
import { credentialUsername, holdSensitive, lockVersion, platform } from '../../../state';
import { Banner, Button, Dialog, Field, PasswordInput, TextInput } from '../../components';
import { PASSPHRASE_FIELD, PASSWORD_MANAGER_IGNORE } from '../welcome/flow';

interface AuthorizationProps {
  open: boolean;
  wallet: WalletInfo;
  busy: boolean;
  error?: string;
  onClose: () => void;
  onAuthorize: (request: UnlockRequest) => void;
}

export function AuthorizationDialog({ open, wallet, busy, error, onClose, onAuthorize }: AuthorizationProps) {
  const [value, setValue] = useState('');
  const ref = useRef<HTMLInputElement>(null);
  const lockedAt = lockVersion.value;
  const previous = useRef({ walletId: wallet.id, lockedAt, open });
  useEffect(() => open ? holdSensitive() : undefined, [open]);
  useEffect(() => {
    const old = previous.current;
    // Mounting must not erase a credential a password manager just filled.
    if (old.walletId !== wallet.id || old.lockedAt !== lockedAt || old.open !== open) {
      setValue('');
      if (ref.current) ref.current.value = '';
    }
    if (old.lockedAt !== lockedAt) onClose();
    previous.current = { walletId: wallet.id, lockedAt, open };
  }, [wallet.id, lockedAt, open]);
  useEffect(() => {
    if (error) ref.current?.focus();
  }, [error, busy]);
  const phrase = platform.value === 'web' && !wallet.withPassphrase;
  return (
    <Dialog
      open={open}
      onClose={() => { setValue(''); onClose(); }}
      dismissible={!busy}
      size="sm"
      title="Unlock for spending"
      description={`Unlock ${wallet.name} to continue. It locks again after inactivity.`}
    >
      <form
        class={phrase ? 'unlock-credentials' : undefined}
        method="post"
        action="#"
        {...(phrase ? {} : PASSWORD_MANAGER_IGNORE)}
        onSubmit={(event) => {
          event.preventDefault();
          if (busy) return;
          const typed = ref.current?.value ?? value;
          setValue('');
          if (ref.current) ref.current.value = '';
          onAuthorize(phrase ? { mnemonic: typed } : wallet.withPassphrase ? { passphrase: typed } : {});
        }}
      >
        {phrase ? (
          <Field label="Wallet">
            <TextInput name="username" autocomplete="username" value={credentialUsername(wallet.name, wallet.fingerprint)} readOnly />
          </Field>
        ) : null}
        {phrase || wallet.withPassphrase ? (
          <Field label={phrase ? 'Recovery phrase' : 'Passphrase'}>
            <PasswordInput
              name={phrase ? 'password' : 'wallet-passphrase'}
              {...(phrase ? { autocomplete: 'current-password' } : PASSPHRASE_FIELD)}
              value={value}
              onValue={setValue}
              inputRef={ref}
              disabled={busy}
              data-autofocus=""
            />
          </Field>
        ) : null}
        {error ? <Banner tone="danger">{error}</Banner> : null}
        <Button type="submit" variant="primary" loading={busy}>Unlock</Button>
      </form>
    </Dialog>
  );
}
