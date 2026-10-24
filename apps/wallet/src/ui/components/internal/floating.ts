import { useEffect, useLayoutEffect, useRef } from 'preact/hooks';
import type { RefObject } from 'preact';

export type Side = 'top' | 'bottom' | 'left' | 'right';
export type Align = 'start' | 'center' | 'end';
export type Placement = Side | `${Side}-${'start' | 'end'}`;

const GAP = 6;
const MARGIN = 8;

export function parsePlacement(placement: Placement): { side: Side; align: Align } {
  const [side, align] = placement.split('-') as [Side, 'start' | 'end' | undefined];
  return { side, align: align ?? 'center' };
}

const opposite: Record<Side, Side> = { top: 'bottom', bottom: 'top', left: 'right', right: 'left' };

/** Positions a `position: fixed` panel next to an anchor, flipping and clamping into the viewport. */
export function placeFloating(anchor: HTMLElement, panel: HTMLElement, placement: Placement, matchWidth = false): void {
  const { side: preferred, align } = parsePlacement(placement);
  const vw = document.documentElement.clientWidth;
  const vh = document.documentElement.clientHeight;
  const a = anchor.getBoundingClientRect();
  if (matchWidth) panel.style.minWidth = `${Math.round(a.width)}px`;
  panel.style.maxHeight = '';
  const pw = panel.offsetWidth;
  let ph = panel.offsetHeight;

  const fits = (side: Side): boolean => {
    switch (side) {
      case 'bottom':
        return a.bottom + GAP + ph <= vh - MARGIN;
      case 'top':
        return a.top - GAP - ph >= MARGIN;
      case 'right':
        return a.right + GAP + pw <= vw - MARGIN;
      case 'left':
        return a.left - GAP - pw >= MARGIN;
    }
  };
  let side = preferred;
  if (!fits(side) && fits(opposite[side])) side = opposite[side];

  if (side === 'top' || side === 'bottom') {
    const room = side === 'bottom' ? vh - MARGIN - (a.bottom + GAP) : a.top - GAP - MARGIN;
    if (ph > room && room > 120) {
      panel.style.maxHeight = `${Math.floor(room)}px`;
      ph = room;
    }
  }

  let top: number;
  let left: number;
  if (side === 'bottom' || side === 'top') {
    top = side === 'bottom' ? a.bottom + GAP : a.top - GAP - ph;
    left = align === 'start' ? a.left : align === 'end' ? a.right - pw : a.left + a.width / 2 - pw / 2;
  } else {
    left = side === 'right' ? a.right + GAP : a.left - GAP - pw;
    top = align === 'start' ? a.top : align === 'end' ? a.bottom - ph : a.top + a.height / 2 - ph / 2;
  }
  left = Math.max(MARGIN, Math.min(left, vw - pw - MARGIN));
  top = Math.max(MARGIN, Math.min(top, vh - ph - MARGIN));
  panel.style.top = `${Math.round(top)}px`;
  panel.style.left = `${Math.round(left)}px`;
  panel.dataset.side = side;
}

const popoverSupported =
  typeof HTMLElement !== 'undefined' && typeof HTMLElement.prototype.showPopover === 'function';

/** Moves the element into the top layer when the Popover API is available. */
export function showInTopLayer(el: HTMLElement): void {
  if (!popoverSupported) return;
  if (!el.hasAttribute('popover')) el.setAttribute('popover', 'manual');
  try {
    if (el.matches(':popover-open')) el.hidePopover();
    el.showPopover();
  } catch {
    // Not connected or already handled; the fixed-position fallback still applies.
  }
}

/** Keeps a floating panel attached to its anchor while it is open. */
export function useFloatingPosition(
  open: boolean,
  anchor: RefObject<HTMLElement>,
  panel: RefObject<HTMLElement>,
  placement: Placement,
  matchWidth = false,
): void {
  useLayoutEffect(() => {
    if (!open || !anchor.current || !panel.current) return;
    const p = panel.current;
    showInTopLayer(p);
    placeFloating(anchor.current, p, placement, matchWidth);
    const update = (): void => {
      if (anchor.current && panel.current) placeFloating(anchor.current, panel.current, placement, matchWidth);
    };
    window.addEventListener('resize', update);
    window.addEventListener('scroll', update, true);
    const observer = typeof ResizeObserver === 'function' ? new ResizeObserver(update) : null;
    observer?.observe(p);
    return () => {
      window.removeEventListener('resize', update);
      window.removeEventListener('scroll', update, true);
      observer?.disconnect();
    };
  }, [open, placement, matchWidth]);
}

/** Calls `onOutside` for pointer presses outside every given element while active. */
export function useOutsidePress(active: boolean, refs: Array<RefObject<HTMLElement>>, onOutside: () => void): void {
  const callback = useRef(onOutside);
  callback.current = onOutside;
  useEffect(() => {
    if (!active) return;
    const handler = (event: PointerEvent): void => {
      const target = event.target as Node | null;
      if (!target) return;
      for (const ref of refs) {
        if (ref.current && ref.current.contains(target)) return;
      }
      callback.current();
    };
    document.addEventListener('pointerdown', handler, true);
    return () => document.removeEventListener('pointerdown', handler, true);
  }, [active]);
}
