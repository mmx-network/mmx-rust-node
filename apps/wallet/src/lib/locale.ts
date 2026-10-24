/** The user's locale for number and date formatting (UI copy is English). */

let override: string | null = null;

function supported(tag: string): string | null {
  try {
    return Intl.NumberFormat.supportedLocalesOf([tag]).length > 0 ? tag : null;
  } catch {
    return null;
  }
}

export function getLocale(): string {
  if (override) return override;
  if (typeof navigator !== 'undefined') {
    const candidates = [...(navigator.languages ?? []), navigator.language];
    for (const tag of candidates) {
      if (typeof tag === 'string' && tag && supported(tag)) return tag;
    }
  }
  return 'en-US';
}

/** For tests and previews. Pass null to restore the browser locale. */
export function setLocale(locale: string | null): void {
  override = locale;
  separatorCache.clear();
}

export interface Separators {
  decimal: string;
  /** Grouping character; '' when the locale does not group digits. */
  group: string;
}

const separatorCache = new Map<string, Separators>();

export function getSeparators(locale = getLocale()): Separators {
  let s = separatorCache.get(locale);
  if (!s) {
    const parts = new Intl.NumberFormat(locale, { numberingSystem: 'latn', useGrouping: true }).formatToParts(
      1234567.5,
    );
    s = {
      decimal: parts.find((p) => p.type === 'decimal')?.value ?? '.',
      group: parts.find((p) => p.type === 'group')?.value ?? '',
    };
    separatorCache.set(locale, s);
  }
  return s;
}
