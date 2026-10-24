/** Small UI actions: clipboard and explorer links. */
import { addressUrl, transactionUrl } from '../lib/explorer';
import { errorMessage } from '../lib/errors';
import { backend } from './core';
import { notify } from './toasts';

/** Copies text. Sensitive text (recovery words) is cleared from the clipboard after a minute. */
export async function copyToClipboard(text: string, options: { sensitive?: boolean; message?: string } = {}): Promise<boolean> {
  try {
    await backend().copyText(text, options.sensitive === true);
    notify(options.message ?? (options.sensitive ? 'Copied. The clipboard clears in 1 minute.' : 'Copied'), 'success', 2500);
    return true;
  } catch (e) {
    notify(errorMessage(e), 'error');
    return false;
  }
}

/** Copies an address with an "Address copied" confirmation; throws when it could not be copied (CopyButton shows nothing then). */
export async function copyAddress(address: string): Promise<void> {
  if (!(await copyToClipboard(address, { message: 'Address copied' }))) throw new Error('copy failed');
}

export async function openTransaction(txid: string): Promise<void> {
  const url = transactionUrl(txid);
  if (url) await backend().openExternal(url).catch((e: unknown) => notify(errorMessage(e), 'error'));
}

export async function openAddress(address: string): Promise<void> {
  const url = addressUrl(address);
  if (url) await backend().openExternal(url).catch((e: unknown) => notify(errorMessage(e), 'error'));
}
