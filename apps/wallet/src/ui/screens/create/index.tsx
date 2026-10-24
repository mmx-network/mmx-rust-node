import { useEffect, useRef, useState } from 'preact/hooks';
import { toBackendError } from '../../../backend/types';
import { errorMessage } from '../../../lib/errors';
import {
  beginSetup,
  credentialUsername,
  finishCreate,
  leaveOnboarding,
  navigate,
  newRecoveryPhrase,
  newWalletFingerprint,
  platform,
  suggestWalletName,
} from '../../../state';
import { Banner, Button, ProgressSteps } from '../../components';
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

/** What the details step settled on. Changing it later keeps the same recovery words. */
interface Chosen {
  name: string;
  /** '' = no passphrase. */
  passphrase: string;
  /** Web: wallet ID of the words with this passphrase (part of the saved username). */
  fingerprint: string;
}

const STEPS = ['Name', 'Recovery phrase', 'Confirm'];
const STEP_INDEX: Record<Step, number> = { details: 0, phrase: 1, save: 1, confirm: 2, verify: 2 };

/**
 * Creates a wallet: name and passphrase → recovery words → confirmation
 * (desktop: pick three words; web: save to the password manager, then fill
 * the phrase in again). The wallet is stored only after the confirmation;
 * until then the words exist only in this screen and leaving drops them.
 */
export default function CreateScreen() {
  const web = platform.value === 'web';
  const [step, setStep] = useState<Step>('details');
  const [details, setDetails] = useState<DetailsValue>(() => emptyDetails(suggestWalletName()));
  /** The new recovery words, lowercase, single spaces. */
  const [phrase, setPhrase] = useState<string | null>(null);
  const [chosen, setChosen] = useState<Chosen | null>(null);
  /** Web: the username last saved to the password manager. */
  const [savedUsername, setSavedUsername] = useState<string | null>(null);
  const [revealed, setRevealed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | undefined>(undefined);
  const [nameError, setNameError] = useState<string | undefined>(undefined);
  const mounted = useMounted();
  /** Getting new words or storing the wallet is running. */
  const working = useRef(false);

  // Held words count as an open wallet for auto-lock; a lock ends the setup and shows the welcome screen.
  const holding = phrase !== null;
  useEffect(() => (holding ? beginSetup() : undefined), [holding]);

  const goto = (next: Step): void => {
    setError(undefined);
    setStep(next);
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
      // The words stay the same when the name or passphrase is changed later.
      const words = phrase ?? (await newRecoveryPhrase());
      const fingerprint = web ? await newWalletFingerprint(words, passphrase) : '';
      if (!mounted.current) return;
      setPhrase(words);
      setChosen({ name, passphrase, fingerprint });
      goto(web ? 'save' : 'phrase');
    } catch (e) {
      if (mounted.current) setError(errorMessage(e));
    } finally {
      working.current = false;
      if (mounted.current) setBusy(false);
    }
  };

  /** The words were confirmed: store the wallet and open it. */
  const finish = async (): Promise<void> => {
    if (!phrase || !chosen || working.current) return;
    working.current = true;
    setBusy(true);
    setError(undefined);
    try {
      await finishCreate({ name: chosen.name, mnemonic: phrase, ...(chosen.passphrase ? { passphrase: chosen.passphrase } : {}) });
    } catch (e) {
      if (!mounted.current) return;
      if (toBackendError(e).code === 'invalid_argument') {
        setNameError('Choose a different name.');
        goto('details');
      } else {
        setError(errorMessage(e));
      }
    } finally {
      working.current = false;
      if (mounted.current) setBusy(false);
    }
  };

  const words = phrase ? phrase.split(' ') : [];
  const username = chosen ? credentialUsername(chosen.name, chosen.fingerprint) : '';
  const cancelAction =
    step !== 'details' ? (
      <Button variant="ghost" size="sm" onClick={leaveOnboarding} disabled={busy}>
        Cancel
      </Button>
    ) : undefined;
  const back: Record<Step, () => void> = {
    details: () => navigate('welcome'),
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

      {step === 'phrase' && phrase ? (
        <>
          <FlowHeader
            key="phrase"
            autoFocus
            title="Write down your recovery phrase"
            subtitle="It’s the only way to restore your wallet. Never share it."
          />
          <PhraseWords words={words} revealed={revealed} onRevealedChange={setRevealed} />
          <Button variant="primary" size="lg" block disabled={!revealed} onClick={() => goto('confirm')}>
            I wrote it down
          </Button>
        </>
      ) : null}

      {step === 'confirm' && phrase ? (
        <>
          <FlowHeader key="confirm" autoFocus title="Confirm your recovery phrase" subtitle="Pick the right word for each number." />
          <ConfirmWords
            words={words}
            busy={busy}
            error={error}
            onConfirmed={() => void finish()}
            onShowWords={() => goto('phrase')}
          />
        </>
      ) : null}

      {step === 'save' && phrase && chosen ? (
        <>
          <FlowHeader
            key="save"
            autoFocus
            title="Save your recovery phrase"
            subtitle="Save it in your password manager and keep a written copy."
          />
          {savedUsername !== null && savedUsername !== username ? (
            <Banner tone="warning">Your wallet details changed. Save it again, then delete the older entry in your password manager.</Banner>
          ) : null}
          {/* The form below has its own copy button for the words. */}
          <PhraseWords words={words} revealed={revealed} onRevealedChange={setRevealed} copyable={false} />
          <SaveCredential
            username={username}
            words={phrase}
            submitLabel="Save to password manager"
            onSaved={() => {
              setSavedUsername(username);
              goto('verify');
            }}
          />
        </>
      ) : null}

      {step === 'verify' && phrase && chosen ? (
        <>
          <FlowHeader key="verify" title="Open your new wallet" subtitle="This confirms your password manager saved it." />
          <VerifyOpen
            username={username}
            fingerprint={chosen.fingerprint}
            words={phrase}
            busy={busy}
            error={error}
            onVerified={() => void finish()}
            onShowAgain={() => goto('save')}
          />
        </>
      ) : null}
    </FocusLayout>
  );
}
