import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { poll, type ResourceState } from '../../src/lib/poll';

class Visibility extends EventTarget {
  hidden = false;
  set(hidden: boolean) {
    this.hidden = hidden;
    this.dispatchEvent(new Event('visibilitychange'));
  }
}
const flush = async () => {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
};
beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe('page-scoped refresh scheduling', () => {
  it('does not overlap slow reads and waits a full period after completion', async () => {
    let complete!: (value: number) => void;
    const load = vi.fn(
      () =>
        new Promise<number>((resolve) => {
          complete = resolve;
        }),
    );
    const resource = poll(load, () => {}, 1000, new Visibility());
    await vi.advanceTimersByTimeAsync(5000);
    expect(load).toHaveBeenCalledTimes(1);
    complete(1);
    await flush();
    await vi.advanceTimersByTimeAsync(999);
    expect(load).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(load).toHaveBeenCalledTimes(2);
    resource.stop();
  });
  it('cancels on navigation and ignores delayed results from the departed page', async () => {
    let complete!: (value: number) => void;
    let signal!: AbortSignal;
    const publish = vi.fn();
    const resource = poll(
      (next) => {
        signal = next;
        return new Promise<number>((resolve) => {
          complete = resolve;
        });
      },
      publish,
      1000,
      new Visibility(),
    );
    resource.stop();
    expect(signal.aborted).toBe(true);
    complete(99);
    await flush();
    await vi.advanceTimersByTimeAsync(5000);
    expect(publish).toHaveBeenCalledTimes(1);
  });
  it('pauses hidden pages and resumes one due refresh without a burst', async () => {
    const visibility = new Visibility();
    const load = vi.fn(async () => 1);
    const resource = poll(load, () => {}, 1000, visibility);
    await flush();
    visibility.set(true);
    await vi.advanceTimersByTimeAsync(10000);
    expect(load).toHaveBeenCalledTimes(1);
    visibility.set(false);
    await flush();
    expect(load).toHaveBeenCalledTimes(2);
    resource.stop();
  });
  it('keeps older result pages still while allowing an explicit refresh', async () => {
    const load = vi.fn(async () => [1, 2]);
    const resource = poll(load, () => {}, 1000, new Visibility());
    await flush();
    resource.setPeriod(0);
    await vi.advanceTimersByTimeAsync(10000);
    expect(load).toHaveBeenCalledTimes(1);
    resource.refresh();
    await flush();
    expect(load).toHaveBeenCalledTimes(2);
    resource.setPeriod(1000);
    await flush();
    expect(load).toHaveBeenCalledTimes(3);
    resource.stop();
  });
  it('retains successful data after a failed refresh and clears errors on recovery', async () => {
    const states: ResourceState<number>[] = [];
    const load = vi
      .fn<(_: AbortSignal) => Promise<number | null>>()
      .mockResolvedValueOnce(1)
      .mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce(2);
    const resource = poll(load, (state) => states.push(state), 1000, new Visibility());
    await flush();
    await vi.advanceTimersByTimeAsync(1000);
    expect(states.at(-1)).toMatchObject({ data: 1, loaded: true, loading: false, error: new Error('offline') });
    await vi.advanceTimersByTimeAsync(1000);
    expect(states.at(-1)).toMatchObject({ data: 2, error: null });
    resource.stop();
  });

  it('freezes an older page even when a newer response was already in flight', async () => {
    let complete!: (value: number) => void;
    let signal!: AbortSignal;
    const states: ResourceState<number>[] = [];
    const load = vi
      .fn<(next: AbortSignal) => Promise<number | null>>()
      .mockResolvedValueOnce(1)
      .mockImplementation((next) => {
        signal = next;
        return new Promise<number>((resolve) => {
          complete = resolve;
        });
      });
    const resource = poll(load, (state) => states.push(state), 1000, new Visibility());
    await flush();
    await vi.advanceTimersByTimeAsync(1000);
    resource.setPeriod(0);
    expect(signal.aborted).toBe(true);
    complete(2);
    await flush();
    expect(states.at(-1)).toMatchObject({ data: 1, loading: false });
    resource.stop();
  });
});
