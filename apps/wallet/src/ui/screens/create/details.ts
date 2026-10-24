/** Name and passphrase rules for creating or importing a wallet (no UI). */
import { WALLET_NAME_MAX } from '../../../lib/appstate';
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

/** Validation of the details step. `confirm`: the passphrase is entered twice (new wallets). */
export function detailsErrors(value: DetailsValue, confirm: boolean): DetailsErrors {
  const errors: DetailsErrors = {};
  const nameError = validateName(value.name, WALLET_NAME_MAX);
  if (nameError) errors.name = NAME_ERRORS[nameError];
  if (value.usePassphrase) {
    if (!value.passphrase) errors.passphrase = 'Enter a passphrase.';
    else if (confirm && value.repeat !== value.passphrase) errors.repeat = 'The passphrases don’t match.';
  }
  return errors;
}
