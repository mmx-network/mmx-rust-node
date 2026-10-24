import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from 'preact/hooks';
import type { BalanceSeries, SeriesRange } from '../../../state';
import { formatAmount } from '../../../lib/amount';
import { formatDate, formatDateTime, TIME } from '../../../lib/time';
import { Skeleton } from '../../components';
import { cx } from '../../components/internal/cx';
import { formatTick, formatTimeTick, indexAt, stepPath, timeTicks, toDisplayNumber, valueScale } from './chart';

const PAD_TOP = 14;
const PAD_BOTTOM = 26;
const PAD_X = 2;

export interface BalanceChartProps {
  series: BalanceSeries | null;
  decimals: number;
  symbol: string;
  range: SeriesRange;
  /** Older history is being fetched for the range. */
  loading: boolean;
  /** Accessible name, e.g. "Balance history, past month". */
  label: string;
}

interface Readout {
  t: number;
  /** Set when chosen with the keyboard: the point index. */
  index?: number;
  touch?: boolean;
}

/**
 * Balance over time as a step area chart. Hover or drag for a readout of the
 * balance at that moment; with focus, ← → move between changes and Home/End
 * jump to the first and last value.
 */
export function BalanceChart({ series, decimals, symbol, range, loading, label }: BalanceChartProps) {
  const plotRef = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ w: 0, h: 0 });
  const [readout, setReadout] = useState<Readout | null>(null);
  const auto = useId();
  const gradientId = `bc${auto}-fill`;
  const summaryId = `bc${auto}-summary`;

  useLayoutEffect(() => {
    const el = plotRef.current;
    if (!el) return;
    const measure = (w: number, h: number) =>
      setSize((prev) => (prev.w === Math.round(w) && prev.h === Math.round(h) ? prev : { w: Math.round(w), h: Math.round(h) }));
    measure(el.clientWidth, el.clientHeight);
    if (typeof ResizeObserver === 'undefined') return;
    const ro = new ResizeObserver((entries) => {
      const r = entries[0]?.contentRect;
      if (r) measure(r.width, r.height);
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [series === null]);

  // Another asset or range invalidates the readout (regular refreshes keep it).
  useEffect(() => setReadout(null), [series?.contract, range]);

  // A touch readout stays after lifting the finger until the user taps elsewhere.
  useEffect(() => {
    if (!readout?.touch) return;
    const onDown = (event: PointerEvent) => {
      if (plotRef.current && event.target instanceof Node && plotRef.current.contains(event.target)) return;
      setReadout(null);
    };
    document.addEventListener('pointerdown', onDown, true);
    return () => document.removeEventListener('pointerdown', onDown, true);
  }, [readout?.touch]);

  const values = useMemo(() => (series ? series.points.map((p) => toDisplayNumber(p.v, decimals)) : []), [series, decimals]);
  const scale = useMemo(
    () => (series ? valueScale(toDisplayNumber(series.min, decimals), toDisplayNumber(series.max, decimals)) : null),
    [series, decimals],
  );

  const plotW = Math.max(1, size.w - PAD_X * 2);
  const plotH = Math.max(1, size.h - PAD_TOP - PAD_BOTTOM);
  const t0 = series?.start ?? 0;
  const t1 = series && series.end > series.start ? series.end : t0 + 1;
  const xOf = (t: number) => PAD_X + ((Math.min(Math.max(t, t0), t1) - t0) / (t1 - t0)) * plotW;
  const yOf = (v: number) => (scale ? PAD_TOP + (1 - (v - scale.lo) / (scale.hi - scale.lo || 1)) * plotH : PAD_TOP + plotH);
  const bottom = PAD_TOP + plotH;

  const paths = useMemo(() => {
    if (!series || !scale || size.w === 0 || series.points.length === 0) return null;
    const xs = series.points.map((p) => xOf(p.t));
    const ys = values.map((v) => yOf(v));
    const line = stepPath(xs, ys, PAD_X + plotW);
    const startX = Math.round(xs[0]! * 2) / 2;
    return { line, area: `${line}V${bottom}H${startX}Z`, endY: ys[ys.length - 1]! };
    // xOf/yOf derive from the listed inputs.
  }, [series, values, scale, size.w, size.h]);

  if (!series || !scale) {
    return (
      <div class="overview-chart" aria-busy="true">
        <div class="overview-chart__plot overview-chart__plot--skeleton" ref={plotRef}>
          <Skeleton shape="rect" height="100%" />
        </div>
      </div>
    );
  }

  const points = series.points;
  const format = (v: bigint) => `${formatAmount(v, decimals)} ${symbol}`;
  const shortSpan = t1 - t0 <= 8 * TIME.DAY;
  const formatWhen = (t: number) => (shortSpan ? formatDateTime(t) : formatDate(t));
  const flatZero = series.max === 0n;

  const axis = size.w > 0 ? timeTicks(t0, t1, Math.max(3, Math.floor(plotW / 115))) : { unit: 'day' as const, ticks: [] };

  let active: { t: number; v: bigint; x: number; y: number } | null = null;
  if (readout && points.length > 0) {
    const i = Math.min(points.length - 1, Math.max(0, readout.index ?? indexAt(points, readout.t)));
    const p = points[i]!;
    const t = readout.index !== undefined ? p.t : Math.min(Math.max(readout.t, t0), t1);
    active = { t, v: p.v, x: xOf(t), y: yOf(values[i]!) };
  }

  const timeAt = (clientX: number): number => {
    const el = plotRef.current;
    if (!el) return t1;
    const rect = el.getBoundingClientRect();
    const x = Math.min(Math.max(clientX - rect.left - PAD_X, 0), plotW);
    return t0 + (x / plotW) * (t1 - t0);
  };

  const moveKey = (key: string): boolean => {
    const last = points.length - 1;
    if (last < 0) return false;
    const current = readout ? Math.min(last, readout.index ?? indexAt(points, readout.t)) : last + 1;
    let next: number;
    if (key === 'ArrowLeft') next = Math.max(0, Math.min(last, current - 1));
    else if (key === 'ArrowRight') next = Math.min(last, current >= last + 1 ? last : current + 1);
    else if (key === 'Home') next = 0;
    else if (key === 'End') next = last;
    else return false;
    setReadout({ t: points[next]!.t, index: next });
    return true;
  };

  const first = points[0];
  const lastPoint = points[points.length - 1];
  const summary =
    first && lastPoint
      ? `${format(first.v)} on ${formatDate(series.start)}, ${format(lastPoint.v)} now. Use the arrow keys to read the changes.`
      : '';
  const tooltipRight = active ? active.x > size.w * 0.62 : false;

  return (
    <div class={cx('overview-chart', loading && 'overview-chart--loading')} aria-busy={loading ? 'true' : undefined}>
      <div
        ref={plotRef}
        class="overview-chart__plot"
        role="group"
        aria-label={label}
        aria-describedby={summaryId}
        tabIndex={0}
        onPointerDown={(event) => setReadout({ t: timeAt(event.clientX), touch: event.pointerType !== 'mouse' })}
        onPointerMove={(event) => {
          if (event.pointerType === 'mouse' || event.buttons !== 0) {
            setReadout({ t: timeAt(event.clientX), touch: event.pointerType !== 'mouse' });
          }
        }}
        onPointerLeave={(event) => {
          if (event.pointerType === 'mouse') setReadout(null);
        }}
        onPointerCancel={() => setReadout(null)}
        onBlur={() => setReadout((r) => (r?.index !== undefined ? null : r))}
        onKeyDown={(event) => {
          if (event.key === 'Escape') {
            if (readout) {
              event.preventDefault();
              setReadout(null);
            }
          } else if (moveKey(event.key)) {
            event.preventDefault();
          }
        }}
      >
        {size.w > 0 && paths ? (
          <svg class="overview-chart__svg" width={size.w} height={size.h} aria-hidden="true" focusable="false">
            <defs>
              <linearGradient id={gradientId} x1="0" y1="0" x2="0" y2="1">
                <stop offset="0%" class="overview-chart__stop overview-chart__stop--top" />
                <stop offset="100%" class="overview-chart__stop overview-chart__stop--bottom" />
              </linearGradient>
            </defs>
            <g class="overview-chart__grid">
              {scale.ticks.map((v) => {
                const y = Math.round(yOf(v)) + 0.5;
                return <line key={v} class="overview-chart__gridline" x1={PAD_X} x2={PAD_X + plotW} y1={y} y2={y} />;
              })}
            </g>
            <path class="overview-chart__area" d={paths.area} fill={`url(#${gradientId})`} />
            <path class="overview-chart__line" d={paths.line} />
            <circle class="overview-chart__end" cx={PAD_X + plotW} cy={paths.endY} r="3.5" />
            <g class="overview-chart__yaxis">
              {scale.ticks.map((v) =>
                flatZero && v !== 0 ? null : (
                  <text key={v} class="overview-chart__ytick" x={PAD_X} y={Math.round(yOf(v)) - 4.5}>
                    {formatTick(v, scale.step)}
                  </text>
                ),
              )}
            </g>
            <g class="overview-chart__xaxis">
              {axis.ticks.map((t) => {
                const x = xOf(t);
                const anchor = x < 24 ? 'start' : x > size.w - 24 ? 'end' : 'middle';
                return (
                  <text key={t} class="overview-chart__xtick" x={anchor === 'start' ? Math.max(PAD_X, x) : x} y={size.h - 7} text-anchor={anchor}>
                    {formatTimeTick(t, axis.unit)}
                  </text>
                );
              })}
            </g>
            {active ? (
              <g class="overview-chart__cursor">
                <line class="overview-chart__crosshair" x1={Math.round(active.x) + 0.5} x2={Math.round(active.x) + 0.5} y1={PAD_TOP - 6} y2={bottom} />
                <circle class="overview-chart__dot" cx={active.x} cy={active.y} r="5" />
              </g>
            ) : null}
          </svg>
        ) : null}
        {flatZero && size.w > 0 ? <span class="overview-chart__empty">{`No ${symbol} in this period`}</span> : null}
        {active ? (
          <div
            class={cx('overview-chart__tooltip', tooltipRight && 'overview-chart__tooltip--left')}
            style={{ left: `${active.x}px` }}
            aria-hidden="true"
          >
            <span class="overview-chart__tooltip-value">{format(active.v)}</span>
            <span class="overview-chart__tooltip-time">{formatWhen(active.t)}</span>
          </div>
        ) : null}
      </div>
      <p id={summaryId} class="visually-hidden">
        {summary}
      </p>
      <span class="visually-hidden" aria-live="polite">
        {active && readout?.index !== undefined ? `${format(active.v)}, ${formatWhen(active.t)}` : ''}
      </span>
    </div>
  );
}
