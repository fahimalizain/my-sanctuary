// Pure theme preference helpers for the light / dark / system engine.
// No React, no side effects beyond the document passed into applyResolvedTheme.
//
// Keep in sync with the inline boot script in apps/web/index.html: it must
// resolve the theme before the bundle loads to avoid a flash of the wrong
// theme, so it duplicates the storage key, the resolved colors, and the
// preference rules below.

export const THEME_STORAGE_KEY = 'sanctuary.theme';
export const THEME_COLOR_LIGHT = '#204f0a';
export const THEME_COLOR_DARK = '#11140f';

export type ThemePreference = 'light' | 'dark' | 'system';
export type ResolvedTheme = 'light' | 'dark';

/**
 * Only the exact lowercase tokens are honored.
 * null, '', and anything else fall back to 'light' — the app is light today
 * and the settings UI shows Light selected by default.
 */
export function parseThemePreference(raw: string | null): ThemePreference {
  return raw === 'light' || raw === 'dark' || raw === 'system' ? raw : 'light';
}

/** Map a preference plus the current system setting to a concrete theme. */
export function resolveTheme(
  preference: ThemePreference,
  systemDark: boolean,
): ResolvedTheme {
  if (preference === 'light') return 'light';
  if (preference === 'dark') return 'dark';
  return systemDark ? 'dark' : 'light';
}

/** Read the stored preference. Blocked storage falls back to light. */
export function readThemePreference(
  storage: Pick<Storage, 'getItem'>,
): ThemePreference {
  try {
    return parseThemePreference(storage.getItem(THEME_STORAGE_KEY));
  } catch {
    return 'light';
  }
}

/** Persist the preference. Quota errors / private mode are swallowed. */
export function writeThemePreference(
  storage: Pick<Storage, 'setItem'>,
  preference: ThemePreference,
): void {
  try {
    storage.setItem(THEME_STORAGE_KEY, preference);
  } catch {
    // Storage unavailable — the in-memory preference still applies.
  }
}

/**
 * Apply a resolved theme to the document: the `dark` class drives the CSS
 * token overrides, color-scheme drives native controls, and the theme-color
 * meta tints the browser chrome.
 */
export function applyResolvedTheme(
  doc: Document,
  resolved: ResolvedTheme,
): void {
  const isDark = resolved === 'dark';

  doc.documentElement.classList.toggle('dark', isDark);
  doc.documentElement.style.colorScheme = resolved;

  const meta = doc.querySelector('meta[name="theme-color"]');
  if (meta) {
    meta.setAttribute('content', isDark ? THEME_COLOR_DARK : THEME_COLOR_LIGHT);
  }
}
