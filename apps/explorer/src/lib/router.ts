import { normalizeAddress } from '@wallet/lib/address';

export type Section = 'blocks' | 'transactions' | 'farmers';
export interface Paging {
  page?: number;
  at?: number;
}
export type Route =
  | ({ kind: Section } & Paging)
  | { kind: 'block'; hash: string }
  | { kind: 'block'; height: number }
  | ({ kind: 'transaction' | 'farmer' | 'address'; id: string } & Paging)
  | { kind: 'not-found' };

export const isHash = (value: string): boolean => /^[a-fA-F0-9]{64}$/.test(value);
export const isFarmer = (value: string): boolean => /^(02|03)[a-fA-F0-9]{64}$/.test(value);
export function blockHeight(value: string): number | null {
  if (!/^\d{1,10}$/.test(value)) return null;
  const height = Number(value);
  return height <= 0xffffffff ? height : null;
}

/** Existing wallet links use /#/explore/...; keep them valid indefinitely. */
export function parseRoute(hash: string): Route {
  let path: string;
  try {
    path = decodeURIComponent(hash.replace(/^#/, '')).replace(/\/$/, '');
  } catch {
    return { kind: 'not-found' };
  }
  if (!path || path === '/explore' || path === '/') return { kind: 'blocks' };
  const [pathname, query = ''] = path.split('?');
  const params = new URLSearchParams(query);
  const page = params.has('page') ? blockHeight(params.get('page')!) : 1;
  const at = params.has('at') ? blockHeight(params.get('at')!) : undefined;
  if (!page || page > 1_000_000 || at === null) return { kind: 'not-found' };
  const paging = { page, at };
  const parts = pathname!.split('/');
  if (parts[0] !== '' || parts[1] !== 'explore') return { kind: 'not-found' };
  const [, , kind, id, value] = parts;
  if (parts.length === 3 && ['blocks', 'transactions', 'farmers'].includes(kind ?? '')) {
    if (kind === 'blocks' && page > 1 && (at === undefined || at - (page - 1) * 20 < 0)) return { kind: 'not-found' };
    return { kind: kind as Section, ...paging };
  }
  if (parts.length === 5 && kind === 'block') {
    if (id === 'hash' && value && isHash(value)) return { kind: 'block', hash: value.toUpperCase() };
    const height = value ? blockHeight(value) : null;
    if (id === 'height' && height !== null) return { kind: 'block', height };
  }
  if (parts.length === 4 && id) {
    if (kind === 'transaction' && isHash(id)) return { kind, id: id.toUpperCase() };
    if (kind === 'farmer' && isFarmer(id)) return { kind, id: id.toUpperCase(), ...paging };
    const address = kind === 'address' ? normalizeAddress(id) : null;
    if (address) return { kind: 'address', id: address, ...paging };
  }
  return { kind: 'not-found' };
}

export function href(route: Exclude<Route, { kind: 'not-found' }>): string {
  let path: string = route.kind;
  if (route.kind === 'block') path += 'height' in route ? `/height/${route.height}` : `/hash/${route.hash}`;
  else if ('id' in route) path += `/${route.id}`;
  const params = new URLSearchParams();
  if ('page' in route && route.page && route.page > 1) params.set('page', String(route.page));
  if ('at' in route && route.at !== undefined && route.page && route.page > 1) params.set('at', String(route.at));
  return `#/explore/${path}${params.size ? `?${params}` : ''}`;
}

export const sectionOf = (route: Route): Section | null => {
  if (route.kind === 'block') return 'blocks';
  if (route.kind === 'transaction') return 'transactions';
  if (route.kind === 'farmer') return 'farmers';
  return ['blocks', 'transactions', 'farmers'].includes(route.kind) ? (route.kind as Section) : null;
};

export type SearchTarget = Exclude<Route, { kind: 'not-found' }> | { kind: 'hash'; id: string };
export function searchTarget(input: string): SearchTarget | null {
  const value = input.trim();
  const address = normalizeAddress(value);
  if (address) return { kind: 'address', id: address };
  if (isHash(value)) return { kind: 'hash', id: value.toUpperCase() };
  if (isFarmer(value)) return { kind: 'farmer', id: value.toUpperCase() };
  const height = blockHeight(value);
  return height === null ? null : { kind: 'block', height };
}
