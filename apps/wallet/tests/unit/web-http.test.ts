import { describe, expect, it } from 'vitest';
import { isBackendError, type ErrorCode } from '../../src/backend/types';
import { checkReadPath, MAX_RESPONSE_BYTES, rejectionCode, RpcHttp, type FetchLike } from '../../src/backend/web/http';
import { DEFAULT_RPC_URL, parseRpcUrl } from '../../src/backend/web/rpc-url';

async function code(p: Promise<unknown>): Promise<ErrorCode | 'ok'> {
  try {
    await p;
    return 'ok';
  } catch (e) {
    if (isBackendError(e)) return e.code;
    throw e;
  }
}

function reply(body: BodyInit | null, init: ResponseInit = {}): FetchLike {
  return async () => new Response(body, init);
}

describe('chain server URL', () => {
  it('defaults to the public server', () => {
    expect(parseRpcUrl(undefined)).toEqual({ base: DEFAULT_RPC_URL, origin: DEFAULT_RPC_URL });
    expect(parseRpcUrl('  ')).toEqual({ base: DEFAULT_RPC_URL, origin: DEFAULT_RPC_URL });
  });

  it('normalizes paths and keeps the origin for the CSP', () => {
    expect(parseRpcUrl('https://rpc.example.org/api/')).toEqual({ base: 'https://rpc.example.org/api', origin: 'https://rpc.example.org' });
    expect(parseRpcUrl('https://RPC.example.org:8443')).toEqual({ base: 'https://rpc.example.org:8443', origin: 'https://rpc.example.org:8443' });
    expect(parseRpcUrl('http://127.0.0.1:47100').origin).toBe('http://127.0.0.1:47100');
    expect(parseRpcUrl('http://localhost:11380').origin).toBe('http://localhost:11380');
  });

  it('rejects insecure or ambiguous URLs', () => {
    for (const bad of ['http://rpc.example.org', 'ftp://x', 'rpc.mmx.network', 'https://user:pw@rpc.example.org', 'https://rpc.example.org/?a=1', 'https://rpc.example.org/#x', 'javascript:alert(1)']) {
      expect(() => parseRpcUrl(bad), bad).toThrow();
    }
  });
});

describe('read paths', () => {
  it('allows the endpoints the wallet reads, with plain queries', () => {
    for (const ok of ['/node/info', '/chain/info', '/address?id=mmx1abc&limit=1000', '/address/history?id=mmx1abc&limit=25&until=99&since=3', '/transaction?id=ABCDEF0123']) {
      expect(checkReadPath(ok)).toBe(ok);
    }
  });

  it('rejects everything else', () => {
    for (const bad of ['', '/', '/wallet/keys', '/balance?id=mmx1abc', '/transaction/broadcast', '/transaction/validate', '/node/info/', '//node/info', '/address/../wallet', '/address?id=a%2F', '/address?id=a b', '/address?id=a#x', '/address?id=a.b', '/address?id=a/b', '/address?id=a?b', 'http://evil/node/info', `/address?id=${'a'.repeat(600)}`]) {
      expect(() => checkReadPath(bad), bad).toThrow(expect.objectContaining({ code: 'invalid_argument' }));
    }
  });
});

describe('RpcHttp', () => {
  it('has no way to send a signed transfer other than the broadcast', () => {
    expect(Object.getOwnPropertyNames(RpcHttp.prototype).filter((n) => n !== 'constructor' && n !== 'request').sort()).toEqual(['broadcast', 'get']);
  });

  it('sends credential-free, uncached, non-redirecting requests to the base URL', async () => {
    const calls: Array<{ url: string; init: RequestInit }> = [];
    const http = new RpcHttp(async (url, init) => {
      calls.push({ url, init });
      return new Response('{"height":5}', { status: 200 });
    }, 'https://rpc.example.org/api');
    expect((await http.get('/node/info')).value).toEqual({ height: 5 });
    await http.broadcast('{"x":1}');
    expect(calls[0]!.url).toBe('https://rpc.example.org/api/node/info');
    expect(calls[0]!.init).toMatchObject({ method: 'GET', credentials: 'omit', cache: 'no-store', redirect: 'error', referrerPolicy: 'no-referrer', mode: 'cors' });
    expect(calls[0]!.init.headers).toBeUndefined();
    expect(calls[1]!.url).toBe('https://rpc.example.org/api/transaction/broadcast');
    expect(calls[1]!.init).toMatchObject({ method: 'POST', body: '{"x":1}', headers: { 'Content-Type': 'text/plain;charset=UTF-8' } });
  });

  it('refuses paths outside the allowlist before any request', async () => {
    let called = false;
    const http = new RpcHttp(async () => {
      called = true;
      return new Response('{}');
    }, 'https://x');
    expect(await code(http.get('/wallet/keys'))).toBe('invalid_argument');
    expect(called).toBe(false);
  });

  it('maps empty bodies to null and bad JSON to rpc_response_invalid', async () => {
    expect((await new RpcHttp(reply(null, { status: 204 }), 'https://x').get('/node/info')).value).toBeNull();
    expect((await new RpcHttp(reply(''), 'https://x').get('/node/info')).value).toBeNull();
    expect(await code(new RpcHttp(reply('<html>'), 'https://x').get('/node/info'))).toBe('rpc_response_invalid');
  });

  it('maps transport failures', async () => {
    expect(await code(new RpcHttp(async () => Promise.reject(new TypeError('Failed to fetch')), 'https://x').get('/node/info'))).toBe('rpc_unreachable');
    expect(await code(new RpcHttp(reply('oops', { status: 500 }), 'https://x').get('/node/info'))).toBe('rpc_http_error');
    expect(await code(new RpcHttp(reply('bad', { status: 400 }), 'https://x').get('/node/info'))).toBe('rpc_http_error');
  });

  it('times out after the deadline', async () => {
    const hang: FetchLike = (_url, init) =>
      new Promise((_resolve, reject) => {
        init.signal?.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')));
      });
    expect(await code(new RpcHttp(hang, 'https://x', 30).get('/node/info'))).toBe('rpc_timeout');
  });

  it('times out while reading a stalled body', async () => {
    const stalled: FetchLike = async (_url, init) =>
      new Response(
        new ReadableStream({
          start(controller) {
            controller.enqueue(new TextEncoder().encode('{"a":'));
            init.signal?.addEventListener('abort', () => controller.error(new DOMException('aborted', 'AbortError')));
          },
        }),
      );
    expect(await code(new RpcHttp(stalled, 'https://x', 30).get('/node/info'))).toBe('rpc_timeout');
  });

  it('rejects responses over the size limit', async () => {
    const declared = reply('{}', { headers: { 'Content-Length': String(MAX_RESPONSE_BYTES + 1) } });
    expect(await code(new RpcHttp(declared, 'https://x').get('/node/info'))).toBe('rpc_response_invalid');
    const chunk = new Uint8Array(1024 * 1024).fill(32);
    let sent = 0;
    const streamed: FetchLike = async () =>
      new Response(
        new ReadableStream({
          pull(controller) {
            if (sent > MAX_RESPONSE_BYTES) controller.close();
            else {
              sent += chunk.length;
              controller.enqueue(chunk);
            }
          },
        }),
      );
    expect(await code(new RpcHttp(streamed, 'https://x').get('/node/info'))).toBe('rpc_response_invalid');
  });

  it('decodes multi-byte characters split across chunks', async () => {
    const bytes = new TextEncoder().encode('{"memo":"\u{4e16}\u{754c}"}');
    const split: FetchLike = async () =>
      new Response(
        new ReadableStream({
          start(controller) {
            controller.enqueue(bytes.slice(0, 11));
            controller.enqueue(bytes.slice(11));
            controller.close();
          },
        }),
      );
    expect((await new RpcHttp(split, 'https://x').get('/node/info')).value).toEqual({ memo: '\u{4e16}\u{754c}' });
  });

  it('maps a refused transfer (HTTP 4xx) by the node reason, server errors to rpc_http_error', async () => {
    const cases: Array<[string, ErrorCode]> = [
      ['insufficient funds for transaction fee: 5 > 0', 'insufficient_fee'],
      ['insufficient funds for mmx1abc', 'insufficient_funds'],
      ['tx expired', 'tx_expired'],
      ['invalid signature', 'tx_rejected'],
    ];
    for (const [text, expected] of cases) {
      expect(rejectionCode(text)).toBe(expected);
      expect(await code(new RpcHttp(reply(text, { status: 400 }), 'https://x').broadcast('{}'))).toBe(expected);
    }
    // Not accepted either: a proxy or rate limit in front of the node.
    for (const status of [403, 404, 413, 429]) {
      expect(await code(new RpcHttp(reply('no', { status }), 'https://x').broadcast('{}')), String(status)).toBe('tx_rejected');
    }
    expect(await code(new RpcHttp(reply('tx expired', { status: 422 }), 'https://x').broadcast('{}'))).toBe('tx_expired');
    // The outcome is open: the node may have accepted it.
    for (const status of [500, 502, 503, 504]) {
      expect(await code(new RpcHttp(reply('busy', { status }), 'https://x').broadcast('{}')), String(status)).toBe('rpc_http_error');
    }
    expect(await code(new RpcHttp(async () => Promise.reject(new TypeError('Failed to fetch')), 'https://x').broadcast('{}'))).toBe('rpc_unreachable');
    expect(await code(new RpcHttp(reply('', { status: 200 }), 'https://x').broadcast('{}'))).toBe('ok');
  });

  it('never exposes server text in errors', async () => {
    try {
      await new RpcHttp(reply('secret detail', { status: 500 }), 'https://x').get('/node/info');
      expect.unreachable();
    } catch (e) {
      expect(String((e as Error).message)).not.toContain('secret');
    }
  });
});
