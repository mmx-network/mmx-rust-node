/**
 * HTTP transport to the public chain server for the web wallet. Mirrors the
 * desktop service (crates/mmxwallet): a fixed set of read endpoints with a
 * plain query string, a 20 s deadline, a response size limit, no redirects,
 * no cookies, and server text never passed on to the UI.
 */
import { BackendError, type ErrorCode } from '../types';

export type FetchLike = (input: string, init: RequestInit) => Promise<Response>;

export const RPC_TIMEOUT_MS = 20_000;
/** Same limit as the desktop service. */
export const MAX_RESPONSE_BYTES = 16 * 1024 * 1024;
/** Error bodies are only inspected for a short reason. */
const MAX_ERROR_BYTES = 4096;
const MAX_PATH_LENGTH = 512;

const READ_ROUTES: ReadonlySet<string> = new Set([
  '/node/info',
  '/chain/info',
  '/address',
  '/address/history',
  '/transaction',
]);
const QUERY = /^[A-Za-z0-9=&_-]*$/u;

/** Returns the path when the UI may request it; throws invalid_argument otherwise. */
export function checkReadPath(path: string): string {
  if (typeof path !== 'string' || path.length > MAX_PATH_LENGTH) throw new BackendError('invalid_argument');
  const q = path.indexOf('?');
  const route = q >= 0 ? path.slice(0, q) : path;
  const query = q >= 0 ? path.slice(q + 1) : '';
  if (!READ_ROUTES.has(route) || !QUERY.test(query)) throw new BackendError('invalid_argument');
  return path;
}

/**
 * Error code for a transfer the node refused (HTTP 4xx on broadcast). The
 * node's text only selects the code; it is never shown.
 */
export function rejectionCode(text: string): ErrorCode {
  const t = text.toLowerCase();
  if (t.includes('insufficient funds for transaction fee') || t.includes('insufficient funds for tx fee')) {
    return 'insufficient_fee';
  }
  if (t.includes('insufficient funds')) return 'insufficient_funds';
  if (t.includes('expired')) return 'tx_expired';
  return 'tx_rejected';
}

/**
 * Reads a body as UTF-8 text. Over `limit` bytes: throws rpc_response_invalid,
 * or with `truncate` returns the first `limit` bytes.
 */
async function readText(res: Response, limit: number, truncate: boolean): Promise<string> {
  const declared = Number(res.headers.get('content-length'));
  if (!truncate && Number.isFinite(declared) && declared > limit) {
    void res.body?.cancel().catch(() => undefined);
    throw new BackendError('rpc_response_invalid');
  }
  const decoder = new TextDecoder('utf-8');
  if (!res.body) {
    const text = await res.text();
    if (new TextEncoder().encode(text).length <= limit) return text;
    if (truncate) return text.slice(0, limit);
    throw new BackendError('rpc_response_invalid');
  }
  const reader = res.body.getReader();
  let total = 0;
  let text = '';
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    if (total + value.byteLength > limit) {
      void reader.cancel().catch(() => undefined);
      if (!truncate) throw new BackendError('rpc_response_invalid');
      text += decoder.decode(value.subarray(0, limit - total), { stream: true });
      return text + decoder.decode();
    }
    total += value.byteLength;
    text += decoder.decode(value, { stream: true });
  }
  return text + decoder.decode();
}

export interface Reply {
  /** Raw response text. */
  text: string;
  /** Parsed JSON; null for an empty body. */
  value: unknown;
}

export class RpcHttp {
  constructor(
    private readonly fetchFn: FetchLike,
    private readonly base: string,
    private readonly timeoutMs = RPC_TIMEOUT_MS,
  ) {}

  /** GET on an allowlisted read endpoint. */
  async get(path: string): Promise<Reply> {
    const text = await this.request(checkReadPath(path), undefined, () => new BackendError('rpc_http_error'));
    return { text, value: parseJson(text) };
  }

  /**
   * POST /transaction/broadcast, the only request that carries a signed
   * transfer (the node validates it before accepting). Resolves once the node
   * accepted it; a refusal rejects with tx_rejected, insufficient_funds,
   * insufficient_fee or tx_expired, and only the rpc_* codes leave the
   * outcome open.
   */
  async broadcast(txJson: string): Promise<void> {
    await this.request('/transaction/broadcast', txJson, refusal);
  }

  private async request(
    path: string,
    body: string | undefined,
    onStatus: (status: number, text: string) => BackendError,
  ): Promise<string> {
    const controller = new AbortController();
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      controller.abort();
    }, this.timeoutMs);
    const init: RequestInit = {
      method: body === undefined ? 'GET' : 'POST',
      signal: controller.signal,
      mode: 'cors',
      credentials: 'omit',
      cache: 'no-store',
      redirect: 'error',
      referrerPolicy: 'no-referrer',
    };
    // text/plain keeps the request "simple" (no CORS preflight); the node parses it as JSON.
    if (body !== undefined) {
      init.body = body;
      init.headers = { 'Content-Type': 'text/plain;charset=UTF-8' };
    }
    try {
      let res: Response;
      try {
        res = await this.fetchFn(this.base + path, init);
      } catch {
        throw new BackendError(timedOut ? 'rpc_timeout' : 'rpc_unreachable');
      }
      if (res.status < 200 || res.status > 299) {
        const detail = await readText(res, MAX_ERROR_BYTES, true).catch(() => '');
        throw onStatus(res.status, detail);
      }
      try {
        return await readText(res, MAX_RESPONSE_BYTES, false);
      } catch (e) {
        if (e instanceof BackendError) throw e;
        throw new BackendError(timedOut ? 'rpc_timeout' : 'rpc_unreachable');
      }
    } finally {
      clearTimeout(timer);
    }
  }
}

/**
 * Broadcast: a 4xx reply means the transfer was not accepted (the node
 * answers 400 with its reason); 5xx is a server problem and the transfer may
 * or may not have been accepted.
 */
function refusal(status: number, text: string): BackendError {
  return status >= 400 && status <= 499 ? new BackendError(rejectionCode(text)) : new BackendError('rpc_http_error');
}

function parseJson(text: string): unknown {
  if (text.trim() === '') return null;
  try {
    return JSON.parse(text) as unknown;
  } catch {
    throw new BackendError('rpc_response_invalid');
  }
}
