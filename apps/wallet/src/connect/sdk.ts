/** Standalone ESM SDK. Each user action owns exactly one short-lived popup. */
import type { Account, TransactionRequest } from './protocol';

export interface ConnectOptions {
  walletUrl?: string;
  network?: string;
  timeoutMs?: number;
}
export interface WalletConnection {
  readonly account: Account;
  /** Call directly from a click/tap handler so the browser permits a new popup. */
  sendTransaction(
    transaction: Record<string, unknown>,
    options?: Pick<TransactionRequest, 'feeLevel' | 'gasLimit'>,
  ): Promise<{ txid: string }>;
  disconnect(): void;
}
export class WalletError extends Error {
  constructor(
    readonly code: string,
    message = code,
  ) {
    super(message);
    this.name = 'WalletError';
  }
}
const VERSION = 2;
let popupInUse = false;
function random(): string {
  return Array.from(crypto.getRandomValues(new Uint8Array(16)), (b) => b.toString(16).padStart(2, '0')).join('');
}
function record(v: unknown): v is Record<string, unknown> {
  return !!v && typeof v === 'object' && !Array.isArray(v);
}
function allowed(url: URL): boolean {
  return url.protocol === 'https:' || (url.protocol === 'http:' && ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname));
}
function json(value: unknown, depth = 0, budget = { left: 4096 }, seen = new Set<object>()): boolean {
  if (depth > 32 || --budget.left < 0) return false;
  if (value === null || typeof value === 'boolean' || typeof value === 'string') return true;
  if (typeof value === 'number') return Number.isFinite(value) && Math.abs(value) <= Number.MAX_SAFE_INTEGER;
  if (typeof value !== 'object' || seen.has(value)) return false;
  seen.add(value);
  const valid = Array.isArray(value)
    ? value.every((v) => json(v, depth + 1, budget, seen))
    : Object.getPrototypeOf(value) === Object.prototype && Object.values(value).every((v) => json(v, depth + 1, budget, seen));
  seen.delete(value);
  return valid;
}
interface PopupRequest {
  promise: Promise<Record<string, unknown>>;
  cancel: () => void;
}
function request(options: ConnectOptions, action: 'connect' | 'sendTransaction', payload: Record<string, unknown>): PopupRequest {
  const failure = (error: WalletError): PopupRequest => ({ promise: Promise.reject(error), cancel: () => undefined });
  if (popupInUse) return failure(new WalletError('busy', 'Finish the current wallet request first.'));
  if (!allowed(new URL(window.location.href))) return failure(new WalletError('insecure_origin'));
  let url: URL;
  try {
    url = new URL('connect.html', options.walletUrl ?? 'https://wallet.mmx.network/');
  } catch {
    return failure(new WalletError('invalid_wallet_url'));
  }
  if (!allowed(url) || url.username || url.password) return failure(new WalletError('invalid_wallet_url'));
  const timeout = options.timeoutMs ?? 120_000;
  if (
    !Number.isFinite(timeout) ||
    timeout < 1 ||
    timeout > 300_000 ||
    (options.network !== undefined && (typeof options.network !== 'string' || !options.network.length || options.network.length > 256))
  )
    return failure(new WalletError('invalid_argument'));
  const channel = random(),
    id = random();
  url.hash = new URLSearchParams({ channel, origin: window.location.origin }).toString();
  // Never await anything before window.open: mobile browsers require activation.
  let popup: Window | null;
  try { popup = window.open(url, `mmx-wallet-${channel}`, 'popup,width=480,height=800'); }
  catch { return failure(new WalletError('popup_blocked', 'Allow the wallet popup and try again.')); }
  if (!popup) return failure(new WalletError('popup_blocked', 'Allow the wallet popup and try again.'));
  popupInUse = true;
  let done = false,
    greeted = false,
    submitting = false;
  let resolve!: (v: Record<string, unknown>) => void;
  let reject!: (e: Error) => void;
  const promise = new Promise<Record<string, unknown>>((ok, no) => {
    resolve = ok;
    reject = no;
  });
  const send = (data: Record<string, unknown>) =>
    popup.postMessage({ protocol: 'mmx-wallet', version: VERSION, channel, ...data }, url.origin);
  const finish = (result?: Record<string, unknown>, error?: Error) => {
    if (done) return;
    done = true;
    popupInUse = false;
    clearInterval(watch);
    clearTimeout(timer);
    window.removeEventListener('message', onMessage);
    window.removeEventListener('pagehide', onLeave);
    try {
      popup.close();
    } catch {}
    if (error) reject(error);
    else resolve(result!);
  };
  const cancel = () => {
    if (done) return;
    try {
      send({ type: 'cancel', id });
    } catch {}
    finish(
      undefined,
      new WalletError(
        submitting ? 'tx_uncertain' : 'disconnected',
        submitting ? 'Check wallet Activity before requesting another transaction.' : 'disconnected',
      ),
    );
  };
  const onLeave = () => cancel();
  function onMessage(event: MessageEvent<unknown>) {
    if (event.source !== popup || event.origin !== url.origin || !record(event.data)) return;
    const m = event.data;
    if (m.protocol !== 'mmx-wallet' || m.version !== VERSION || m.channel !== channel) return;
    if (m.type === 'ready' && !greeted) {
      greeted = true;
      send({ type: 'hello', id, action, ...payload });
    } else if (m.type === 'submitting' && m.id === id) {
      submitting = true;
    } else if (m.type === 'result' && m.id === id) {
      if (record(m.error) && typeof m.error.code === 'string')
        finish(undefined, new WalletError(m.error.code, typeof m.error.message === 'string' ? m.error.message : m.error.code));
      else if (record(m.result)) finish(m.result);
      else finish(undefined, new WalletError('invalid_response'));
    } else if (m.type === 'closed')
      finish(
        undefined,
        new WalletError(
          submitting ? 'tx_uncertain' : 'user_rejected',
          submitting ? 'Check wallet Activity before requesting another transaction.' : 'Request cancelled.',
        ),
      );
  }
  const watch = setInterval(() => {
    try {
      if (popup.closed)
        finish(
          undefined,
          new WalletError(
            submitting ? 'tx_uncertain' : 'user_rejected',
            submitting ? 'Check wallet Activity before requesting another transaction.' : 'The wallet window was closed.',
          ),
        );
    } catch {
      finish(undefined, new WalletError('disconnected'));
    }
  }, 500);
  const timer = setTimeout(() => {
    try {
      send({ type: 'cancel', id });
    } catch {}
    finish(
      undefined,
      new WalletError(
        submitting ? 'tx_uncertain' : 'timeout',
        submitting ? 'Check wallet Activity before requesting another transaction.' : 'timeout',
      ),
    );
  }, timeout);
  window.addEventListener('message', onMessage);
  window.addEventListener('pagehide', onLeave);
  return { promise, cancel };
}
function account(v: unknown, network?: string): v is Account {
  return (
    record(v) &&
    typeof v.walletId === 'string' &&
    typeof v.address === 'string' &&
    Array.isArray(v.addresses) &&
    v.addresses.length >= 1 &&
    v.addresses.length <= 100 &&
    v.addresses.every((a) => typeof a === 'string') &&
    v.addresses[0] === v.address &&
    typeof v.network === 'string' &&
    (network === undefined || v.network === network)
  );
}
/** Call directly from a user gesture. COOP must be unsafe-none (the default)
 * or same-origin-allow-popups on the calling website. */
export function connect(options: ConnectOptions = {}): Promise<WalletConnection> {
  return request(options, 'connect', options.network ? { network: options.network } : {}).promise.then((result) => {
    if (
      !account(result.account, options.network) ||
      typeof result.session !== 'string' ||
      !/^[a-f0-9]{32}$/.test(result.session) ||
      typeof result.expires !== 'number'
    )
      throw new WalletError('invalid_response');
    const approved = structuredClone(result.account);
    const session = result.session,
      expires = result.expires;
    let disconnected = false;
    let active: PopupRequest | undefined;
    const disconnect = () => {
      disconnected = true;
      active?.cancel();
      active = undefined;
      window.removeEventListener('pagehide', disconnect);
    };
    window.addEventListener('pagehide', disconnect);
    return {
      get account() {
        return structuredClone(approved);
      },
      sendTransaction(transaction, opts = {}) {
        if (disconnected || Date.now() >= expires) return Promise.reject(new WalletError('disconnected', 'Connect your wallet again.'));
        if (active) return Promise.reject(new WalletError('busy', 'Finish the current wallet request first.'));
        let params: TransactionRequest;
        try {
          if (!record(transaction) || !json(transaction) || !json(opts)) throw new Error();
          const text = JSON.stringify({ transaction, network: approved.network, ...opts });
          if (new TextEncoder().encode(text).length > 65_536) throw new Error();
          params = JSON.parse(text) as TransactionRequest;
        } catch {
          return Promise.reject(new WalletError('invalid_request'));
        }
        active = request(options, 'sendTransaction', { session, params });
        return active.promise
          .then((result) => {
            if (typeof result.txid !== 'string' || !/^[a-fA-F0-9]{64}$/.test(result.txid)) throw new WalletError('invalid_response');
            return { txid: result.txid };
          })
          .finally(() => {
            active = undefined;
          });
      },
      disconnect,
    };
  });
}
export const mmxWallet = { connect };
