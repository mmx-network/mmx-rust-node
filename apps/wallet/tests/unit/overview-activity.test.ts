import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ActivityItem } from '../../src/chain/activity';
import { NATIVE_CONTRACT } from '../../src/lib/address';
import { toInputString } from '../../src/lib/amount';
import { setLocale } from '../../src/lib/locale';
import {
  formatTick,
  formatTimeTick,
  indexAt,
  niceStep,
  stepPath,
  timeTicks,
  toDisplayNumber,
  valueScale,
} from '../../src/ui/screens/overview/chart';
import {
  activityIcon,
  activityTitle,
  counterpartyLine,
  extraAssetCount,
  isUnverified,
  rowAmount,
} from '../../src/ui/screens/activity/present';

const TOKEN = 'mmx1an9xrxafngpujg0qt98pu750s3n7duz00m8dwmuv228p8nyavh0qvv24ne';
const OTHER = 'mmx1n35pu3qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq3fmvf6';

function item(partial: Partial<ActivityItem>): ActivityItem {
  return {
    key: 'A'.repeat(64),
    txid: 'A'.repeat(64),
    kind: 'received',
    status: 'confirmed',
    height: 100,
    timeMs: 0,
    amounts: [],
    fee: 0n,
    ownAddresses: [],
    ...partial,
  };
}

const mmx = (amount: bigint) => ({ contract: NATIVE_CONTRACT, symbol: 'MMX', decimals: 6, native: true, verified: true, amount });

describe('balance chart geometry', () => {
  beforeEach(() => setLocale('en-US'));
  afterEach(() => setLocale(null));

  it('converts atomic units without losing the integer part', () => {
    expect(toDisplayNumber(1_500_000n, 6)).toBe(1.5);
    expect(toDisplayNumber(0n, 6)).toBe(0);
    expect(toDisplayNumber(123_456_789_000_000n, 6)).toBe(123_456_789);
    expect(toDisplayNumber(42n, 0)).toBe(42);
  });

  it('picks nice steps', () => {
    expect(niceStep(1000, 4)).toBe(250);
    expect(niceStep(9, 3)).toBe(5);
    expect(niceStep(0, 3)).toBe(1);
    expect(niceStep(Number.NaN, 3)).toBe(1);
  });

  it('starts the value axis at zero when values are near it', () => {
    const s = valueScale(100, 3400);
    expect(s.lo).toBe(0);
    expect(s.hi).toBeGreaterThanOrEqual(3400);
    expect(s.ticks[0]).toBe(0);
    expect(s.ticks[s.ticks.length - 1]).toBe(s.hi);
  });

  it('fits the axis to large balances with small changes', () => {
    const s = valueScale(10_000, 10_050);
    expect(s.lo).toBeGreaterThan(9_000);
    expect(s.lo).toBeLessThanOrEqual(10_000);
    expect(s.hi).toBeGreaterThanOrEqual(10_050);
    expect(s.hi - s.lo).toBeLessThan(200);
    expect(s.ticks.length).toBeGreaterThanOrEqual(2);
  });

  it('handles flat and empty series', () => {
    expect(valueScale(0, 0)).toEqual({ lo: 0, hi: 1, step: 1, ticks: [0] });
    const flat = valueScale(500, 500);
    expect(flat.lo).toBe(0);
    expect(500 / flat.hi).toBeGreaterThan(0.5);
    expect(500 / flat.hi).toBeLessThan(0.8);
  });

  it('formats ticks so neighbours stay distinct', () => {
    const s = valueScale(10_000, 10_050);
    const labels = s.ticks.map((v) => formatTick(v, s.step));
    expect(new Set(labels).size).toBe(labels.length);
    expect(formatTick(2500, 500)).toBe('2,500');
    expect(formatTick(0, 500)).toBe('0');
    expect(formatTick(0.25, 0.05)).toBe('0.25');
    expect(formatTick(1_500_000, 500_000)).toBe('1.5M');
    expect(formatTick(20_000, 10_000)).toBe('20K');
  });

  it('puts time ticks on local calendar boundaries within the range', () => {
    const start = new Date(2026, 9, 17, 15, 20).getTime();
    const end = new Date(2026, 9, 24, 15, 20).getTime();
    const week = timeTicks(start, end, 8);
    expect(week.unit).toBe('day');
    expect(week.ticks).toHaveLength(7);
    for (const t of week.ticks) {
      const d = new Date(t);
      expect(d.getHours()).toBe(0);
      expect(t).toBeGreaterThanOrEqual(start);
      expect(t).toBeLessThanOrEqual(end);
    }
    expect(formatTimeTick(week.ticks[0]!, 'day')).toBe('Oct 18');

    const year = timeTicks(new Date(2025, 9, 24).getTime(), new Date(2026, 9, 24).getTime(), 8);
    expect(year.unit).toBe('month');
    expect(year.ticks.length).toBeLessThanOrEqual(8);
    expect(year.ticks.every((t) => new Date(t).getDate() === 1)).toBe(true);
    expect(year.ticks.map((t) => formatTimeTick(t, 'month'))).toContain('2026');

    const day = timeTicks(new Date(2026, 9, 24, 1, 30).getTime(), new Date(2026, 9, 25, 1, 30).getTime(), 6);
    expect(day.unit).toBe('hour');
    expect(day.ticks.length).toBeLessThanOrEqual(6);
    expect(day.ticks.every((t) => new Date(t).getMinutes() === 0)).toBe(true);

    expect(timeTicks(5, 5, 4).ticks).toEqual([]);
  });

  it('looks up the value at a time with step semantics', () => {
    const points = [{ t: 10 }, { t: 20 }, { t: 30 }];
    expect(indexAt(points, 5)).toBe(0);
    expect(indexAt(points, 10)).toBe(0);
    expect(indexAt(points, 19)).toBe(0);
    expect(indexAt(points, 20)).toBe(1);
    expect(indexAt(points, 99)).toBe(2);
    expect(indexAt([], 1)).toBe(-1);
  });

  it('draws steps and collapses points in the same pixel column', () => {
    expect(stepPath([0, 10, 20], [50, 40, 60], 30)).toBe('M0 50H10V40H20V60H30');
    // Three changes inside one column keep their full vertical extent.
    expect(stepPath([0, 10, 10.1, 10.2], [50, 20, 80, 60], 20)).toBe('M0 50H10V20V80V60H20');
    // A change back to the same value inside a column still shows the spike.
    expect(stepPath([0, 10, 10.1], [50, 20, 50], 20)).toBe('M0 50H10V20V50H20');
    expect(stepPath([0, 10], [50, 50], 10)).toBe('M0 50H10');
    expect(stepPath([], [], 10)).toBe('');
  });
});

describe('activity presentation', () => {
  it('names every kind', () => {
    expect(activityTitle(item({ kind: 'received' }))).toBe('Received');
    expect(activityTitle(item({ kind: 'sent' }))).toBe('Sent');
    expect(activityTitle(item({ kind: 'moved' }))).toBe('Moved between your addresses');
    expect(activityTitle(item({ kind: 'reward', rewardType: 'farming' }))).toBe('Farming reward');
    expect(activityTitle(item({ kind: 'reward', rewardType: 'timelord' }))).toBe('Timelord reward');
    expect(activityTitle(item({ kind: 'reward', rewardType: 'project' }))).toBe('Project reward');
    expect(activityTitle(item({ kind: 'fee' }))).toBe('Network fee');
  });

  it('signs and colours amounts by direction and status', () => {
    const received = item({ kind: 'received', primary: mmx(5_000_000n), amounts: [mmx(5_000_000n)] });
    expect(rowAmount(received, 'confirmed', 6)).toMatchObject({ value: 5_000_000n, sign: 'always', tone: 'positive' });
    const sent = item({ kind: 'sent', primary: mmx(-2_000_000n), amounts: [mmx(-2_000_000n)] });
    expect(rowAmount(sent, 'confirmed', 6)).toMatchObject({ value: -2_000_000n, sign: 'auto', tone: 'neutral' });
    expect(rowAmount(sent, 'failed', 6)).toMatchObject({ tone: 'muted' });
    const moved = item({ kind: 'moved', primary: mmx(7n), amounts: [mmx(7n)] });
    expect(rowAmount(moved, 'confirmed', 6)).toMatchObject({ value: 7n, sign: 'never', tone: 'muted' });
    const fee = item({ kind: 'fee', fee: 55_000n });
    expect(rowAmount(fee, 'confirmed', 6)).toMatchObject({ value: -55_000n, symbol: 'MMX', native: true });
    expect(rowAmount(item({ kind: 'fee' }), 'confirmed', 6)).toBeNull();
  });

  it('never presents unverified tokens as real income', () => {
    const fake = { contract: TOKEN, symbol: 'MMX', decimals: 6, native: false, verified: false, amount: 1_000_000n };
    const it1 = item({ kind: 'received', primary: fake, amounts: [fake] });
    expect(isUnverified(it1)).toBe(true);
    expect(rowAmount(it1, 'confirmed', 6)).toMatchObject({ tone: 'muted', native: false, verified: false });
    expect(activityIcon(it1, 'confirmed').tone).toBe('neutral');
    expect(isUnverified(item({ kind: 'received', primary: mmx(1n) }))).toBe(false);
  });

  it('uses status icons for unconfirmed items', () => {
    expect(activityIcon(item({ kind: 'sent' }), 'pending')).toEqual({ icon: 'clock', tone: 'warning' });
    expect(activityIcon(item({ kind: 'sent' }), 'failed')).toEqual({ icon: 'x-circle', tone: 'negative' });
    expect(activityIcon(item({ kind: 'sent' }), 'expired')).toEqual({ icon: 'x-circle', tone: 'negative' });
    expect(activityIcon(item({ kind: 'reward' }), 'confirmed')).toEqual({ icon: 'reward', tone: 'accent' });
  });

  it('describes the counterparty', () => {
    const cp = { address: OTHER, source: 'address' as const };
    expect(counterpartyLine(item({ kind: 'received' }), { ...cp, name: 'Alice', source: 'contact' })).toBe('From Alice');
    expect(counterpartyLine(item({ kind: 'sent' }), cp)).toMatch(/^To mmx1n35pu3/);
    expect(counterpartyLine(item({ kind: 'reward' }), cp)).toBeNull();
    expect(counterpartyLine(item({ kind: 'sent' }), null)).toBeNull();
    expect(counterpartyLine(item({ kind: 'sent' }), undefined)).toBeNull();
  });

  it('counts extra assets of multi-asset transfers', () => {
    const t = { contract: TOKEN, symbol: 'USDM', decimals: 6, native: false, verified: true, amount: 3n };
    expect(extraAssetCount(item({ primary: mmx(1n), amounts: [mmx(1n), t] }))).toBe(1);
    expect(extraAssetCount(item({ amounts: [] }))).toBe(0);
  });
});

describe('overview and activity store helpers', () => {
  const START = 1_792_835_101_709;

  beforeEach(() => {
    vi.useFakeTimers({ now: START, toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval', 'Date'] });
  });
  afterEach(() => {
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  async function boot() {
    vi.resetModules();
    const S = await import('../../src/state');
    const M = await import('../../src/backend/mock');
    const A = await import('../../src/state/actions/activity');
    const O = await import('../../src/state/actions/overview');
    const session = await import('../../src/state/session');
    const backend = M.createMockBackend({ platform: 'desktop' });
    await S.init({ backend });
    await vi.advanceTimersByTimeAsync(500);
    return { S, A, O, session, backend };
  }

  it('lists filter assets with MMX first and unverified tokens last', async () => {
    const { S, A } = await boot();
    const trading = S.wallets.value.find((w) => w.name === 'Trading')!;
    await S.selectWallet(trading.id);
    await vi.advanceTimersByTimeAsync(1000);
    const options = A.activityAssetOptions.value;
    expect(options[0]).toMatchObject({ contract: NATIVE_CONTRACT, native: true });
    expect(options.map((o) => o.symbol)).toEqual(['MMX', 'USDM', 'MMX']);
    expect(options[2]).toMatchObject({ native: false, verified: false });
  });

  it('prefills "Send again" only for sent transfers of verified assets', async () => {
    const { S, A } = await boot();
    const sent = S.activity.value.find((i) => i.kind === 'sent' && i.primary?.native)!;
    expect(sent).toBeDefined();
    expect(A.sendAgainDraft(sent)).toBeNull(); // counterparty not loaded yet
    await S.loadTx(sent.txid);
    const draft = A.sendAgainDraft(sent, 'Hello')!;
    expect(draft.contract).toBe(NATIVE_CONTRACT);
    expect(draft.to).toMatch(/^mmx1/);
    expect(S.ownAddressSet.value.has(draft.to!)).toBe(false);
    expect(draft.amount).toBe(toInputString(-sent.primary!.amount, 6));
    expect(draft.memo).toBe('Hello');
    const reward = S.activity.value.find((i) => i.kind === 'reward')!;
    expect(A.sendAgainDraft(reward)).toBeNull();
    const received = S.activity.value.find((i) => i.kind === 'received')!;
    expect(A.sendAgainDraft(received)).toBeNull();

    expect(await A.sendAgain(sent)).toBe(true);
    expect(S.route.value.name).toBe('send');
    expect(S.sendDraft.value.to).toBe(draft.to);
    expect(S.sendDraft.value.contract).toBe(NATIVE_CONTRACT);
  });

  it('reports pending outgoing MMX and the charted change', async () => {
    const { S, A, O, session } = await boot();
    expect(O.pendingNativeOutgoing.value).toBe(0n);
    session.trackPending({
      txid: 'B'.repeat(64),
      walletId: S.sessionWalletId.value!,
      to: OTHER,
      contract: NATIVE_CONTRACT,
      symbol: 'MMX',
      decimals: 6,
      amount: 12_500_000n,
      fee: 55_000n,
      expires: 1e9,
      createdAt: START,
      status: 'pending',
    });
    expect(O.pendingNativeOutgoing.value).toBe(12_555_000n);
    const local = S.activity.value.find((i) => i.local)!;
    expect(local).toMatchObject({ kind: 'sent', status: 'pending' });
    expect(A.sendAgainDraft(local)).toBeNull();
    const change = O.balanceChange.value!;
    const series = S.balanceSeries.value!;
    expect(change.delta).toBe(series.points[series.points.length - 1]!.v - series.points[0]!.v);
    expect(O.verifiedTokens.value.every((a) => !a.native)).toBe(true);
    expect(O.hasTokens.value).toBe(true);
  });

  it('copies values without a toast', async () => {
    const { S, A, backend } = await boot();
    const before = S.toasts.value.length;
    await A.copyValue('mmx1test');
    expect(backend.clipboard).toBe('mmx1test');
    expect(S.toasts.value.length).toBe(before);
  });
});
