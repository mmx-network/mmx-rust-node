/**
 * Hash router: #/overview #/send #/receive #/activity #/activity/<key>
 * #/contacts #/settings #/welcome #/create #/import #/unlock
 */
import { signal } from '@preact/signals';
import { isActivityKey } from '../chain/activity';
import type { Platform } from '../backend/types';

/** Routes rendered inside the shell. */
export type RouteId = 'overview' | 'send' | 'receive' | 'activity' | 'contacts' | 'settings';
/** Full-screen routes rendered without the shell. */
export type FlowRouteId = 'welcome' | 'create' | 'import' | 'unlock';
export type RouteName = RouteId | FlowRouteId;

export interface Route {
  name: RouteName;
  /** Only for 'activity': the selected item key. */
  param?: string;
}

export type AppPhase = 'loading' | 'onboarding' | 'unlock' | 'ready';

export const SHELL_ROUTES: readonly RouteId[] = ['overview', 'send', 'receive', 'activity', 'contacts', 'settings'];
export const FLOW_ROUTES: readonly FlowRouteId[] = ['welcome', 'create', 'import', 'unlock'];

export function isShellRoute(name: RouteName): name is RouteId {
  return (SHELL_ROUTES as readonly string[]).includes(name);
}

export function isFlowRoute(name: RouteName): name is FlowRouteId {
  return (FLOW_ROUTES as readonly string[]).includes(name);
}

/** Parses "#/activity/<key>" etc. Returns null for unknown or malformed hashes. */
export function parseHash(hash: string): Route | null {
  const path = hash.replace(/^#/, '');
  const m = /^\/([a-z]+)(?:\/([^/?#]+))?\/?$/.exec(path);
  if (!m) return null;
  const name = m[1] as RouteName;
  if (!isShellRoute(name) && !isFlowRoute(name)) return null;
  const param = m[2];
  if (param === undefined) return { name };
  if (name !== 'activity') return null;
  const key = param.toUpperCase().replace(/-(FARM|VDF|PROJECT)$/, (s) => s.toLowerCase());
  return isActivityKey(key) ? { name, param: key } : null;
}

export function formatHash(route: Route): string {
  return route.param ? `#/${route.name}/${route.param}` : `#/${route.name}`;
}

export function sameRoute(a: Route, b: Route): boolean {
  return a.name === b.name && a.param === b.param;
}

/** Default route for a phase. */
export function homeRoute(phase: AppPhase): Route {
  switch (phase) {
    case 'onboarding':
      return { name: 'welcome' };
    case 'unlock':
      return { name: 'unlock' };
    default:
      return { name: 'overview' };
  }
}

/**
 * Whether a route may be shown in a phase. Flow routes stay reachable while
 * the app becomes ready (e.g. a wallet created mid-flow) except #/unlock.
 */
export function routeAllowed(phase: AppPhase, route: Route, platform: Platform): boolean {
  switch (phase) {
    case 'loading':
      return true;
    case 'onboarding':
      return (
        route.name === 'welcome' || route.name === 'create' || route.name === 'import' || (route.name === 'unlock' && platform === 'web')
      );
    case 'unlock':
      return isFlowRoute(route.name);
    case 'ready':
      return route.name !== 'unlock';
  }
}

function currentHash(): string {
  return typeof window === 'undefined' ? '' : window.location.hash;
}

export const route = signal<Route>(parseHash(currentHash()) ?? { name: 'overview' });

export function navigate(name: RouteName, param?: string, options: { replace?: boolean } = {}): void {
  const next: Route = param ? { name, param } : { name };
  if (sameRoute(next, route.value) && currentHash() === formatHash(next)) return;
  route.value = next;
  if (typeof window === 'undefined') return;
  const hash = formatHash(next);
  if (options.replace) window.history.replaceState(window.history.state, '', hash);
  else if (window.location.hash !== hash) window.location.hash = hash;
}

/** Listens to hash changes. Returns a stop function. */
export function startRouter(): () => void {
  if (typeof window === 'undefined') return () => undefined;
  const onChange = () => {
    const parsed = parseHash(window.location.hash);
    if (parsed) {
      if (!sameRoute(parsed, route.value)) route.value = parsed;
    } else {
      navigate(route.value.name, route.value.param, { replace: true });
    }
  };
  window.addEventListener('hashchange', onChange);
  onChange();
  return () => window.removeEventListener('hashchange', onChange);
}
