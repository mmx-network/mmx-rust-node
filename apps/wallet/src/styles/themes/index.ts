// Theme registry. Adding a style = a folder with theme.css + theme.ts and one
// line below.
import draft from './draft/theme';
import type { ThemeEntry } from './types';

export type { ShellLayout, ThemeEntry, ThemeMeta } from './types';

export const themes: Record<string, ThemeEntry> = {
  draft: { meta: draft, load: () => import('./draft/theme.css') },
};

export const DEFAULT_THEME = 'draft';
