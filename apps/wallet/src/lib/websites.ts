import type { ErrorCode, Settings } from '../backend/types';

export const MAX_BLOCKED_WEBSITES = 200;

/** Permissions apply to an exact origin, including its port. */
export function websiteOrigin(value: unknown): string | null {
  if (typeof value !== 'string' || value.length > 512) return null;
  try {
    const url = new URL(value);
    return (url.protocol === 'https:' || (url.protocol === 'http:' && ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname))) &&
      url.origin === value
      ? value
      : null;
  } catch {
    return null;
  }
}

export function websiteAccess(settings: Settings, site: string): ErrorCode | null {
  if (!websiteOrigin(site)) return 'invalid_argument';
  if (settings.blockedWebsites?.includes(site)) return 'website_blocked';
  return settings.allowWebsiteConnections === true ? null : 'website_connections_disabled';
}
