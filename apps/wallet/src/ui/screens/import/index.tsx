import { useEffect, useRef, useState } from 'preact/hooks';
import { toBackendError, type WalletInfo } from '../../../backend/types';
import { errorMessage } from '../../../lib/errors';
import { allWordsValid, joinWords, MNEMONIC_WORDS } from '../../../lib/mnemonic';
import { credentialUsername, importWallet, navigate, notify, platform, suggestWalletName } from '../../../state';
import { Banner, Button, ProgressSteps } from '../../components';
import { FocusLayout } from '../../shell';
import { SaveCredential } from '../create/SaveCredential';
import { chosenPassphrase, emptyDetails, type DetailsValue } from '../create/details';
import { WalletDetails } from '../create/WalletDetails';
import { FlowHeader, useMounted } from '../welcome/flow';
import { DiscoveryProgress } from './DiscoveryProgress';
import { WordEntry } from './WordEntry';
import { firstToFix } from './words';
import './import.css';

type Step = 'details' | 'words' | 'progress' | 'save';

const emptyWords = (): string[] => Array.from({ length: MNEMONIC_WORDS }, () => '');

export default function ImportScreen() {
  const web = platform.value === 'web';
  const [step, setStep] = useState<Step>('details');
  const [details, setDetails] = useState<DetailsValue>(() => emptyDetails(suggestWalletName()));
  const [words, setWordsState] = useState<string[]>(emptyWords);
  // Submit handlers can run right after an edit, before a re-render: keep the newest words here.
  const wordsRef = useRef(words);
  const setWords = (next: string[]): void => {
    wordsRef.current = next;
    setWordsState(next);
  };
  /** Words before the last Clear, for Undo; dropped on the next edit and when leaving. */
  const cleared = useRef<string[] | null>(null);
  const [canUndo, setCanUndo] = useState(false);
  const forgetCleared = (): void => {
    cleared.current = null;
    setCanUndo(false);
  };
  const [showErrors, setShowErrors] = useState(false);
  const [error, setError] = useState<string | undefined>(undefined);
  const [nameError, setNameError] = useState<string | undefined>(undefined);
  const [imported, setImported] = useState<WalletInfo | null>(null);
  const mounted = useMounted();
  const importing = useRef(false);
  const steps = web ? ['Name', 'Recovery phrase', 'Save'] : ['Name', 'Recovery phrase'];

  const runImport = async (): Promise<void> => {
    if (importing.current) return;
    const current = wordsRef.current;
    setShowErrors(true);
    if (!allWordsValid(current)) return;
    importing.current = true;
    setError(undefined);
    setStep('progress');
    const passphrase = chosenPassphrase(details);
    const request = { name: details.name.trim(), mnemonic: joinWords(current), ...(passphrase ? { passphrase } : {}) };
    try {
      // Web stays here to save the wallet in the password manager.
      const info = await importWallet(request, web ? { navigateTo: null } : {});
      if (web && mounted.current) {
        setImported(info);
        setStep('save');
      } else {
        notify('Wallet imported', 'success');
      }
    } catch (e) {
      if (!mounted.current) return;
      const code = toBackendError(e).code;
      if (code === 'invalid_argument') {
        setNameError('Choose a different name.');
        setStep('details');
      } else {
        setError(errorMessage(e));
        setStep('words');
      }
    } finally {
      importing.current = false;
    }
  };

  const finish = (): void => {
    navigate('overview');
    notify('Wallet imported', 'success');
  };

  if (step === 'progress') {
    return <DiscoveryProgress />;
  }

  if (step === 'save' && imported) {
    return (
      <FocusLayout class="import">
        <ProgressSteps steps={steps} current={2} label="Import a wallet" />
        <FlowHeader autoFocus title="Save it in your password manager" subtitle="Next time, open this wallet in one step." />
        <SaveCredential
          username={credentialUsername(imported.name, imported.fingerprint)}
          words={joinWords(words)}
          submitLabel="Save to password manager"
          onSaved={finish}
        >
          <div class="onboarding-actions__secondary">
            <Button variant="ghost" onClick={finish}>
              Not now
            </Button>
          </div>
        </SaveCredential>
      </FocusLayout>
    );
  }

  if (step === 'words') {
    return (
      <WordsStep
        steps={steps}
        words={words}
        latestWords={() => wordsRef.current}
        onWords={(next) => {
          setWords(next);
          if (cleared.current) forgetCleared();
          if (error) setError(undefined);
        }}
        showErrors={showErrors}
        error={error}
        onBack={() => {
          setError(undefined);
          setStep('details');
        }}
        canUndo={canUndo}
        onClear={() => {
          cleared.current = wordsRef.current;
          setCanUndo(true);
          setWords(emptyWords());
          setShowErrors(false);
          setError(undefined);
        }}
        onUndo={() => {
          const previous = cleared.current;
          forgetCleared();
          if (previous) setWords(previous);
        }}
        onSubmit={() => void runImport()}
      />
    );
  }

  return (
    <FocusLayout onBack={() => navigate('welcome')} class="import">
      <ProgressSteps steps={steps} current={0} label="Import a wallet" />
      <FlowHeader title="Import a wallet" />
      <WalletDetails
        mode="import"
        value={details}
        onChange={(value) => {
          setDetails(value);
          if (nameError && value.name !== details.name) setNameError(undefined);
        }}
        nameError={nameError}
        submitLabel="Continue"
        onSubmit={() => setStep('words')}
      />
    </FocusLayout>
  );
}

interface WordsStepProps {
  steps: string[];
  words: string[];
  /** Newest words (may be ahead of `words` inside one event). */
  latestWords: () => string[];
  onWords: (words: string[]) => void;
  showErrors: boolean;
  error?: string;
  /** The words were just cleared: offer Undo instead of Clear. */
  canUndo: boolean;
  onBack: () => void;
  onClear: () => void;
  onUndo: () => void;
  onSubmit: () => void;
}

function WordsStep({ steps, words, latestWords, onWords, showErrors, error, canUndo, onBack, onClear, onUndo, onSubmit }: WordsStepProps) {
  const inputs = useRef<(HTMLInputElement | null)[]>([]);
  const formRef = useRef<HTMLFormElement>(null);
  const any = words.some(Boolean);

  // Start in the first empty box.
  useEffect(() => {
    const index = words.findIndex((w) => !w);
    const id = requestAnimationFrame(() => {
      const active = document.activeElement;
      if (active && active !== document.body && active !== document.documentElement) return;
      inputs.current[index >= 0 ? index : 0]?.focus({ preventScroll: true });
    });
    return () => cancelAnimationFrame(id);
  }, []);

  const submit = (event?: Event): void => {
    event?.preventDefault();
    onSubmit();
    const index = firstToFix(latestWords());
    // Go to the first box to fix.
    if (index >= 0) requestAnimationFrame(() => inputs.current[index]?.focus());
  };

  return (
    <FocusLayout width="wide" onBack={onBack} class="import">
      <ProgressSteps steps={steps} current={1} label="Import a wallet" />
      <FlowHeader title="Enter your recovery phrase" subtitle={`Type or paste your ${MNEMONIC_WORDS} words.`} />
      <form ref={formRef} class="onboarding-form import-form" autocomplete="off" noValidate onSubmit={submit}>
        <WordEntry
          words={words}
          onWords={onWords}
          showErrors={showErrors}
          inputs={inputs}
          onDone={(latest, reason) => {
            if (reason === 'enter') submit();
            else if (allWordsValid(latest)) formRef.current?.querySelector<HTMLButtonElement>('button[type="submit"]')?.focus();
          }}
        />
        {error ? <Banner tone="danger">{error}</Banner> : null}
        <div class="import-form__actions">
          {canUndo ? (
            <Button variant="ghost" icon="refresh" onClick={onUndo}>
              Undo clear
            </Button>
          ) : (
            <Button
              variant="ghost"
              icon="close"
              onClick={() => {
                onClear();
                requestAnimationFrame(() => inputs.current[0]?.focus());
              }}
              disabled={!any}
            >
              Clear
            </Button>
          )}
          <Button type="submit" variant="primary" size="lg" class="import-form__submit">
            Import wallet
          </Button>
        </div>
      </form>
    </FocusLayout>
  );
}
