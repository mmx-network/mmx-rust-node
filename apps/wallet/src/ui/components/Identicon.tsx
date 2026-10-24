import { fnv1a, xorshift } from './internal/hash';
import { cx } from './internal/cx';

export interface IdenticonProps {
  /** Any stable string: address, wallet id, contract. */
  value: string;
  /** Pixel size (default 32). */
  size?: number;
  /** Accessible name; decorative when omitted. */
  label?: string;
  class?: string;
}

interface Shapes {
  base: string;
  halo: string;
  band: string;
  disc: string;
  angle: number;
  bandX: number;
  bandW: number;
  haloX: number;
  haloY: number;
  haloR: number;
  discX: number;
  discY: number;
  discR: number;
}

const hsl = (h: number, s: number, l: number): string =>
  `hsl(${Math.round(((h % 360) + 360) % 360)} ${Math.round(s)}% ${Math.round(l)}%)`;

const DISC_SPOTS: ReadonlyArray<readonly [number, number]> = [
  [21, 21],
  [43, 21],
  [21, 43],
  [43, 43],
  [32, 19],
  [19, 32],
  [45, 32],
  [32, 45],
];

const cache = new Map<string, Shapes>();

/** Shape parameters for a value (exported for tests). */
export function identiconShapes(value: string): Shapes {
  const hit = cache.get(value);
  if (hit) return hit;
  const r = xorshift(fnv1a(value));
  const hue = r() * 360;
  const turn = r() < 0.5 ? -1 : 1;
  const [discX, discY] = DISC_SPOTS[Math.floor(r() * DISC_SPOTS.length)]!;
  const shapes: Shapes = {
    base: hsl(hue, 44 + r() * 14, 46 + r() * 8),
    halo: hsl(hue + turn * (12 + r() * 16), 48 + r() * 12, 27 + r() * 7),
    band: hsl(hue - turn * (16 + r() * 18), 58 + r() * 16, 70 + r() * 6),
    disc: hsl(hue + 165 + r() * 30, 56 + r() * 14, 62 + r() * 8),
    angle: (r() < 0.5 ? -1 : 1) * (30 + r() * 30),
    bandX: 32 + (r() - 0.5) * 20,
    bandW: 13 + r() * 6,
    haloX: r() < 0.5 ? 8 + r() * 10 : 56 - r() * 10,
    haloY: r() < 0.5 ? 8 + r() * 10 : 56 - r() * 10,
    haloR: 24 + r() * 12,
    discX,
    discY,
    discR: 8 + r() * 4,
  };
  if (cache.size > 500) cache.clear();
  cache.set(value, shapes);
  return shapes;
}

/** Deterministic geometric avatar derived from a string. */
export function Identicon({ value, size = 32, label, class: className }: IdenticonProps) {
  const s = identiconShapes(value);
  return (
    <svg
      class={cx('identicon', className)}
      width={size}
      height={size}
      viewBox="0 0 64 64"
      focusable="false"
      role={label ? 'img' : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : 'true'}
    >
      <rect width="64" height="64" fill={s.base} />
      <circle cx={s.haloX.toFixed(1)} cy={s.haloY.toFixed(1)} r={s.haloR.toFixed(1)} fill={s.halo} />
      <rect
        x={(s.bandX - s.bandW / 2).toFixed(1)}
        y="-24"
        width={s.bandW.toFixed(1)}
        height="112"
        fill={s.band}
        transform={`rotate(${s.angle.toFixed(1)} 32 32)`}
      />
      <circle cx={s.discX} cy={s.discY} r={s.discR.toFixed(1)} fill={s.disc} />
    </svg>
  );
}
