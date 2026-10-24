/** Name and passphrase rules for creating or importing a wallet, and the web phrase check (no UI). */
import { WALLET_NAME_MAX } from '../../../lib/appstate';
import { messageForCode } from '../../../lib/errors';
import { allWordsValid, joinWords, splitWords } from '../../../lib/mnemonic';
import { validateName } from '../../../lib/text';

export interface DetailsValue {
  name: string;
  usePassphrase: boolean;
  passphrase: string;
  repeat: string;
}

export interface DetailsErrors {
  name?: string;
  passphrase?: string;
  repeat?: string;
}

const NAME_ERRORS = {
  empty: 'Enter a name.',
  too_long: `Use ${WALLET_NAME_MAX} characters or fewer.`,
  invalid: 'This name contains characters that can’t be used.',
} as const;

export function emptyDetails(name: string): DetailsValue {
  return { name, usePassphrase: false, passphrase: '', repeat: '' };
}

/** The passphrase that will be used ('' = none). Never trimmed: every character counts. */
export function chosenPassphrase(value: DetailsValue): string {
  return value.usePassphrase ? value.passphrase : '';
}

/**
 * Validation of the details step. A passphrase is always entered twice: a
 * typo would silently make (or import) a different, empty wallet.
 */
export function detailsErrors(value: DetailsValue): DetailsErrors {
  const errors: DetailsErrors = {};
  const nameError = validateName(value.name, WALLET_NAME_MAX);
  if (nameError) errors.name = NAME_ERRORS[nameError];
  if (value.usePassphrase) {
    if (!value.passphrase) errors.passphrase = 'Enter a passphrase.';
    else if (value.repeat !== value.passphrase) errors.repeat = 'The passphrases don’t match.';
  }
  return errors;
}

export const ENTER_WORDS = 'Enter your recovery phrase.';
const OTHER_WORDS = 'These words don’t match your new recovery phrase.';

/**
 * Web create: why the filled-in or typed words do not confirm the new
 * recovery phrase (`words`, canonical form); undefined when they do.
 */
export function verifyProblem(typed: string, words: string): string | undefined {
  const list = splitWords(typed);
  if (list.length === 0) return ENTER_WORDS;
  if (!allWordsValid(list)) return messageForCode('invalid_mnemonic');
  return joinWords(list) === words ? undefined : OTHER_WORDS;
}
