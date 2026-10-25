import { useEffect, useRef, useState } from 'preact/hooks';
import { api } from './api';
import { initialState, poll } from './poll';

export function useResource<T>(path: string, interval = 0, load?: (signal: AbortSignal) => Promise<T | null>) {
  const [current, setState] = useState({ path, state: initialState<T>() });
  const control = useRef<ReturnType<typeof poll<T>> | null>(null);
  useEffect(() => {
    setState({ path, state: initialState<T>() });
    const resource = poll<T>(
      load ?? ((signal) => api.get<T>(path, signal)),
      (state) => setState({ path, state }),
      interval,
      document,
    );
    control.current = resource;
    const refresh = (): void => resource.refresh();
    window.addEventListener('explorer:refresh', refresh);
    return () => {
      resource.stop();
      window.removeEventListener('explorer:refresh', refresh);
    };
  }, [path]);
  useEffect(() => control.current?.setPeriod(interval), [interval, path]);
  return { ...(current.path === path ? current.state : initialState<T>()), refresh: () => control.current?.refresh() };
}
