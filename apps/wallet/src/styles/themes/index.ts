// Theme registry. Adding a style = a folder with theme.css + theme.ts and one
// line below.
import draft from './draft/theme';
import meridian from './meridian/theme';
import atrium from './atrium/theme';
import ledger from './ledger/theme';
import obsidian from './obsidian/theme';
import terminal from './terminal/theme';
import fjord from './fjord/theme';
import blockprint from './blockprint/theme';
import sovereign from './sovereign/theme';
import verdant from './verdant/theme';
import prism from './prism/theme';
import type { ThemeEntry } from './types';

export type { ShellLayout, ThemeEntry, ThemeMeta } from './types';

export const themes: Record<string, ThemeEntry> = {
  draft: { meta: draft, load: () => import('./draft/theme.css') },
  meridian: { meta: meridian, load: () => import('./meridian/theme.css') },
  atrium: { meta: atrium, load: () => import('./atrium/theme.css') },
  ledger: { meta: ledger, load: () => import('./ledger/theme.css') },
  obsidian: { meta: obsidian, load: () => import('./obsidian/theme.css') },
  terminal: { meta: terminal, load: () => import('./terminal/theme.css') },
  fjord: { meta: fjord, load: () => import('./fjord/theme.css') },
  blockprint: { meta: blockprint, load: () => import('./blockprint/theme.css') },
  sovereign: { meta: sovereign, load: () => import('./sovereign/theme.css') },
  verdant: { meta: verdant, load: () => import('./verdant/theme.css') },
  prism: { meta: prism, load: () => import('./prism/theme.css') },
};

export const DEFAULT_THEME = 'draft';
