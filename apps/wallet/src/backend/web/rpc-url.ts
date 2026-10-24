/**
 * Chain server address of the web wallet. Fixed at build time (`VITE_RPC_URL`)
 * and shared by the backend and the build's security headers, so the
 * Content-Security-Policy always allows exactly the server the app talks to.
 */

export const DEFAULT_RPC_URL = 'https://rpc.mmx.network';

export interface RpcEndpoint {
  /** Base URL requests are appended to (no trailing slash). */
  base: string;
  /** `scheme://host[:port]` for `connect-src`. */
  origin: string;
}

function isLoopback(hostname: string): boolean {
  return hostname === 'localhost' || hostname === '127.0.0.1' || hostname === '[::1]' || hostname.endsWith('.localhost');
}

/**
 * Validates and normalizes a chain server URL. HTTPS is required; plain HTTP
 * is accepted only for a server on this machine (tests, local node). Throws
 * an Error describing the problem.
 */
export function parseRpcUrl(raw: string | undefined): RpcEndpoint {
  const text = (raw ?? '').trim() || DEFAULT_RPC_URL;
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    throw new Error(`VITE_RPC_URL is not a valid URL: ${JSON.stringify(text)}`);
  }
  const secure = url.protocol === 'https:' || (url.protocol === 'http:' && isLoopback(url.hostname));
  if (!secure) throw new Error('VITE_RPC_URL must use https:// (http:// only for localhost)');
  if (url.username || url.password) throw new Error('VITE_RPC_URL must not contain credentials');
  if (url.search || url.hash || text.includes('?') || text.includes('#')) {
    throw new Error('VITE_RPC_URL must not contain a query or fragment');
  }
  const path = url.pathname.replace(/\/+$/u, '');
  return { base: url.origin + path, origin: url.origin };
}
