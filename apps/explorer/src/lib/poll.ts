export interface ResourceState<T> {
  data: T | null;
  loading: boolean;
  loaded: boolean;
  error: Error | null;
  updated: number | null;
}
export const initialState = <T>(): ResourceState<T> => ({
  data: null,
  loading: true,
  loaded: false,
  error: null,
  updated: null,
});

/** One request at a time; page/visibility changes cancel its subscription. */
export function poll<T>(
  load: (signal: AbortSignal) => Promise<T | null>,
  publish: (state: ResourceState<T>) => void,
  interval: number,
  visibility: Pick<Document, 'hidden' | 'addEventListener' | 'removeEventListener'>,
): { refresh: () => void; stop: () => void; setPeriod: (value: number) => void } {
  let period = interval;
  let state = initialState<T>();
  let stopped = false;
  let controller: AbortController | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let lastAttempt: number | null = null;
  const run = (): void => {
    if (stopped || controller || visibility.hidden) return;
    clearTimeout(timer);
    const request = new AbortController();
    controller = request;
    lastAttempt = Date.now();
    state = { ...state, loading: true };
    publish(state);
    void load(request.signal)
      .then(
        (data) => {
          if (stopped || request.signal.aborted) return;
          state = { data, loading: false, loaded: true, error: null, updated: Date.now() };
          publish(state);
        },
        (error) => {
          if (stopped || request.signal.aborted) return;
          state = {
            ...state,
            loading: false,
            loaded: true,
            error: error instanceof Error ? error : new Error('Could not load data.'),
          };
          publish(state);
        },
      )
      .finally(() => {
        if (controller !== request) return;
        controller = null;
        if (!stopped && !visibility.hidden && period) timer = setTimeout(run, period);
      });
  };
  const visible = (): void => {
    if (visibility.hidden) {
      clearTimeout(timer);
      if (controller) {
        controller.abort();
        controller = null;
        lastAttempt = null;
      }
    } else if (lastAttempt === null || (period && Date.now() - lastAttempt >= period)) run();
    else if (period) timer = setTimeout(run, Math.max(0, period - (Date.now() - lastAttempt)));
  };
  visibility.addEventListener('visibilitychange', visible);
  run();
  return {
    refresh: run,
    setPeriod: (value) => {
      if (value === period) return;
      clearTimeout(timer);
      period = value;
      // Freezing an older page must also fence an already-running refresh.
      // An initial read still completes, so a bookmarked older page can load.
      if (!period && controller && state.loaded) {
        controller.abort();
        controller = null;
        state = { ...state, loading: false };
        publish(state);
      }
      if (period && !controller && !visibility.hidden) run();
    },
    stop: () => {
      stopped = true;
      clearTimeout(timer);
      controller?.abort();
      visibility.removeEventListener('visibilitychange', visible);
    },
  };
}
