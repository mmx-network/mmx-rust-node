// Theme registry. The shipped style is Obsidian; its stylesheet is part of the
// main CSS so the first frame already has the right colours. Adding a style =
// a folder with theme.css + theme.ts and one entry below (loaded on demand).
import './obsidian/theme.css';
import obsidian from './obsidian/theme';
import type { ThemeEntry } from './types';

export type { ShellLayout, ThemeEntry, ThemeMeta } from './types';

export const themes: Record<string, ThemeEntry> = {
  obsidian: { meta: obsidian, load: () => Promise.resolve() },
};

export const DEFAULT_THEME = 'obsidian';
