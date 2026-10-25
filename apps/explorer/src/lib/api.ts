import { rpcUrl } from './config';

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status = 0,
  ) {
    super(message);
  }
}

export const MAX_REPLY_BYTES = 16 * 1024 * 1024;
const TIMEOUT_MS = 20_000;
const ALLOWED = new Set([
  '/node/info',
  '/headers',
  '/header',
  '/block',
  '/transactions',
  '/transaction',
  '/farmers',
  '/farmer',
  '/farmer/blocks',
  '/balance',
  '/contract',
  '/address/history',
]);

export function endpoint(path: string, params: Record<string, string | number> = {}): string {
  if (!ALLOWED.has(path)) throw new Error('Unknown explorer endpoint');
  const query = new URLSearchParams(Object.entries(params).map(([key, value]) => [key, String(value)]));
  return `${path}${query.size ? `?${query}` : ''}`;
}

interface Pending {
  controller: AbortController;
  promise: Promise<unknown>;
  users: number;
}

/** In-flight GET sharing only. Nothing is retained after a request completes. */
export class ApiClient {
  private readonly pending = new Map<string, Pending>();
  readonly base: string;
  constructor(
    base?: string,
    private readonly transport: typeof fetch = (input, init) => fetch(input, init),
  ) {
    this.base = rpcUrl(base);
  }

  get<T>(path: string, signal?: AbortSignal): Promise<T | null> {
    if (!ALLOWED.has(path.split('?')[0] ?? '') || path.includes('#'))
      return Promise.reject(new Error('Unknown explorer endpoint'));
    if (signal?.aborted) return Promise.reject(new DOMException('Aborted', 'AbortError'));
    let entry = this.pending.get(path);
    if (!entry) {
      const controller = new AbortController();
      entry = { controller, users: 0, promise: this.read(path, controller) };
      const current = entry;
      this.pending.set(path, entry);
      void entry.promise
        .finally(() => {
          if (this.pending.get(path) === current) this.pending.delete(path);
        })
        .catch(() => {});
    }
    const current = entry;
    current.users++;
    return new Promise<T | null>((resolve, reject) => {
      let done = false;
      const release = (): boolean => {
        if (done) return false;
        done = true;
        signal?.removeEventListener('abort', abort);
        if (--current.users === 0) {
          if (this.pending.get(path) === current) this.pending.delete(path);
          current.controller.abort();
        }
        return true;
      };
      const abort = (): void => {
        if (release()) reject(new DOMException('Aborted', 'AbortError'));
      };
      signal?.addEventListener('abort', abort, { once: true });
      current.promise.then(
        (value) => {
          if (release()) resolve(value as T | null);
        },
        (error) => {
          if (release()) reject(error);
        },
      );
    });
  }

  private async read(path: string, controller: AbortController): Promise<unknown> {
    const timer = setTimeout(() => controller.abort(), TIMEOUT_MS);
    try {
      const response = await this.transport(`${this.base}${path}`, {
        signal: controller.signal,
        credentials: 'omit',
        cache: 'no-store',
        referrerPolicy: 'no-referrer',
        headers: { Accept: 'application/json' },
        redirect: 'error',
      });
      if (response.status === 204 || response.status === 404) {
        await response.body?.cancel();
        return null;
      }
      if (!response.ok) {
        await response.body?.cancel();
        throw new ApiError(
          response.status === 503
            ? 'The chain server is syncing. Please try again shortly.'
            : `The chain server returned HTTP ${response.status}.`,
          response.status,
        );
      }
      if (Number(response.headers.get('content-length')) > MAX_REPLY_BYTES) {
        await response.body?.cancel();
        throw new ApiError('The server response is too large.');
      }
      const reader = response.body?.getReader();
      if (!reader) throw new ApiError('The chain server returned an empty response.');
      const decoder = new TextDecoder();
      let size = 0;
      let body = '';
      try {
        while (true) {
          const next = await reader.read();
          if (next.done) break;
          size += next.value.byteLength;
          if (size > MAX_REPLY_BYTES) {
            await reader.cancel();
            throw new ApiError('The server response is too large.');
          }
          body += decoder.decode(next.value, { stream: true });
        }
        body += decoder.decode();
      } finally {
        reader.releaseLock();
      }
      try {
        return JSON.parse(body) as unknown;
      } catch {
        throw new ApiError('The chain server returned invalid JSON.');
      }
    } catch (error) {
      if (controller.signal.aborted) throw new ApiError('The request timed out. Please try again.');
      if (error instanceof ApiError) throw error;
      throw new ApiError('Could not reach the chain server. Check your connection and try again.');
    } finally {
      clearTimeout(timer);
    }
  }
}

export const api = new ApiClient(import.meta.env.VITE_RPC_URL);
