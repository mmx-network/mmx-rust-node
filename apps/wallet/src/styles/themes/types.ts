export type ShellLayout = 'sidebar' | 'rail' | 'topbar';

export interface ThemeMeta {
  /** Folder name and value of VITE_WALLET_STYLE / ?style=. */
  id: string;
  /** Display name of the style. */
  name: string;
  /** Shell layout applied as data-layout on <html>. */
  layout: ShellLayout;
  /** Font families used by the style (bundled with the theme, never remote). */
  fonts: readonly string[];
  /** Browser UI color (<meta name="theme-color">) per scheme; matches --bg. */
  themeColor: { light: string; dark: string };
}

export interface ThemeEntry {
  meta: ThemeMeta;
  /** Loads the theme stylesheet (tokens for both schemes + component overrides). */
  load: () => Promise<unknown>;
}
