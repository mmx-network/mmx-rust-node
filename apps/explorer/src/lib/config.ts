export const DEFAULT_RPC = 'https://rpc.mmx.network';

/** Match the wallet: a public HTTPS server, with HTTP allowed for local tests. */
export function rpcUrl(input: string | undefined): string {
  const text = input?.trim() || DEFAULT_RPC;
  const url = new URL(text);
  const local = ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname);
  if (
    (url.protocol !== 'https:' && !(url.protocol === 'http:' && local)) ||
    url.username ||
    url.password ||
    url.search ||
    url.hash ||
    text.includes('?') ||
    text.includes('#')
  ) {
    throw new Error('VITE_RPC_URL must be HTTPS (or loopback HTTP), without credentials, query or fragment.');
  }
  return url.origin + url.pathname.replace(/\/+$/, '');
}

export const rpcOrigin = (input: string | undefined): string => new URL(rpcUrl(input)).origin;
