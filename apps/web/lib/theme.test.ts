import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { JSDOM } from 'jsdom';
import {
  applyResolvedTheme,
  parseThemePreference,
  readThemePreference,
  resolveTheme,
  THEME_COLOR_DARK,
  THEME_COLOR_LIGHT,
  THEME_STORAGE_KEY,
  writeThemePreference,
} from './theme';

const THEME_COLOR_META = '<meta name="theme-color" content="#204f0a" />';

function createThemeDocument(meta = THEME_COLOR_META): Document {
  const dom = new JSDOM(
    `<!DOCTYPE html><html><head>${meta}</head><body></body></html>`,
  );
  return dom.window.document;
}

function fakeStorage(initial: Record<string, string> = {}) {
  const data = new Map(Object.entries(initial));
  return {
    getItem(key: string): string | null {
      return data.get(key) ?? null;
    },
    setItem(key: string, value: string): void {
      data.set(key, value);
    },
  };
}

test('parseThemePreference passes through the exact lowercase tokens', () => {
  assert.equal(parseThemePreference('light'), 'light');
  assert.equal(parseThemePreference('dark'), 'dark');
  assert.equal(parseThemePreference('system'), 'system');
});

test('parseThemePreference falls back to light for null, empty and unknown values', () => {
  for (const raw of [null, '', 'DARK', 'System', 'nope', ' light', 'light ']) {
    assert.equal(parseThemePreference(raw), 'light', JSON.stringify(raw));
  }
});

test('resolveTheme maps explicit preferences regardless of the system', () => {
  for (const systemDark of [true, false]) {
    assert.equal(resolveTheme('light', systemDark), 'light');
    assert.equal(resolveTheme('dark', systemDark), 'dark');
  }
});

test('resolveTheme follows the system for the system preference', () => {
  assert.equal(resolveTheme('system', true), 'dark');
  assert.equal(resolveTheme('system', false), 'light');
});

test('readThemePreference round-trips through storage', () => {
  const storage = fakeStorage();

  for (const preference of ['light', 'dark', 'system'] as const) {
    writeThemePreference(storage, preference);
    assert.equal(readThemePreference(storage), preference);
  }

  assert.equal(storage.getItem(THEME_STORAGE_KEY), 'system');
});

test('readThemePreference treats a missing key as light', () => {
  assert.equal(readThemePreference(fakeStorage()), 'light');
});

test('readThemePreference returns light when getItem throws', () => {
  const storage = {
    getItem(): string | null {
      throw new Error('storage blocked');
    },
  };
  assert.equal(readThemePreference(storage), 'light');
});

test('writeThemePreference stores the raw preference string', () => {
  const storage = fakeStorage();
  writeThemePreference(storage, 'dark');
  assert.equal(storage.getItem(THEME_STORAGE_KEY), 'dark');
});

test('writeThemePreference swallows a throwing setItem', () => {
  const storage = {
    setItem(): void {
      throw new Error('quota exceeded');
    },
  };
  assert.doesNotThrow(() => writeThemePreference(storage, 'dark'));
});

test('applyResolvedTheme applies the dark class, color scheme and meta', () => {
  const doc = createThemeDocument();

  applyResolvedTheme(doc, 'dark');

  assert.equal(doc.documentElement.classList.contains('dark'), true);
  assert.equal(doc.documentElement.style.colorScheme, 'dark');
  assert.equal(
    doc.querySelector('meta[name="theme-color"]')?.getAttribute('content'),
    THEME_COLOR_DARK,
  );
});

test('applyResolvedTheme removes the dark class and restores the light meta', () => {
  const doc = createThemeDocument();

  applyResolvedTheme(doc, 'dark');
  applyResolvedTheme(doc, 'light');

  assert.equal(doc.documentElement.classList.contains('dark'), false);
  assert.equal(doc.documentElement.style.colorScheme, 'light');
  assert.equal(
    doc.querySelector('meta[name="theme-color"]')?.getAttribute('content'),
    THEME_COLOR_LIGHT,
  );
});

test('applyResolvedTheme does not throw without a theme-color meta', () => {
  const doc = createThemeDocument('');

  assert.doesNotThrow(() => applyResolvedTheme(doc, 'dark'));
  assert.equal(doc.documentElement.classList.contains('dark'), true);
  assert.equal(doc.querySelector('meta[name="theme-color"]'), null);
});

test('boot script in index.html stays in sync with the theme module', () => {
  const html = readFileSync(
    fileURLToPath(new URL('../index.html', import.meta.url)),
    'utf8',
  );

  assert.ok(html.includes(THEME_STORAGE_KEY), 'storage key');
  assert.ok(html.includes(THEME_COLOR_DARK), 'dark theme-color');
  assert.ok(html.includes(THEME_COLOR_LIGHT), 'light theme-color');
  assert.ok(html.includes('prefers-color-scheme: dark'), 'system query');
});
