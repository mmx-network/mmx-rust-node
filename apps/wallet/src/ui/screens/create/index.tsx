import { useEffect, useRef, useState } from 'preact/hooks';
import { toBackendError, type WalletInfo } from '../../../backend/types';
import { errorMessage } from '../../../lib/errors';
import { joinWords, splitWords } from '../../../lib/mnemonic';
import {
  createWallet,
  credentialUsername,
  discardNewWallet,
  finishCreate,
  leaveOnboarding,
  navigate,
  platform,
  renameWallet,
  suggestWalletName,
  wallets,
} from '../../../state';
import { Button, ProgressSteps } from '../../components';
import { FocusLayout } from '../../shell';
import { FlowHeader, useMounted } from '../welcome/flow';
import { ConfirmWords } from './ConfirmWords';
import { PhraseWords } from './PhraseWords';
import { SaveCredential } from './SaveCredential';
import { VerifyOpen } from './VerifyOpen';
import { chosenPassphrase, emptyDetails, type DetailsValue } from './details';
import { WalletDetails } from './WalletDetails';
import './create.css';

type Step = 'details' | 'phrase' | 'confirm' | 'save' | 'verify';

interface Created {
  wallet: WalletInfo;
  mnemonic: string;
  /** '' = no passphrase. */
  passphrase: string;
}

const STEPS = ['Name', 'Recovery phrase', 'Confirm'];
const STEP_INDEX: Record<Step, number> = { details: 0, phrase: 1, save: 1, confirm: 2, verify: 2 };

export default function CreateScreen() {
  const web = platform.value === 'web';
  const [step, setStep] = useState<Step>('details');
  const [details, setDetails] = useState<DetailsValue>(() => emptyDetails(suggestWalletName()));
  const [created, setCreatedState] = useState<Created | null>(null);
  const [revealed, setRevealed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | undefined>(undefined);
  const [nameError, setNameError] = useState<string | undefined>(undefined);
  const mounted = useMounted();
  const createdRef = useRef<Created | null>(null);
  /** The new wallet stays when leaving: it was confirmed (desktop) or saved to the password manager (web). */
  const keep = useRef(false);
  /** A create, rename, discard or confirm call is running. */
  const working = useRef(false);
  // The ref is the source of truth for async handlers and the unmount cleanup.
  const setCreated = (c: Created | null): void => {
    createdRef.current = c;
    setCreatedState(c);
  };

  // Leaving before the words were confirmed or saved removes the unfinished wallet.
  useEffect(
    () => () => {
      const c = createdRef.current;
      if (c && !keep.current) void discardNewWallet(c.wallet.id);
    },
    [],
  );

  const goto = (next: Step): void => {
    setError(undefined);
    setStep(next);
  };

  const cancel = async (): Promise<void> => {
    if (working.current) return;
    const c = createdRef.current;
    if (c) {
      working.current = true;
      setBusy(true);
      setCreated(null);
      await discardNewWallet(c.wallet.id);
      working.current = false;
      if (!mounted.current) return;
      setBusy(false);
    }
    leaveOnboarding();
  };

  const backToStart = async (): Promise<void> => {
    if (working.current) return;
    const c = createdRef.current;
    if (c) {
      working.current = true;
      setCreated(null);
      await discardNewWallet(c.wallet.id);
      working.current = false;
    }
    navigate('welcome');
  };

  const submitDetails = async (): Promise<void> => {
    if (working.current) return;
    working.current = true;
    const passphrase = chosenPassphrase(details);
    const name = details.name.trim();
    setBusy(true);
    setError(undefined);
    setNameError(undefined);
    try {
      let c = createdRef.current;
      if (c && c.passphrase !== passphrase) {
        // A different passphrase makes a different wallet.
        setCreated(null);
        await discardNewWallet(c.wallet.id);
        c = null;
      }
      if (c) {
        if (c.wallet.name !== name) {
          await renameWallet(c.wallet.id, name);
          c = { ...c, wallet: { ...c.wallet, name } };
        }
      } else {
        const result = await createWallet(passphrase ? { name, passphrase } : { name });
        c = { wallet: result.wallet, mnemonic: result.mnemonic, passphrase };
        keep.current = false;
        setRevealed(false);
      }
      if (!mounted.current) {
        if (!keep.current) void discardNewWallet(c.wallet.id);
        return;
      }
      setCreated(c);
      goto(web ? 'save' : 'phrase');
    } catch (e) {
      if (!mounted.current) return;
      if (toBackendError(e).code === 'invalid_argument') setNameError('Choose a different name.');
      else setError(errorMessage(e));
    } finally {
      working.current = false;
      if (mounted.current) setBusy(false);
    }
  };

  const confirm = async (): Promise<void> => {
    const c = createdRef.current;
    if (!c || working.current) return;
    working.current = true;
    keep.current = true;
    setBusy(true);
    setError(undefined);
    try {
      await finishCreate(c.wallet.id);
    } catch (e) {
      keep.current = false;
      if (mounted.current) setError(errorMessage(e));
    } finally {
      working.current = false;
      if (mounted.current) setBusy(false);
    }
  };

  // Canonical form (lowercase, single spaces): exactly what is saved and later typed or filled in.
  const phrase = created ? joinWords(splitWords(created.mnemonic)) : '';
  const words = phrase ? phrase.split(' ') : [];
  const wallet = created ? (wallets.value.find((w) => w.id === created.wallet.id) ?? created.wallet) : null;
  const cancelAction =
    step !== 'details' ? (
      <Button variant="ghost" size="sm" onClick={() => void cancel()} disabled={busy}>
        Cancel
      </Button>
    ) : undefined;
  const back: Record<Step, () => void> = {
    details: () => void backToStart(),
    phrase: () => goto('details'),
    save: () => goto('details'),
    confirm: () => goto('phrase'),
    verify: () => goto('save'),
  };
  const wide = step === 'phrase' || step === 'save';

  return (
    <FocusLayout
      width={wide ? 'wide' : 'narrow'}
      onBack={() => {
        if (!busy && !working.current) back[step]();
      }}
      headerAction={cancelAction}
      class="create"
    >
      <ProgressSteps steps={STEPS} current={STEP_INDEX[step]} label="Create a wallet" />
      {step === 'details' ? (
        <>
          <FlowHeader title="Create a wallet" />
          <WalletDetails
            mode="create"
            value={details}
            onChange={(value) => {
              if (nameError && value.name !== details.name) setNameError(undefined);
              setDetails(value);
            }}
            busy={busy}
            nameError={nameError}
            error={error}
            submitLabel="Continue"
            onSubmit={() => void submitDetails()}
          />
        </>
      ) : null}

      {step === 'phrase' && created ? (
        <>
          <FlowHeader
            key="phrase"
            autoFocus
            title="Write down your recovery phrase"
            subtitle="They’re the only way to restore your wallet. Never share them."
          />
          <PhraseWords words={words} revealed={revealed} onRevealedChange={setRevealed} />
          <Button variant="primary" size="lg" block disabled={!revealed} onClick={() => goto('confirm')}>
            I wrote it down
          </Button>
        </>
      ) : null}

      {step === 'confirm' && created ? (
        <>
          <FlowHeader key="confirm" autoFocus title="Confirm your recovery phrase" subtitle="Pick the right word for each number." />
          <ConfirmWords
            words={words}
            busy={busy}
            error={error}
            onConfirmed={() => void confirm()}
            onShowWords={() => goto('phrase')}
          />
        </>
      ) : null}

      {step === 'save' && created && wallet ? (
        <>
          <FlowHeader
            key="save"
            autoFocus
            title="Save your recovery phrase"
            subtitle="Save it in your password manager and keep a written copy."
          />
          <PhraseWords words={words} revealed={revealed} onRevealedChange={setRevealed} />
          <SaveCredential
            username={credentialUsername(wallet.name, wallet.fingerprint)}
            words={phrase}
            submitLabel="Save to password manager"
            onSaved={() => {
              keep.current = true;
              goto('verify');
            }}
          />
        </>
      ) : null}

      {step === 'verify' && created && wallet ? (
        <>
          <FlowHeader key="verify" title="Open your new wallet" subtitle="This confirms your password manager saved it." />
          <VerifyOpen wallet={wallet} passphrase={created.passphrase} onShowAgain={() => goto('save')} />
        </>
      ) : null}
    </FocusLayout>
  );
}
