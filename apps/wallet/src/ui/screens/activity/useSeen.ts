import { useEffect, useState } from 'preact/hooks';

type Callback = () => void;

const callbacks = new Map<Element, Callback>();
let observer: IntersectionObserver | null = null;

function shared(): IntersectionObserver | null {
  if (typeof IntersectionObserver === 'undefined') return null;
  if (!observer) {
    observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (!entry.isIntersecting) continue;
          const cb = callbacks.get(entry.target);
          callbacks.delete(entry.target);
          observer?.unobserve(entry.target);
          cb?.();
        }
      },
      { rootMargin: '160px 0px' },
    );
  }
  return observer;
}

/**
 * True once the element has been on (or near) the screen. Used to look up
 * transaction details only for rows the user can actually see.
 */
export function useSeen<T extends Element>(ref: { current: T | null }): boolean {
  const [seen, setSeen] = useState(false);
  useEffect(() => {
    if (seen) return;
    const el = ref.current;
    const io = shared();
    if (!el || !io) {
      setSeen(true);
      return;
    }
    callbacks.set(el, () => setSeen(true));
    io.observe(el);
    return () => {
      callbacks.delete(el);
      io.unobserve(el);
    };
  }, [seen]);
  return seen;
}
