import { cx } from './internal/cx';

// Official MMX mark, traced from the circular logo (256 × 256 grid).
export const MMX_GLYPH =
  'M94.29 72H117.66L174.78 184H138.34L87.16 83.64A8 8 0 0 1 94.29 72Z' +
  'M147.08 72H183.48L158.65 120.69L140.45 85Z' +
  'M108.92 184H72.52L97.35 135.31L115.55 171Z' +
  'M112.77 34H128.54L126.6 58.2H110.83Z' +
  'M142.19 34H157.95L156.01 58.2H140.25Z' +
  'M99.99 197.8H115.75L113.81 222H98.05Z' +
  'M129.4 197.8H145.17L143.23 222H127.46Z';

export interface BrandMarkProps {
  /** Pixel size (default 32). */
  size?: number;
  /** color: purple disc with white glyph; glyph: glyph only in currentColor. */
  variant?: 'color' | 'glyph';
  /** Accessible name; decorative when omitted. */
  label?: string;
  class?: string;
}

export function BrandMark({ size = 32, variant = 'color', label, class: className }: BrandMarkProps) {
  return (
    <svg
      class={cx('brand-mark', `brand-mark--${variant}`, className)}
      width={size}
      height={size}
      viewBox={variant === 'glyph' ? '28 28 200 200' : '0 0 256 256'}
      focusable="false"
      role={label ? 'img' : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : 'true'}
    >
      {variant === 'color' ? <circle class="brand-mark__disc" cx="128" cy="128" r="128" /> : null}
      <path class="brand-mark__glyph" d={MMX_GLYPH} />
    </svg>
  );
}

export interface BrandLockupProps {
  size?: 'sm' | 'md' | 'lg';
  /** Product name next to the mark (default "MMX Wallet"). */
  name?: string;
  class?: string;
}

export function BrandLockup({ size = 'md', name = 'MMX Wallet', class: className }: BrandLockupProps) {
  const px = size === 'lg' ? 44 : size === 'sm' ? 26 : 32;
  return (
    <span class={cx('brand', `brand--${size}`, className)}>
      <BrandMark size={px} />
      <span class="brand__name">{name}</span>
    </span>
  );
}
