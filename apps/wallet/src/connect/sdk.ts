/** Standalone browser SDK, also emitted as /mmx-wallet.js. No dependencies,
 * storage, relay, seed handling, injected provider or background connection. */
import type { Account, TransactionRequest } from './protocol';

export interface ConnectOptions {
  walletUrl?: string;
  network?: string;
  timeoutMs?: number;
}
export interface WalletConnection {
  readonly account: Account;
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

/** Call directly from a click/tap handler so the browser permits the popup.
 * The caller must use COOP unsafe-none (the default) or same-origin-allow-popups. */
export function connect(options: ConnectOptions = {}): Promise<WalletConnection> {
  if (!allowed(new URL(window.location.href))) return Promise.reject(new WalletError('insecure_origin'));
  let url: URL;
  try {
    url = new URL('connect.html', options.walletUrl ?? 'https://wallet.mmx.network/');
  } catch {
    return Promise.reject(new WalletError('invalid_wallet_url'));
  }
  if (!allowed(url) || url.username || url.password) return Promise.reject(new WalletError('invalid_wallet_url'));
  const timeout = options.timeoutMs ?? 120_000;
  if (!Number.isFinite(timeout) || timeout < 1 || timeout > 300_000) return Promise.reject(new WalletError('invalid_argument'));
  const channel = random();
  const connectId = random();
  url.hash = new URLSearchParams({ channel, origin: window.location.origin }).toString();
  const popup = window.open(url, `mmx-wallet-${channel}`, 'popup,width=540,height=800');
  if (!popup) return Promise.reject(new WalletError('popup_blocked', 'Allow the wallet popup and try again.'));
  let closed = false;
  let greeted = false;
  let account: Account | undefined;
  const pending = new Map<string, { resolve: (v: unknown) => void; reject: (e: Error) => void; timer: ReturnType<typeof setTimeout> }>();
  const send = (v: Record<string, unknown>) => popup.postMessage({ protocol: 'mmx-wallet', version: 1, channel, ...v }, url.origin);
  let rejectConnect: (e: Error) => void = () => undefined;
  let resolveConnect: (c: WalletConnection) => void = () => undefined;
  const cleanup = (error: Error) => {
    if (closed) return;
    closed = true;
    clearInterval(watch);
    clearTimeout(connectTimer);
    window.removeEventListener('message', onMessage);
    window.removeEventListener('pagehide', onLeave);
    rejectConnect(error);
    for (const p of pending.values()) {
      clearTimeout(p.timer);
      p.reject(error);
    }
    pending.clear();
  };
  const onLeave = () => {
    try {
      send({ type: 'disconnect' });
    } catch {}
    cleanup(new WalletError('disconnected'));
  };
  const connection: WalletConnection = {
    get account() {
      return account!;
    },
    sendTransaction(transaction, opts = {}) {
      if (closed || !account) return Promise.reject(new WalletError('disconnected'));
      if (pending.size) return Promise.reject(new WalletError('busy', 'Finish the current wallet request first.'));
      let params: TransactionRequest;
      try {
        if (!record(transaction) || !json(transaction) || !json(opts)) throw new Error();
        const encoded = JSON.stringify({ transaction, network: account.network, ...opts });
        if (new TextEncoder().encode(encoded).length > 65_536) throw new Error();
        params = JSON.parse(encoded) as TransactionRequest;
      } catch {
        return Promise.reject(new WalletError('invalid_request'));
      }
      const id = random();
      popup.focus();
      return new Promise<{ txid: string }>((resolve, reject) => {
        const timer = setTimeout(() => {
          pending.delete(id);
          send({ type: 'cancel', id });
          reject(new WalletError('timeout'));
        }, timeout);
        pending.set(id, { resolve: (v) => resolve(v as { txid: string }), reject, timer });
        send({ type: 'request', id, method: 'sendTransaction', params });
      });
    },
    disconnect() {
      if (!closed) {
        send({ type: 'disconnect' });
        cleanup(new WalletError('disconnected'));
        popup.close();
      }
    },
  };
  function onMessage(event: MessageEvent<unknown>) {
    if (event.source !== popup || event.origin !== url.origin || !record(event.data)) return;
    const m = event.data;
    if (m.protocol !== 'mmx-wallet' || m.version !== 1 || m.channel !== channel) return;
    if (m.type === 'ready' && !greeted) {
      greeted = true;
      send({ type: 'hello', id: connectId, ...(options.network ? { network: options.network } : {}) });
    } else if (m.type === 'closed') cleanup(new WalletError('disconnected'));
    else if (m.type === 'connected' && m.id === connectId && !account) {
      if (
        !record(m.account) ||
        typeof m.account.walletId !== 'string' ||
        typeof m.account.address !== 'string' ||
        !Array.isArray(m.account.addresses) ||
        m.account.addresses.length < 1 ||
        m.account.addresses.length > 100 ||
        !m.account.addresses.every((a) => typeof a === 'string') ||
        m.account.address !== m.account.addresses[0] ||
        typeof m.account.network !== 'string' ||
        (options.network !== undefined && m.account.network !== options.network)
      )
        return;
      account = m.account as unknown as Account;
      clearTimeout(connectTimer);
      resolveConnect(connection);
    } else if (m.type === 'result' && typeof m.id === 'string') {
      const error =
        record(m.error) && typeof m.error.code === 'string'
          ? new WalletError(m.error.code, typeof m.error.message === 'string' ? m.error.message : m.error.code)
          : undefined;
      if (m.id === connectId && !account && error) {
        cleanup(error);
        return;
      }
      const p = pending.get(m.id);
      if (!p) return;
      clearTimeout(p.timer);
      pending.delete(m.id);
      if (error) p.reject(error);
      else if (record(m.result) && typeof m.result.txid === 'string' && /^[a-fA-F0-9]{64}$/.test(m.result.txid))
        p.resolve({ txid: m.result.txid });
      else p.reject(new WalletError('invalid_response'));
    }
  }
  const watch = setInterval(() => {
    try {
      if (popup.closed) cleanup(new WalletError('disconnected', 'The wallet window closed or its opener policy blocked the connection.'));
    } catch {
      cleanup(new WalletError('disconnected'));
    }
  }, 1000);
  const connectTimer = setTimeout(() => {
    cleanup(new WalletError('timeout'));
    popup.close();
  }, timeout);
  window.addEventListener('message', onMessage);
  window.addEventListener('pagehide', onLeave);
  return new Promise<WalletConnection>((resolve, reject) => {
    resolveConnect = resolve;
    rejectConnect = reject;
  });
}
export const mmxWallet = { connect };
