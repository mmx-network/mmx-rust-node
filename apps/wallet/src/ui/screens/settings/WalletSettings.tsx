import { useEffect, useRef, useState } from 'preact/hooks';
import { toBackendError, type WalletInfo } from '../../../backend/types';
import { MAX_ADDRESSES, WALLET_NAME_MAX } from '../../../lib/appstate';
import { errorMessage } from '../../../lib/errors';
import { validateName } from '../../../lib/text';
import {
  activeWalletId,
  lockVersion,
  holdSensitive,
  checkHiddenFunds,
  copyToClipboard,
  notify,
  platform,
  removeWallet,
  renameWallet,
  revealMnemonic,
  setAddressCount,
  unlockWallet,
  type HiddenFundsCheck,
} from '../../../state';
import {
  Banner,
  Button,
  ConfirmDialog,
  CopyButton,
  Dialog,
  Field,
  NumberStepper,
  OptionRow,
  PasswordInput,
  Section,
  Stack,
  TextInput,
  WordGrid,
} from '../../components';
import { PASSPHRASE_FIELD, PASSWORD_MANAGER_IGNORE, useMounted } from '../welcome/flow';
import { fundedMessage } from './text';
import { AuthorizationDialog } from '../unlock/AuthorizationDialog';

function nameError(name: string): string | undefined {
  switch (validateName(name, WALLET_NAME_MAX)) {
    case 'empty':
      return 'Enter a name.';
    case 'too_long':
      return `Use up to ${WALLET_NAME_MAX} characters.`;
    case 'invalid':
      return 'Remove line breaks and special characters.';
    default:
      return undefined;
  }
}

function RenameForm({ wallet, onClose }: { wallet: WalletInfo; onClose: () => void }) {
  const [name, setName] = useState(wallet.name);
  const [submitted, setSubmitted] = useState(false);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const error = nameError(name);

  useEffect(() => inputRef.current?.select(), []);

  const submit = async (event: Event): Promise<void> => {
    event.preventDefault();
    if (busy) return;
    setSubmitted(true);
    if (error) return;
    if (name.trim() === wallet.name) {
      onClose();
      return;
    }
    setBusy(true);
    setFailure(null);
    try {
      await renameWallet(wallet.id, name);
      notify('Wallet renamed.', 'success');
      onClose();
    } catch (e) {
      setFailure(errorMessage(e));
      setBusy(false);
    }
  };

  return (
    <Dialog
      open
      onClose={onClose}
      dismissible={!busy}
      size="sm"
      title="Rename wallet"
      footer={
        <>
          <Button variant="secondary" disabled={busy} onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" type="submit" form="rename-wallet" loading={busy}>
            Save
          </Button>
        </>
      }
    >
      <form id="rename-wallet" onSubmit={submit} noValidate>
        <Stack gap={4}>
          {failure ? <Banner tone="danger">{failure}</Banner> : null}
          <Field label="Name" error={submitted || name.trim() !== '' ? error : undefined}>
            <TextInput value={name} onValue={setName} inputRef={inputRef} autocomplete="off" enterkeyhint="done" data-autofocus="" />
          </Field>
        </Stack>
      </form>
    </Dialog>
  );
}

/** A confirmation before hiding addresses, for the count that was checked. */
type CountCheck = Exclude<HiddenFundsCheck, { kind: 'none' }> & { count: number };

function AddressCount({ wallet }: { wallet: WalletInfo }) {
  const [count, setCount] = useState(wallet.addressCount);
  const [busy, setBusy] = useState(false);
  const [check, setCheck] = useState<CountCheck | null>(null);
  const [authorizeCount, setAuthorizeCount] = useState<number | null>(null);
  const [passphraseError, setPassphraseError] = useState<string | undefined>();
  const mounted = useMounted();

  useEffect(() => setCount(wallet.addressCount), [wallet.id, wallet.addressCount]);
  const changed = count !== wallet.addressCount;

  const apply = async (target: number, secret?: string): Promise<void> => {
    const id = wallet.id;
    setBusy(true);
    try {
      await setAddressCount(target, secret);
      if (mounted.current) setAuthorizeCount(null);
      // Another wallet may be open by now: don't report this change there.
      if (activeWalletId.value === id) {
        notify(target === 1 ? 'This wallet now uses 1 address.' : `This wallet now uses ${target} addresses.`, 'success');
      }
    } catch (e) {
      const code = toBackendError(e).code;
      if (mounted.current && (code === 'locked' || code === 'passphrase_required' || code === 'invalid_passphrase')) {
        setAuthorizeCount(target);
        setPassphraseError(code === 'invalid_passphrase' ? errorMessage(e) : undefined);
        return;
      }
      notify(errorMessage(e), 'error');
      if (mounted.current) setCount(wallet.addressCount);
    } finally {
      if (mounted.current) {
        setBusy(false);
        setCheck(null);
      }
    }
  };

  const save = async (): Promise<void> => {
    if (busy) return;
    const target = count;
    // Checked against the stored count: it may have changed in another tab or with the command line.
    setBusy(true);
    let result: HiddenFundsCheck;
    try {
      result = await checkHiddenFunds(target);
    } catch {
      result = { kind: 'unknown' };
    }
    if (!mounted.current) return;
    setBusy(false);
    if (result.kind === 'none') void apply(target);
    else setCheck({ ...result, count: target });
  };

  return (
    <>
      <div class="settings-count">
        <NumberStepper value={count} onChange={setCount} min={1} max={MAX_ADDRESSES} label="Number of addresses" disabled={busy} />
        <div class="settings-count__actions">
          <Button size="sm" variant="ghost" disabled={busy || !changed} onClick={() => setCount(wallet.addressCount)}>
            Cancel
          </Button>
          <Button size="sm" variant="primary" disabled={!changed} loading={busy} onClick={() => void save()}>
            Save
          </Button>
        </div>
      </div>
      <ConfirmDialog
        open={check !== null}
        title={check?.kind === 'funded' ? 'Hide addresses with funds?' : 'Hide addresses?'}
        message={
          check?.kind === 'funded'
            ? `${fundedMessage(check.indices)} Hidden addresses are left out of your balance until you show them again.`
            : 'The balance of these addresses isn’t loaded yet, so some may hold funds.'
        }
        confirmLabel="Hide addresses"
        busy={busy}
        onConfirm={() => {
          if (check) void apply(check.count);
        }}
        onCancel={() => setCheck(null)}
      />
      {authorizeCount !== null ? <AuthorizationDialog open wallet={wallet} busy={busy} error={passphraseError}
        onClose={() => { setAuthorizeCount(null); setPassphraseError(undefined); }} onAuthorize={async (request) => {
          const target = authorizeCount;
          setBusy(true);
          try {
            if (!wallet.withPassphrase) await unlockWallet(wallet.id, request, { navigateTo: null });
            setBusy(false);
            await apply(target, request.passphrase);
          } catch (e) { setPassphraseError(errorMessage(e)); setBusy(false); }
        }} /> : null}
    </>
  );
}

function RecoveryPhrase({ wallet }: { wallet: WalletInfo }) {
  const [stage, setStage] = useState<'idle' | 'confirm' | 'authorize' | 'loading'>('idle');
  const [authorizing, setAuthorizing] = useState(false);
  const [words, setWords] = useState<string[] | null>(null);
  const [passphrase, setPassphrase] = useState('');
  const [passphraseError, setPassphraseError] = useState<string | undefined>(undefined);
  const passphraseRef = useRef<HTMLInputElement>(null);
  const mounted = useMounted();
  const lockedAt = lockVersion.value;
  const holding = words !== null || stage !== 'idle';
  useEffect(() => holding ? holdSensitive() : undefined, [holding]);
  // Someone at an open wallet must still know the passphrase to see the words.
  const ask = wallet.withPassphrase;

  // Never keep the words of another wallet around.
  useEffect(() => {
    setWords(null);
    setStage('idle');
    setPassphrase('');
    setPassphraseError(undefined);
  }, [wallet.id, lockedAt]);

  const close = (): void => {
    setStage('idle');
    setPassphrase('');
    setPassphraseError(undefined);
  };

  const reveal = async (): Promise<void> => {
    if (stage === 'loading') return;
    const started = lockVersion.peek();
    const typed = passphraseRef.current?.value ?? passphrase;
    if (ask && !typed) {
      setPassphraseError('Enter your passphrase.');
      passphraseRef.current?.focus();
      return;
    }
    setStage('loading');
    setPassphraseError(undefined);
    try {
      setPassphrase('');
      if (passphraseRef.current) passphraseRef.current.value = '';
      const phrase = await revealMnemonic(wallet.id, ask ? typed : undefined);
      if (!mounted.current || started !== lockVersion.peek()) return;
      setWords(phrase.trim().split(/\s+/));
      close();
    } catch (e) {
      if (!mounted.current) return;
      const code = toBackendError(e).code;
      if (ask && (code === 'invalid_passphrase' || code === 'passphrase_required')) {
        setStage('confirm');
        setPassphraseError(errorMessage(e));
        requestAnimationFrame(() => passphraseRef.current?.select());
        return;
      }
      close();
      notify(errorMessage(e), 'error');
    }
  };

  const copy = async (): Promise<void> => {
    if (!words || !(await copyToClipboard(words.join(' '), { sensitive: true }))) throw new Error('copy failed');
  };

  return (
    <>
      <Button size="sm" icon="key" onClick={() => setStage(platform.value === 'web' && wallet.locked && !ask ? 'authorize' : 'confirm')}>
        Show
      </Button>
      <ConfirmDialog
        open={stage === 'confirm' || stage === 'loading'}
        title="Show recovery phrase?"
        message="Anyone who sees these words can take your funds. Make sure no one is watching your screen."
        confirmLabel="Show phrase"
        busy={stage === 'loading'}
        onConfirm={() => void reveal()}
        onCancel={close}
      >
        {ask ? (
          <form
            class="settings-reveal"
            {...PASSWORD_MANAGER_IGNORE}
            noValidate
            onSubmit={(event) => {
              event.preventDefault();
              void reveal();
            }}
          >
            <Field label="Passphrase" error={passphraseError}>
              <PasswordInput
                name="wallet-passphrase"
                {...PASSPHRASE_FIELD}
                value={passphrase}
                onValue={setPassphrase}
                inputRef={passphraseRef}
                disabled={stage === 'loading'}
                data-autofocus=""
              />
            </Field>
          </form>
        ) : null}
      </ConfirmDialog>
      {stage === 'authorize' ? <AuthorizationDialog open wallet={wallet} busy={authorizing} error={passphraseError}
        onClose={close} onAuthorize={async (request) => {
          setAuthorizing(true);
          try {
            await unlockWallet(wallet.id, request, { navigateTo: null });
            await reveal();
          } catch (e) {
            if (!mounted.current) return;
            setPassphraseError(errorMessage(e));
          } finally { if (mounted.current) setAuthorizing(false); }
        }} /> : null}
      <Dialog
        open={words !== null}
        onClose={() => setWords(null)}
        title="Recovery phrase"
        description={wallet.name}
        size="lg"
        class="settings-phrase"
        footer={
          <>
            <CopyButton variant="button" label="Copy" onCopy={copy} />
            <Button variant="primary" onClick={() => setWords(null)} data-autofocus="">
              Done
            </Button>
          </>
        }
      >
        <Stack gap={4}>
          {words ? <WordGrid words={words} /> : null}
          {wallet.withPassphrase ? <Banner tone="info">You also need your passphrase to restore this wallet.</Banner> : null}
        </Stack>
      </Dialog>
    </>
  );
}

/** What the user needs before removing a wallet: the words, and the passphrase if it has one. */
function removeMessage(web: boolean, withPassphrase: boolean): string {
  if (web) {
    return withPassphrase
      ? 'Make sure your recovery phrase is saved and you know your passphrase. You need both to open this wallet again.'
      : 'Make sure your recovery phrase is saved. You need it to open this wallet again.';
  }
  return withPassphrase
    ? 'Make sure you have the recovery phrase and your passphrase. You need both to restore this wallet.'
    : 'Make sure you have the recovery phrase. You need it to restore this wallet.';
}

function RemoveWallet({ wallet }: { wallet: WalletInfo }) {
  const web = platform.value === 'web';
  const [open, setOpen] = useState(false);
  const [typed, setTyped] = useState('');
  const [busy, setBusy] = useState(false);
  const mounted = useMounted();
  const matches = typed.trim().toLowerCase() === wallet.name.trim().toLowerCase();

  const close = (): void => {
    setOpen(false);
    setTyped('');
  };

  const remove = async (): Promise<void> => {
    if (!matches || busy) return;
    setBusy(true);
    try {
      await removeWallet(wallet.id);
      notify(web ? 'Wallet forgotten on this device.' : 'Wallet removed from this computer.', 'success');
    } catch (e) {
      notify(errorMessage(e), 'error');
    } finally {
      if (mounted.current) {
        setBusy(false);
        close();
      }
    }
  };

  return (
    <>
      <Button size="sm" variant="danger" icon="trash" onClick={() => setOpen(true)}>
        {web ? 'Forget' : 'Remove'}
      </Button>
      <ConfirmDialog
        open={open}
        tone="danger"
        title={web ? `Forget ${wallet.name}?` : `Remove ${wallet.name}?`}
        message={removeMessage(web, wallet.withPassphrase)}
        confirmLabel={web ? 'Forget wallet' : 'Remove wallet'}
        busy={busy}
        confirmDisabled={!matches}
        onConfirm={() => void remove()}
        onCancel={close}
      >
        <form
          class="settings-remove"
          onSubmit={(event) => {
            event.preventDefault();
            void remove();
          }}
          noValidate
        >
          <Field label={`Type “${wallet.name}” to confirm`}>
            <TextInput value={typed} onValue={setTyped} autocomplete="off" spellcheck={false} disabled={busy} data-autofocus="" />
          </Field>
        </form>
      </ConfirmDialog>
    </>
  );
}

export function WalletSettings({ wallet }: { wallet: WalletInfo }) {
  const web = platform.value === 'web';
  const [renaming, setRenaming] = useState(false);

  const copyId = async (): Promise<void> => {
    if (!(await copyToClipboard(wallet.fingerprint, { message: 'Wallet ID copied' }))) throw new Error('copy failed');
  };

  return (
    <>
      <Section title="Wallet" card>
        <OptionRow title="Name" description={wallet.name}>
          <Button size="sm" icon="edit" onClick={() => setRenaming(true)}>
            Rename
          </Button>
        </OptionRow>
        <OptionRow title="Wallet ID" description={<span class="settings__mono">{wallet.fingerprint}</span>}>
          <CopyButton label="Copy wallet ID" onCopy={copyId} />
        </OptionRow>
        <OptionRow title="Addresses" description="How many addresses this wallet uses">
          <AddressCount wallet={wallet} />
        </OptionRow>
        <OptionRow
          title="Recovery phrase"
          description={wallet.withPassphrase ? 'Restores this wallet together with your passphrase' : 'Restores this wallet on any device'}
        >
          <RecoveryPhrase wallet={wallet} />
        </OptionRow>
        <OptionRow
          tone="danger"
          title={web ? 'Forget on this device' : 'Remove from this computer'}
          description={`${web ? 'You can open it again with the recovery phrase' : 'You can restore it with the recovery phrase'}${
            wallet.withPassphrase ? ' and passphrase' : ''
          }`}
        >
          <RemoveWallet wallet={wallet} />
        </OptionRow>
      </Section>
      {renaming ? <RenameForm wallet={wallet} onClose={() => setRenaming(false)} /> : null}
    </>
  );
}
