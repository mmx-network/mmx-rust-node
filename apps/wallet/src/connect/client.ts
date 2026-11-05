import { CHANNEL, REQUEST_TIMEOUT, ConnectionError, boundedJson, object, randomToken, token, type Offer } from './protocol';

/** The connection page's private channel to wallet tabs. Its bridge ID is never
 * given to the website; no unlock credentials can be carried on this protocol. */
export class WalletClient {
  private closed = false;
  readonly id = randomToken();
  private readonly channel = new BroadcastChannel(CHANNEL);
  private readonly offers = new Map<string, { rows: Offer[]; seen: number }>();
  private readonly pending = new Map<
    string,
    { host: string; resolve: (v: unknown) => void; reject: (e: Error) => void; timer: ReturnType<typeof setTimeout> }
  >();
  private readonly timer: ReturnType<typeof setInterval>;
  onOffers: (offers: Offer[]) => void = () => undefined;
  onRevoked: (code: string) => void = () => undefined;
  constructor() {
    this.channel.onmessage = ({ data: m }) => {
      if (!object(m) || !token(m.host) || !boundedJson(m, 72_000) || (m.bridge !== undefined && m.bridge !== this.id)) return;
      if (m.type === 'offer' && Array.isArray(m.wallets)) {
        const rows: Offer[] = [];
        for (const w of m.wallets.slice(0, 100))
          if (object(w) && typeof w.id === 'string' && typeof w.name === 'string' && Number.isInteger(w.addressCount)) {
            rows.push({ host: m.host, wallet: { id: w.id, name: w.name, addressCount: Number(w.addressCount) } });
          }
        this.offers.set(m.host, { rows, seen: Date.now() });
        this.publish();
      } else if (m.type === 'revoked' && m.bridge === this.id && typeof m.code === 'string') {
        this.onRevoked(m.code);
      } else if (m.type === 'gone') {
        this.offers.delete(m.host);
        this.publish();
        for (const [key, p] of this.pending)
          if (p.host === m.host) {
            clearTimeout(p.timer);
            this.pending.delete(key);
            p.reject(new ConnectionError('disconnected'));
          }
      } else if (m.type === 'reply' && token(m.requestId)) {
        const p = this.pending.get(m.requestId);
        if (!p || p.host !== m.host || m.bridge !== this.id) return;
        clearTimeout(p.timer);
        this.pending.delete(m.requestId);
        if (object(m.error) && typeof m.error.code === 'string')
          p.reject(new ConnectionError(m.error.code, typeof m.error.message === 'string' ? m.error.message : m.error.code));
        else p.resolve(m.result);
      }
    };
    this.timer = setInterval(() => this.discover(), 1500);
    this.discover();
  }
  private publish() {
    for (const [key, v] of this.offers) if (Date.now() - v.seen > 5000) this.offers.delete(key);
    this.onOffers([...this.offers.values()].flatMap((v) => v.rows));
  }
  discover(): void {
    if (this.closed) return;
    this.channel.postMessage({ type: 'discover', bridge: this.id });
    this.publish();
  }
  call(host: string, walletId: string, origin: string, method: string, extra: Record<string, unknown> = {}): Promise<unknown> {
    if (this.closed) return Promise.reject(new ConnectionError('disconnected'));
    if (this.pending.size >= 2) return Promise.reject(new ConnectionError('busy'));
    const requestId = randomToken();
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(requestId);
        this.cancel(host);
        reject(new ConnectionError('timeout'));
      }, REQUEST_TIMEOUT);
      this.pending.set(requestId, { host, resolve, reject, timer });
      this.channel.postMessage({ type: 'call', bridge: this.id, requestId, host, walletId, origin, method, sentAt: Date.now(), ...extra });
    });
  }
  cancel(host: string): void {
    if (!this.closed) this.channel.postMessage({ type: 'cancel', bridge: this.id, host });
  }
  close(): void {
    if (this.closed) return;
    clearInterval(this.timer);
    for (const host of this.offers.keys()) this.cancel(host);
    for (const p of this.pending.values()) {
      this.cancel(p.host);
      clearTimeout(p.timer);
      p.reject(new ConnectionError('disconnected'));
    }
    this.closed = true;
    this.pending.clear();
    this.channel.close();
  }
}
