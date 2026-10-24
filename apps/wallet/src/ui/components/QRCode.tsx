import { useMemo } from 'preact/hooks';
import { encode } from 'uqr';
import { cx } from './internal/cx';

export interface QRCodeProps {
  value: string;
  /** Rendered size in px (default 200). The code itself is a crisp SVG at any size. */
  size?: number;
  /** Accessible name, e.g. "QR code of your address". */
  label: string;
  ecc?: 'L' | 'M' | 'Q' | 'H';
  class?: string;
}

const QUIET = 4;

/** Dark-on-light QR code (always, for reliable scanning) with a 4-module quiet zone. */
export function QRCode({ value, size = 200, label, ecc = 'M', class: className }: QRCodeProps) {
  const { path, dim } = useMemo(() => {
    const qr = encode(value, { ecc, border: 0 });
    let d = '';
    qr.data.forEach((row, y) => {
      let x = 0;
      while (x < row.length) {
        if (!row[x]) {
          x++;
          continue;
        }
        const start = x;
        while (x < row.length && row[x]) x++;
        d += `M${start + QUIET} ${y + QUIET}h${x - start}v1h-${x - start}Z`;
      }
    });
    return { path: d, dim: qr.size + QUIET * 2 };
  }, [value, ecc]);
  return (
    <svg
      class={cx('qr', className)}
      width={size}
      height={size}
      viewBox={`0 0 ${dim} ${dim}`}
      shape-rendering="crispEdges"
      role="img"
      aria-label={label}
      focusable="false"
    >
      <rect class="qr__light" width={dim} height={dim} fill="#ffffff" />
      <path class="qr__dark" d={path} fill="#0b0a10" />
    </svg>
  );
}
