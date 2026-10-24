import { describe, expect, it } from 'vitest';
import { formatHash, homeRoute, parseHash, routeAllowed, type AppPhase, type Route } from '../../src/state/router';

const TXID = 'AB'.repeat(32);

describe('router', () => {
  it('parses known routes', () => {
    expect(parseHash('#/overview')).toEqual({ name: 'overview' });
    expect(parseHash('#/send/')).toEqual({ name: 'send' });
    expect(parseHash('#/unlock')).toEqual({ name: 'unlock' });
    expect(parseHash(`#/activity/${TXID}`)).toEqual({ name: 'activity', param: TXID });
    expect(parseHash(`#/activity/${TXID.toLowerCase()}`)).toEqual({ name: 'activity', param: TXID });
    expect(parseHash(`#/activity/${TXID}-vdf`)).toEqual({ name: 'activity', param: `${TXID}-vdf` });
    expect(parseHash(`#/activity/${TXID.toLowerCase()}-FARM`)).toEqual({ name: 'activity', param: `${TXID}-farm` });
  });

  it('rejects unknown or malformed routes', () => {
    for (const h of ['', '#', '#/', '#/nope', '#/activity/xyz', `#/send/${TXID}`, '#/overview/extra', `#/activity/${TXID}-bad`, '#/activity/<script>', '#//overview']) {
      expect(parseHash(h)).toBeNull();
    }
  });

  it('formats routes', () => {
    expect(formatHash({ name: 'contacts' })).toBe('#/contacts');
    expect(formatHash({ name: 'activity', param: TXID })).toBe(`#/activity/${TXID}`);
    expect(parseHash(formatHash({ name: 'activity', param: `${TXID}-project` }))).toEqual({ name: 'activity', param: `${TXID}-project` });
  });

  it('guards routes per phase', () => {
    const allowed = (phase: AppPhase, name: Route['name'], platform: 'desktop' | 'web' = 'desktop') => routeAllowed(phase, { name }, platform);
    expect(allowed('loading', 'send')).toBe(true);
    expect(allowed('onboarding', 'welcome')).toBe(true);
    expect(allowed('onboarding', 'create')).toBe(true);
    expect(allowed('onboarding', 'import')).toBe(true);
    expect(allowed('onboarding', 'unlock')).toBe(false);
    expect(allowed('onboarding', 'unlock', 'web')).toBe(true);
    expect(allowed('onboarding', 'overview')).toBe(false);
    expect(allowed('unlock', 'unlock')).toBe(true);
    expect(allowed('unlock', 'create')).toBe(true);
    expect(allowed('unlock', 'settings')).toBe(false);
    expect(allowed('ready', 'overview')).toBe(true);
    expect(allowed('ready', 'create')).toBe(true);
    expect(allowed('ready', 'unlock')).toBe(false);
    expect(homeRoute('onboarding')).toEqual({ name: 'welcome' });
    expect(homeRoute('unlock')).toEqual({ name: 'unlock' });
    expect(homeRoute('ready')).toEqual({ name: 'overview' });
  });
});
