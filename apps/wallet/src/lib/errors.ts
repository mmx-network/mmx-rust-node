/** One-line, user-facing messages for backend error codes. */
import { toBackendError, type ErrorCode } from '../backend/types';

const MESSAGES: Record<ErrorCode, string> = {
  invalid_passphrase: 'Wrong passphrase. Try again.',
  passphrase_required: 'Enter the passphrase for this wallet.',
  invalid_mnemonic: 'These recovery words are not valid. Check each word and the order.',
  wallet_mismatch: 'These recovery words belong to a different wallet.',
  wallet_exists: 'This wallet is already added.',
  wallet_not_found: 'This wallet is no longer available.',
  locked: 'Unlock the wallet to continue.',
  invalid_address: 'Enter a valid MMX address.',
  invalid_amount: 'Enter a valid amount.',
  invalid_memo: 'The memo is too long.',
  insufficient_funds: 'Not enough funds for this amount and the network fee.',
  insufficient_fee: 'Not enough MMX to pay the network fee.',
  invalid_argument: 'Some details are not valid. Check them and try again.',
  rpc_unreachable: 'Cannot reach the network. Check your connection and try again.',
  rpc_timeout: 'The network is not responding. Try again in a moment.',
  rpc_not_synced: 'The network server is catching up. Try again in a moment.',
  rpc_network_mismatch: 'The network server is on a different network.',
  rpc_http_error: 'The network server reported a problem. Try again in a moment.',
  rpc_response_invalid: 'The network server sent an unexpected response. Try again later.',
  tx_rejected: 'The network rejected this transfer. Nothing was sent.',
  tx_expired: 'This transfer expired. Review it again.',
  tx_not_sent: 'Could not check this transfer with the network. Nothing was sent. Try again.',
  prepared_not_found: 'This transfer expired. Review it again.',
  io_error: 'Could not save your changes. Check disk space and permissions.',
  internal: 'Something went wrong. Try again.',
};

export function errorMessage(e: unknown): string {
  return MESSAGES[toBackendError(e).code];
}

export function messageForCode(code: ErrorCode): string {
  return MESSAGES[code];
}

/** Errors caused by the connection rather than by the request itself. */
export const TRANSPORT_ERRORS: ReadonlySet<ErrorCode> = new Set<ErrorCode>([
  'rpc_unreachable',
  'rpc_timeout',
  'rpc_http_error',
  'rpc_response_invalid',
  'rpc_not_synced',
  'rpc_network_mismatch',
]);

export function isTransportError(e: unknown): boolean {
  return TRANSPORT_ERRORS.has(toBackendError(e).code);
}
