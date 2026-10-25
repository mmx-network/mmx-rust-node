/**
 * The Obsidian aurora as one image, for lite effects (WebKitGTK on the CPU):
 * blitting an image is far cheaper than painting three radial gradients on
 * every page change. Mirrors `body::before` in theme.css.
 */

interface Glow {
  variable: string;
  /** Ellipse radii in CSS px. */
  rx: number;
  ry: number;
  /** Centre as a fraction of the viewport. */
  x: number;
  y: number;
}

// Painted bottom first (CSS lists the top layer first).
const GLOWS: readonly Glow[] = [
  { variable: '--ob-glow-2', rx: 640, ry: 480, x: 0.38, y: 0.46 },
  { variable: '--ob-glow-3', rx: 740, ry: 520, x: -0.08, y: 1.08 },
  { variable: '--ob-glow-1', rx: 820, ry: 520, x: 0.62, y: -0.14 },
];
// The colour-mix ramp of each gradient in theme.css: [stop, share of the colour].
const RAMP: ReadonlyArray<readonly [number, number]> = [
  [0, 1],
  [0.18, 0.9],
  [0.36, 0.66],
  [0.52, 0.42],
  [0.68, 0.21],
  [0.84, 0.07],
  [1, 0],
];
// The image is drawn at half size and scaled up: the gradients are smooth.
const SCALE = 0.5;

function parseColor(value: string): [number, number, number, number] | null {
  const m = /^rgba?\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)(?:[\s,/]+([\d.]+%?))?\s*\)$/u.exec(value.trim());
  if (!m) return null;
  const alpha = m[4] === undefined ? 1 : m[4].endsWith('%') ? Number.parseFloat(m[4]) / 100 : Number(m[4]);
  return [Number(m[1]), Number(m[2]), Number(m[3]), alpha];
}

/** A data URL of the aurora for the current scheme and viewport, or null when it can't be drawn. */
export function auroraImage(width: number, height: number, style: CSSStyleDeclaration): string | null {
  const w = Math.max(1, Math.round(width * SCALE));
  const h = Math.max(1, Math.round(height * SCALE));
  const canvas = document.createElement('canvas');
  canvas.width = w;
  canvas.height = h;
  const g = canvas.getContext('2d');
  if (!g) return null;
  for (const glow of GLOWS) {
    const color = parseColor(style.getPropertyValue(glow.variable));
    if (!color) continue;
    const [r, gr, b, a] = color;
    g.save();
    g.translate(glow.x * w, glow.y * h);
    g.scale(1, glow.ry / glow.rx);
    const radius = glow.rx * SCALE;
    const gradient = g.createRadialGradient(0, 0, 0, 0, 0, radius);
    for (const [stop, share] of RAMP) gradient.addColorStop(stop, `rgba(${r}, ${gr}, ${b}, ${a * share})`);
    g.fillStyle = gradient;
    g.fillRect(-radius, -radius, radius * 2, radius * 2);
    g.restore();
  }
  try {
    return canvas.toDataURL('image/png');
  } catch {
    return null;
  }
}
