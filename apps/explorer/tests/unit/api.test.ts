import { afterEach, describe, expect, it, vi } from 'vitest';
import { ApiClient, MAX_REPLY_BYTES, endpoint } from '../../src/lib/api';

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});
const json = (value: unknown): Response => new Response(JSON.stringify(value));
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

describe('bounded public API transport', () => {
  it('shares in-flight GETs but keeps each consumer cancellation independent', async () => {
    const response = deferred<Response>();
    let sharedSignal: AbortSignal | undefined;
    const fetcher = vi.fn<typeof fetch>((_, init) => {
      sharedSignal = init?.signal as AbortSignal;
      return response.promise;
    });
    const client = new ApiClient(undefined, fetcher);
    const a = new AbortController();
    const first = client.get('/node/info', a.signal);
    const second = client.get('/node/info');
    const cancelled = expect(first).rejects.toMatchObject({ name: 'AbortError' });
    a.abort();
    await cancelled;
    expect(sharedSignal?.aborted).toBe(false);
    response.resolve(json({ height: 10 }));
    expect(await second).toEqual({ height: 10 });
    expect(fetcher).toHaveBeenCalledTimes(1);
    fetcher.mockImplementation(async () => json({ height: 11 }));
    expect(await client.get('/node/info')).toEqual({ height: 11 });
    expect(fetcher).toHaveBeenCalledTimes(2);
  });
  it('aborts abandoned requests and starts a fresh subscription immediately', async () => {
    const requests: AbortSignal[] = [];
    const fetcher = vi.fn<typeof fetch>((_, init) => {
      requests.push(init!.signal as AbortSignal);
      return requests.length === 1 ? new Promise<Response>(() => {}) : Promise.resolve(json([]));
    });
    const client = new ApiClient(undefined, fetcher);
    const a = new AbortController();
    const request = client.get('/headers', a.signal);
    const rejected = expect(request).rejects.toMatchObject({ name: 'AbortError' });
    a.abort();
    await rejected;
    expect(requests[0]!.aborted).toBe(true);
    expect(await client.get('/headers')).toEqual([]);
    expect(fetcher).toHaveBeenCalledTimes(2);
  });
  it('treats absent resources as null but keeps sync/server failures visible', async () => {
    const fetcher = vi.fn<typeof fetch>();
    const client = new ApiClient(undefined, fetcher);
    fetcher.mockResolvedValueOnce(new Response(null, { status: 204 }));
    expect(await client.get('/header')).toBeNull();
    fetcher.mockResolvedValueOnce(new Response(null, { status: 404 }));
    expect(await client.get('/transaction')).toBeNull();
    fetcher.mockResolvedValueOnce(new Response('internal details', { status: 503 }));
    await expect(client.get('/header')).rejects.toMatchObject({
      status: 503,
      message: 'The chain server is syncing. Please try again shortly.',
    });
  });
  it('rejects oversized bodies before reading or beyond the streaming bound', async () => {
    let cancelled = false;
    const declared = new Response(
      new ReadableStream({
        cancel() {
          cancelled = true;
        },
      }),
      { headers: { 'Content-Length': String(MAX_REPLY_BYTES + 1) } },
    );
    const client = new ApiClient(undefined, async () => declared);
    await expect(client.get('/block')).rejects.toThrow('too large');
    expect(cancelled).toBe(true);
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new Uint8Array(MAX_REPLY_BYTES));
        controller.enqueue(new Uint8Array(1));
        controller.close();
      },
    });
    await expect(new ApiClient(undefined, async () => new Response(stream)).get('/block')).rejects.toThrow('too large');
  });
  it('bounds the entire request, including a stalled response body', async () => {
    vi.useFakeTimers();
    const fetcher: typeof fetch = async (_, init) =>
      new Response(
        new ReadableStream({
          start(controller) {
            init?.signal?.addEventListener('abort', () => controller.error(new Error('aborted')));
          },
        }),
      );
    const promise = new ApiClient(undefined, fetcher).get('/block');
    const rejected = expect(promise).rejects.toThrow('timed out');
    await vi.advanceTimersByTimeAsync(20_000);
    await rejected;
  });
  it('rejects invalid JSON and unexpected API paths', async () => {
    const fetcher = vi.fn<typeof fetch>(async () => new Response('<html>proxy error</html>'));
    const client = new ApiClient(undefined, fetcher);
    await expect(client.get('/headers')).rejects.toThrow('invalid JSON');
    await expect(client.get('/wallet/seed')).rejects.toThrow('Unknown explorer endpoint');
    expect(() => endpoint('/wallet/seed')).toThrow();
    expect(fetcher).toHaveBeenCalledTimes(1);
  });
  it('calls the browser fetch as a function and omits cookies and referrers', async () => {
    let receiver: unknown = 'uncalled';
    const fetcher = vi.fn(function (this: unknown, _input: unknown, _init: unknown) {
      receiver = this;
      return Promise.resolve(json({ height: 1 }));
    });
    vi.stubGlobal('fetch', fetcher);
    await new ApiClient().get('/node/info');
    expect(receiver).toBeUndefined();
    expect(fetcher.mock.calls[0]?.[1]).toMatchObject({
      credentials: 'omit',
      cache: 'no-store',
      redirect: 'error',
      referrerPolicy: 'no-referrer',
    });
  });
});
