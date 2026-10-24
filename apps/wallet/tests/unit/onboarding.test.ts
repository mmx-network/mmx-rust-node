import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { chosenPassphrase, detailsErrors, emptyDetails, verifyProblem } from '../../src/ui/screens/create/details';
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
    expect(detailsErrors(emptyDetails('  ')).name).toBe('Enter a name.');
    expect(detailsErrors(emptyDetails('x'.repeat(65))).name).toBe('Use 64 characters or fewer.');
    expect(detailsErrors(emptyDetails('Bad\u{202e}name')).name).toBeDefined();
    expect(detailsErrors(emptyDetails('Savings'))).toEqual({});
  });

  it('checks the passphrase only when one is used, typed twice for create and import', () => {
    const base = { ...emptyDetails('Savings'), passphrase: 'secret', repeat: 'other' };
    expect(detailsErrors(base)).toEqual({});
    expect(chosenPassphrase(base)).toBe('');
    const on = { ...base, usePassphrase: true };
    // A typo in an imported passphrase would open a different, empty wallet.
    expect(detailsErrors(on).repeat).toBe('The passphrases don’t match.');
    expect(detailsErrors({ ...on, repeat: 'secret' })).toEqual({});
    expect(detailsErrors({ ...on, passphrase: '' }).passphrase).toBe('Enter a passphrase.');
    expect(chosenPassphrase(on)).toBe('secret');
  });

  it('keeps the passphrase exactly as typed', () => {
    expect(chosenPassphrase({ ...emptyDetails('A'), usePassphrase: true, passphrase: ' a b ' })).toBe(' a b ');
  });
});

describe('web create: confirming the saved phrase', () => {
  const words = Array.from({ length: 24 }, (_, i) => ['zoo', 'abandon', 'ability'][i % 3]).join(' ');

  it('accepts the same words in any common format', () => {
    expect(verifyProblem(words, words)).toBeUndefined();
    expect(verifyProblem(`  ${words.toUpperCase().replace(/ /g, '\n')} `, words)).toBeUndefined();
  });

  it('explains empty, invalid and different words', () => {
    expect(verifyProblem('   ', words)).toBe('Enter your recovery phrase.');
    expect(verifyProblem('zoo zoo', words)).toBe('These recovery words are not valid. Check each word and the order.');
    const other = ['ability', ...words.split(' ').slice(1)].join(' ');
    expect(verifyProblem(other, words)).toBe('These words don’t match your new recovery phrase.');
  });
});

type State = typeof import('../../src/state');
type Mock = typeof import('../../src/backend/mock');

async function codeOf(p: Promise<unknown>): Promise<string> {
  try {
    await p;
    return 'ok';
  } catch (e) {
    return (e as { code?: string }).code ?? 'thrown';
  }
}

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

  it('stores nothing until the words are confirmed, so leaving keeps no wallet', async () => {
    const { S, backend } = await boot('desktop');
    const before = (await backend.listWallets()).length;
    S.navigate('create');
    const words = await S.newRecoveryPhrase();
    expect(words.split(' ')).toHaveLength(24);
    // Quitting here (or a crash) leaves nothing behind: a restart opens no unconfirmed wallet.
    expect(await backend.listWallets()).toHaveLength(before);
    expect(S.wallets.value).toHaveLength(before);
    expect(S.phase.value).toBe('ready');
    S.leaveOnboarding();
    expect(S.route.value.name).toBe('overview');
  });

  it('a restart after unconfirmed words starts on the welcome screen again', async () => {
    const { S, backend } = await boot('desktop', { empty: true });
    S.navigate('create');
    await S.newRecoveryPhrase();
    vi.resetModules();
    const again: State = await import('../../src/state');
    await again.init({ backend });
    await vi.advanceTimersByTimeAsync(200);
    expect(again.wallets.value).toHaveLength(0);
    expect(again.phase.value).toBe('onboarding');
  });

  it('finishes a desktop wallet on the overview', async () => {
    const { S } = await boot('desktop', { empty: true });
    S.navigate('create');
    const mnemonic = await S.newRecoveryPhrase();
    const wallet = await S.finishCreate({ name: 'First', mnemonic, passphrase: 'pw' });
    expect(wallet.withPassphrase).toBe(true);
    expect(S.phase.value).toBe('ready');
    expect(S.activeWallet.value?.id).toBe(wallet.id);
    expect(S.route.value.name).toBe('overview');
    expect(S.toasts.value.some((t) => t.message === 'Wallet created')).toBe(true);
    expect(await codeOf(S.finishCreate({ name: 'Again', mnemonic, passphrase: 'pw' }))).toBe('wallet_exists');
  });

  it('web: announces the wallet ID before storing, then opens the new wallet', async () => {
    const { S, backend } = await boot('web', { empty: true });
    S.navigate('create');
    const mnemonic = await S.newRecoveryPhrase();
    const id = await S.newWalletFingerprint(mnemonic, 'pw');
    // Changing the passphrase keeps the words and only changes the ID.
    expect(await S.newWalletFingerprint(mnemonic, '')).not.toBe(id);
    expect(await backend.listWallets()).toHaveLength(0);
    const wallet = await S.finishCreate({ name: 'Daily', mnemonic, passphrase: 'pw' });
    expect(wallet).toMatchObject({ id, fingerprint: id, verified: true, locked: false });
    expect(S.phase.value).toBe('ready');
    expect(S.route.value.name).toBe('overview');
  });

  it('auto-lock during setup drops the words and explains it on the welcome screen', async () => {
    const { S } = await boot('desktop', { empty: true });
    S.navigate('create');
    expect(S.autoLockDelayMinutes()).toBeNull();
    const release = S.beginSetup();
    // Words on screen count as an open wallet.
    expect(S.autoLockDelayMinutes()).toBe(15);
    await S.lock({ inactivity: true });
    expect(S.setupActive.value).toBe(false);
    expect(S.route.value.name).toBe('welcome');
    expect(S.setupNotice.value).toBe('Wallet setup was cancelled after inactivity.');
    release();
    expect(S.setupActive.value).toBe(false);
    S.clearSetupNotice();
    expect(S.setupNotice.value).toBeNull();
  });

  it('web: words typed into import count as an open wallet when none is unlocked', async () => {
    const { S } = await boot('web');
    expect(S.wallets.value.every((w) => w.locked)).toBe(true);
    S.navigate('import');
    expect(S.autoLockDelayMinutes()).toBeNull();
    // What the import screen does while words sit in its form.
    S.beginSetup();
    S.markActivity(Date.now());
    expect(S.autoLockDelayMinutes()).toBe(15);
    expect(S.idleExpired(Date.now() + 15 * 60_000)).toBe(true);
    await S.lock({ inactivity: true });
    expect(S.route.value.name).toBe('welcome');
    expect(S.setupNotice.value).toBe('Wallet setup was cancelled after inactivity.');
  });

  it('web: auto-lock while adding a wallet also ends the setup', async () => {
    const { S, M } = await boot('web');
    const main = S.wallets.value[0]!;
    await S.unlockWallet(main.id, { mnemonic: M.MOCK_WALLETS[0].mnemonic });
    await vi.advanceTimersByTimeAsync(200);
    S.navigate('create');
    S.beginSetup();
    await S.lock({ inactivity: true });
    expect(S.phase.value).toBe('unlock');
    expect(S.route.value.name).toBe('welcome');
    expect(S.setupNotice.value).toBe('Wallet setup was cancelled after inactivity.');
    expect(S.wallets.value.every((w) => w.locked)).toBe(true);
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
