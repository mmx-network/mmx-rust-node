// Styles entry: base structure + the active theme.
//
// The theme is chosen at build time with VITE_WALLET_STYLE (default "obsidian").
// Development builds also accept ?style=<id> (and ?layout=<layout>) in the URL.
import './base.css';
import { DEFAULT_THEME, themes, type ThemeMeta } from './themes';
import { auroraImage } from './themes/obsidian/aurora';

export type Appearance = 'system' | 'light' | 'dark';
export type { ShellLayout, ThemeMeta } from './themes';

function selectTheme(): ThemeMeta['id'] {
  const configured = (import.meta.env.VITE_WALLET_STYLE as string | undefined)?.trim();
  let id = configured && themes[configured] ? configured : DEFAULT_THEME;
  if (import.meta.env.DEV && typeof location !== 'undefined') {
    const requested = new URLSearchParams(location.search).get('style');
    if (requested && Object.prototype.hasOwnProperty.call(themes, requested)) id = requested;
  }
  return id;
}

const themeId = selectTheme();
const entry = themes[themeId] ?? themes[DEFAULT_THEME]!;

/** Metadata of the active style. */
export const activeTheme: ThemeMeta = entry.meta;

let appearance: Appearance = 'system';
let media: MediaQueryList | null = null;

const resolveScheme = (): 'light' | 'dark' =>
  appearance === 'system' ? (media?.matches ? 'dark' : 'light') : appearance;

function paint(): void {
  const root = document.documentElement;
  const scheme = resolveScheme();
  root.dataset.scheme = scheme;
  let meta = document.querySelector<HTMLMetaElement>('meta[name="theme-color"]');
  if (!meta) {
    meta = document.createElement('meta');
    meta.name = 'theme-color';
    document.head.appendChild(meta);
  }
  meta.content = activeTheme.themeColor[scheme];
  drawAurora();
}

let auroraSize = '';

/** Lite effects: the Obsidian aurora as a pre-drawn image (see aurora.ts). */
function drawAurora(): void {
  const root = document.documentElement;
  if (root.dataset.effects !== 'lite' || activeTheme.id !== 'obsidian') return;
  auroraSize = `${window.innerWidth}x${window.innerHeight}x${root.dataset.scheme ?? ''}`;
  const url = auroraImage(window.innerWidth, window.innerHeight, getComputedStyle(root));
  if (url) root.style.setProperty('--ob-aurora-image', `url("${url}")`);
  else root.style.removeProperty('--ob-aurora-image');
}

/**
 * Applies the user's appearance setting. "system" follows the OS preference
 * live. Sets data-scheme on <html> and the matching theme-color.
 */
export function applyAppearance(value: Appearance): void {
  if (typeof document === 'undefined') return;
  appearance = value === 'light' || value === 'dark' ? value : 'system';
  paint();
}

let ready: Promise<void> = Promise.resolve();

if (typeof document !== 'undefined') {
  const root = document.documentElement;
  root.dataset.style = activeTheme.id;
  root.dataset.layout = activeTheme.layout;
  // WebKitGTK (Linux desktop app) often renders on the CPU: use cheaper effects.
  if (__PLATFORM__ === 'desktop' && /Linux/.test(navigator.userAgent) && !/Android/.test(navigator.userAgent)) {
    root.dataset.effects = 'lite';
  }
  if (import.meta.env.DEV) {
    const layout = new URLSearchParams(location.search).get('layout');
    if (layout === 'sidebar' || layout === 'rail' || layout === 'topbar') root.dataset.layout = layout;
    if (new URLSearchParams(location.search).has('lite')) root.dataset.effects = 'lite';
  }
  if (typeof window.matchMedia === 'function') {
    media = window.matchMedia('(prefers-color-scheme: dark)');
    media.addEventListener('change', () => {
      if (appearance === 'system') paint();
    });
  }
  paint();
  if (root.dataset.effects === 'lite') {
    let timer: ReturnType<typeof setTimeout> | undefined;
    window.addEventListener('resize', () => {
      clearTimeout(timer);
      timer = setTimeout(() => {
        if (auroraSize !== `${window.innerWidth}x${window.innerHeight}x${root.dataset.scheme ?? ''}`) drawAurora();
      }, 150);
    });
  }
  ready = entry.load().then(
    () => undefined,
    () => undefined,
  );
}

/** Resolves once the theme stylesheet has loaded (or failed; base styles still apply). Await before the first render. */
export const stylesReady: Promise<void> = ready;
