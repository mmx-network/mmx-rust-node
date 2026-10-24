/**
 * Web: opening a wallet with the recovery words from the password manager,
 * followed by a passphrase step when the wallet needs one (§6.3, §6.6).
 * Used by the unlock screen, "Open a saved wallet" and the confirmation step
 * of a new wallet.
 */
import { useEffect, useRef, useState } from 'preact/hooks';
import { toBackendError, type WalletInfo } from '../../../backend/types';
import { errorMessage, messageForCode } from '../../../lib/errors';
import { allWordsValid, joinWords, splitWords } from '../../../lib/mnemonic';
import {
  openSavedWallet,
  parseCredentialUsername,
  passwordManagerApi,
  requestWalletCredential,
  unlockWallet,
  type StoredCredential,
} from '../../../state';
import { useMounted } from '../welcome/flow';

/** A wallet known on this device, or any wallet saved in the password manager. */
export type OpenTarget = { kind: 'known'; wallet: WalletInfo } | { kind: 'saved' };

export interface OpenErrors {
  words?: string;
  passphrase?: string;
  general?: string;
}

type Pending = { kind: 'known'; id: string; words: string } | { kind: 'saved'; username: string; words: string };

export interface OpenWalletOptions {
  /** Passphrase already known (a wallet created a moment ago): no passphrase step. */
  knownPassphrase?: string;
  /**
   * Offer the browser's saved wallets on mount (Chromium): 'once' only the
   * first time in this page (app start, not again after locking), 'always'
   * when the view was opened by the user.
   */
  autoRequest?: 'once' | 'always';
  /** A credential of another wallet picked in the browser's chooser opens that wallet (default true). */
  allowOtherWallet?: boolean;
  onOpened?: (wallet: WalletInfo) => void;
}

export interface OpenWallet {
  step: 'words' | 'passphrase';
  busy: boolean;
  errors: OpenErrors;
  /** The passphrase step may be left empty (the saved username has no wallet ID). */
  passphraseOptional: boolean;
  username: string;
  words: string;
  setUsername: (value: string) => void;
  setWords: (value: string) => void;
  /** Submits the credential form (values read from the inputs). */
  submitWords: (username: string, words: string) => void;
  submitPassphrase: (passphrase: string) => void;
  /** Leaves the passphrase step. */
  back: () => void;
  /** Wallet name for the passphrase step. */
  pendingName: string | undefined;
}

const ENTER_WORDS = 'Enter your recovery phrase.';
const ENTER_PASSPHRASE = 'Enter your passphrase.';

/** The browser's chooser was already offered (or a wallet opened) in this page: 'once' stays quiet. */
let offeredOnStart = false;

export function useOpenWallet(target: OpenTarget, options: OpenWalletOptions = {}): OpenWallet {
  const [step, setStep] = useState<'words' | 'passphrase'>('words');
  const [busy, setBusy] = useState(false);
  const [errors, setErrors] = useState<OpenErrors>({});
  const [optional, setOptional] = useState(false);
  const [username, setUsername] = useState('');
  const [words, setWords] = useState('');
  const [pendingName, setPendingName] = useState<string | undefined>(undefined);
  const pending = useRef<Pending | null>(null);
  const running = useRef(false);
  /** The browser's chooser is open (one request at a time). */
  const requesting = useRef(false);
  const mounted = useMounted();
  const targetKey = target.kind === 'known' ? target.wallet.id : '';

  // A different wallet was selected: start over. Not on mount, where it would
  // wipe words a password manager filled in before the effect ran.
  const shownKey = useRef(targetKey);
  useEffect(() => {
    if (shownKey.current === targetKey) return;
    shownKey.current = targetKey;
    pending.current = null;
    setStep('words');
    setErrors({});
    setWords('');
  }, [targetKey]);

  useEffect(
    () => () => {
      pending.current = null;
    },
    [],
  );

  const run = async (p: Pending, passphrase: string | undefined, fromPassphraseStep: boolean): Promise<void> => {
    if (running.current) return;
    running.current = true;
    setBusy(true);
    setErrors({});
    try {
      const secret = passphrase ? { passphrase } : {};
      const info =
        p.kind === 'known'
          ? await unlockWallet(p.id, { mnemonic: p.words, ...secret })
          : await openSavedWallet({ username: p.username, words: p.words, ...secret });
      pending.current = null;
      offeredOnStart = true;
      if (mounted.current) {
        setWords('');
        setStep('words');
      }
      options.onOpened?.(info);
    } catch (e) {
      if (!mounted.current) return;
      const code = toBackendError(e).code;
      if (code === 'passphrase_required') {
        pending.current = p;
        setOptional(false);
        setStep('passphrase');
        if (fromPassphraseStep) setErrors({ passphrase: ENTER_PASSPHRASE });
      } else if (code === 'invalid_passphrase' && fromPassphraseStep) {
        setErrors({ passphrase: errorMessage(e) });
      } else if (code === 'invalid_passphrase' && passphrase !== undefined) {
        // The passphrase was known to be right (just chosen): the words belong to another wallet.
        pending.current = null;
        setStep('words');
        setErrors({ words: messageForCode('wallet_mismatch') });
      } else if (code === 'wallet_mismatch' || code === 'invalid_mnemonic') {
        pending.current = null;
        setStep('words');
        setErrors({ words: errorMessage(e) });
      } else {
        setErrors({ general: errorMessage(e) });
      }
    } finally {
      running.current = false;
      if (mounted.current) setBusy(false);
    }
  };

  const nameFor = (p: Pending): string | undefined =>
    p.kind === 'known' ? (target.kind === 'known' ? target.wallet.name : undefined) : parseCredentialUsername(p.username).name;

  const start = (rawUsername: string, rawWords: string): void => {
    const list = splitWords(rawWords);
    if (list.length === 0) {
      setErrors({ words: ENTER_WORDS });
      return;
    }
    if (!allWordsValid(list)) {
      setErrors({ words: messageForCode('invalid_mnemonic') });
      return;
    }
    const phrase = joinWords(list);
    let p: Pending;
    if (target.kind === 'known') {
      const parsed = parseCredentialUsername(rawUsername);
      // A credential picked in the browser's chooser may belong to another wallet: open that one.
      p =
        options.allowOtherWallet !== false &&
        rawUsername &&
        parsed.fingerprint &&
        parsed.fingerprint !== target.wallet.fingerprint
          ? { kind: 'saved', username: rawUsername, words: phrase }
          : { kind: 'known', id: target.wallet.id, words: phrase };
    } else {
      p = { kind: 'saved', username: rawUsername.trim(), words: phrase };
    }
    setPendingName(nameFor(p));
    if (p.kind === 'saved' && !parseCredentialUsername(p.username).fingerprint) {
      // Without a wallet ID the passphrase cannot be detected: ask, optionally.
      pending.current = p;
      setErrors({});
      setOptional(true);
      setStep('passphrase');
      return;
    }
    void run(p, p.kind === 'known' ? options.knownPassphrase : undefined, false);
  };

  /** Resolves true when a saved wallet was picked (or the chooser is already open). */
  const requestFromBrowser = async (): Promise<boolean> => {
    if (requesting.current) return true;
    if (!passwordManagerApi() || running.current) return false;
    requesting.current = true;
    let credential: StoredCredential | null;
    try {
      credential = await requestWalletCredential();
    } finally {
      requesting.current = false;
    }
    if (!credential || !mounted.current || running.current || pending.current) return false;
    if (target.kind === 'saved') setUsername(credential.username);
    setWords(credential.words);
    start(credential.username, credential.words);
    return true;
  };

  useEffect(() => {
    const mode = options.autoRequest;
    if (!mode || !passwordManagerApi()) return;
    if (mode === 'once' && offeredOnStart) return;
    offeredOnStart = true;
    void requestFromBrowser();
  }, [targetKey]);

  const submitWords = (rawUsername: string, rawWords: string): void => {
    if (running.current) return;
    setWords(rawWords);
    if (target.kind === 'saved') setUsername(rawUsername);
    if (!rawWords.trim() && passwordManagerApi()) {
      void requestFromBrowser().then((used) => {
        if (!used && mounted.current) setErrors({ words: ENTER_WORDS });
      });
      return;
    }
    start(target.kind === 'saved' ? rawUsername : '', rawWords);
  };

  const submitPassphrase = (passphrase: string): void => {
    const p = pending.current;
    if (!p) {
      setStep('words');
      return;
    }
    if (!passphrase && !optional) {
      setErrors({ passphrase: ENTER_PASSPHRASE });
      return;
    }
    void run(p, passphrase || undefined, true);
  };

  const back = (): void => {
    if (running.current) return;
    pending.current = null;
    setErrors({});
    setStep('words');
  };

  return {
    step,
    busy,
    errors,
    passphraseOptional: optional,
    username,
    words,
    setUsername,
    setWords,
    submitWords,
    submitPassphrase,
    back,
    pendingName,
  };
}
