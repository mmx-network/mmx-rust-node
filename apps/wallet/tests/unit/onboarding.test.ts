import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { chosenPassphrase, detailsErrors, emptyDetails } from '../../src/ui/screens/create/details';
import { fillWords, firstToFix, problemIndices, wordsMessage } from '../../src/ui/screens/import/words';

const START = 1_792_835_101_709;
const EMPTY = Array.from({ length: 24 }, () => '');
const NONE = EMPTY.map(() => false);

describe('recovery word entry', () => {
  it('fills a full pasted phrase from the first box, wherever it was pasted', () => {
    const list = Array.from({ length: 24 }, (_, i) => `w${i}`);
    const { words, start, end } = fillWords(EMPTY, 7, list);
    expect(start).toBe(0);
    expect(end).toBe(24);
    expect(words).toEqual(list);
  });

  it('fills a partial paste from the box it was pasted into and keeps the others', () => {
    const current = EMPTY.slice();
    current[0] = 'abandon';
    const { words, start, end } = fillWords(current, 22, ['zoo', 'wrong', 'extra']);
    expect([start, end]).toEqual([22, 24]);
    expect(words[0]).toBe('abandon');
    expect(words.slice(22)).toEqual(['zoo', 'wrong']);
    expect(words).toHaveLength(24);
  });

  it('ignores words beyond the 24th', () => {
    const list = Array.from({ length: 30 }, (_, i) => `w${i}`);
    expect(fillWords(EMPTY, 0, list).words).toEqual(list.slice(0, 24));
  });

  it('flags words that cannot become list words at once, prefixes only once left', () => {
    const words = EMPTY.slice();
    words[0] = 'xyz'; // no list word starts with it
    words[1] = 'aba'; // prefix of "abandon"
    words[2] = 'zoo';
    expect(problemIndices(words, NONE, 1, false)).toEqual([0]);
    const touched = NONE.slice();
    touched[1] = true;
    expect(problemIndices(words, touched, 1, false)).toEqual([0]);
    expect(problemIndices(words, touched, -1, false)).toEqual([0, 1]);
    expect(problemIndices(words, NONE, -1, true)).toEqual([0, 1]);
  });

  it('describes what to fix in one line', () => {
    const words = EMPTY.slice();
    expect(wordsMessage(words, [], false)).toBeUndefined();
    expect(wordsMessage(words, [], true)).toBe('Enter all 24 words.');
    expect(wordsMessage(words, [4], true)).toBe('Word 5 isn’t on the word list.');
    expect(wordsMessage(words, [2, 6, 8], false)).toBe('Words 3, 7, and 9 aren’t on the word list.');
  });

  it('goes to the first word to fix, then to the first empty box', () => {
    const words = EMPTY.map(() => 'zoo');
    expect(firstToFix(words)).toBe(-1);
    words[10] = '';
    expect(firstToFix(words)).toBe(10);
    words[15] = 'qqq';
    expect(firstToFix(words)).toBe(15);
  });
});

describe('wallet details', () => {
  it('requires a usable name', () => {
    expect(detailsErrors(emptyDetails('  '), true).name).toBe('Enter a name.');
    expect(detailsErrors(emptyDetails('x'.repeat(65)), true).name).toBe('Use 64 characters or fewer.');
    expect(detailsErrors(emptyDetails('Bad\u{202e}name'), true).name).toBeDefined();
    expect(detailsErrors(emptyDetails('Savings'), true)).toEqual({});
  });

  it('checks the passphrase only when one is used', () => {
    const base = { ...emptyDetails('Savings'), passphrase: 'secret', repeat: 'other' };
    expect(detailsErrors(base, true)).toEqual({});
    expect(chosenPassphrase(base)).toBe('');
    const on = { ...base, usePassphrase: true };
    expect(detailsErrors(on, true).repeat).toBe('The passphrases don’t match.');
    expect(detailsErrors(on, false)).toEqual({});
    expect(detailsErrors({ ...on, passphrase: '' }, false).passphrase).toBe('Enter a passphrase.');
    expect(chosenPassphrase(on)).toBe('secret');
  });

  it('keeps the passphrase exactly as typed', () => {
    expect(chosenPassphrase({ ...emptyDetails('A'), usePassphrase: true, passphrase: ' a b ' })).toBe(' a b ');
  });
});

type State = typeof import('../../src/state');
type Mock = typeof import('../../src/backend/mock');

async function boot(platform: 'desktop' | 'web', options: { empty?: boolean } = {}) {
  vi.resetModules();
  const S: State = await import('../../src/state');
  const M: Mock = await import('../../src/backend/mock');
  const backend = M.createMockBackend({ platform, ...options });
  await S.init({ backend });
  await vi.advanceTimersByTimeAsync(200);
  return { S, M, backend };
}

describe('onboarding actions', () => {
  beforeEach(() => {
    vi.useFakeTimers({ now: START, toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval', 'Date'] });
  });
  afterEach(() => {
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  it('removes an abandoned new wallet and leaves the flow back into the app', async () => {
    const { S } = await boot('desktop');
    const before = S.wallets.value.length;
    S.navigate('create');
    const { wallet } = await S.createWallet({ name: 'Spare' });
    expect(S.wallets.value).toHaveLength(before + 1);
    await S.discardNewWallet(wallet.id);
    expect(S.wallets.value).toHaveLength(before);
    expect(S.wallets.value.some((w) => w.id === wallet.id)).toBe(false);
    // Already gone: nothing happens.
    await S.discardNewWallet(wallet.id);
    expect(S.phase.value).toBe('ready');
    S.leaveOnboarding();
    expect(S.route.value.name).toBe('overview');
  });

  it('returns to the start when the only wallet was abandoned', async () => {
    const { S } = await boot('desktop', { empty: true });
    S.navigate('create');
    const { wallet } = await S.createWallet({ name: 'First' });
    expect(S.phase.value).toBe('unlock');
    expect(S.route.value.name).toBe('create');
    await S.discardNewWallet(wallet.id);
    expect(S.phase.value).toBe('onboarding');
    S.leaveOnboarding();
    expect(S.route.value.name).toBe('welcome');
  });

  it('finishes a desktop wallet on the overview', async () => {
    const { S } = await boot('desktop', { empty: true });
    S.navigate('create');
    const { wallet } = await S.createWallet({ name: 'First', passphrase: 'pw' });
    await S.finishCreate(wallet.id);
    expect(S.phase.value).toBe('ready');
    expect(S.activeWallet.value?.id).toBe(wallet.id);
    expect(S.route.value.name).toBe('overview');
    expect(S.toasts.value.some((t) => t.message === 'Wallet created')).toBe(true);
  });

  it('web import can stay on its page for the password manager step', async () => {
    const { S, M } = await boot('web', { empty: true });
    S.navigate('import');
    const words = M.MOCK_WALLETS[2].mnemonic;
    const done = S.importWallet({ name: 'Trading', mnemonic: words }, { navigateTo: null });
    await vi.advanceTimersByTimeAsync(30_000);
    const info = await done;
    expect(info.name).toBe('Trading');
    expect(S.phase.value).toBe('ready');
    expect(S.route.value.name).toBe('import');
  });

  it('does not use the password manager outside the web', async () => {
    const { S } = await boot('desktop');
    expect(S.passwordManagerApi()).toBe(false);
    expect(await S.saveWalletCredential('Main (1)', 'words')).toBe(false);
    expect(await S.requestWalletCredential()).toBeNull();
  });
});
