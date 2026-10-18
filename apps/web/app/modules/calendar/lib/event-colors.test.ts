// Unit tests for the Notion CIECAM02 event-color derivation. No React / DOM.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  isEventPast,
  notionEventCssVars,
  notionEventTheme,
} from './event-colors';

const TONE_FIELDS = [
  'backgroundPrimary',
  'backgroundPrimaryDimmed',
  'backgroundSecondary',
  'backgroundSecondaryDimmed',
  'foregroundPrimary',
  'foregroundPrimaryDimmed',
  'foregroundSecondary',
  'foregroundSecondaryDimmed',
] as const;

const RGB_RE = /^rgb\((\d{1,3}), (\d{1,3}), (\d{1,3})\)$/;

function channels(css: string): [number, number, number] {
  const match = RGB_RE.exec(css);
  assert.ok(match, `not an rgb() string: ${css}`);
  return [Number(match[1]), Number(match[2]), Number(match[3])];
}

/** Each channel within ±1 of the expected integer, and no NaN. */
function assertRgb(actual: string, expected: string, label: string): void {
  assert.ok(!actual.includes('NaN'), `${label}: NaN in ${actual}`);
  const a = channels(actual);
  const e = channels(expected);
  for (let i = 0; i < 3; i++) {
    assert.ok(
      Math.abs(a[i] - e[i]) <= 1,
      `${label}: ${actual} not within ±1 of ${expected}`,
    );
  }
}

function assertCssColor(css: string, label: string): void {
  assert.ok(!css.includes('NaN'), `${label}: NaN in ${css}`);
  assert.match(css, RGB_RE, `${label}: ${css}`);
  for (const channel of channels(css)) {
    assert.ok(
      channel >= 0 && channel <= 255,
      `${label}: channel ${channel} out of range in ${css}`,
    );
  }
}

// ── Measured Notion pixels ──────────────────────────────────────────────

test('notionEventTheme: Notion red dark matches measured pixels', () => {
  const { dark } = notionEventTheme('#ED201D');
  assertRgb(
    dark.backgroundSecondary,
    'rgb(108, 41, 32)',
    'backgroundSecondary',
  );
  assertRgb(dark.backgroundPrimary, 'rgb(242, 36, 32)', 'backgroundPrimary');
  assertRgb(
    dark.backgroundPrimaryDimmed,
    'rgb(134, 47, 36)',
    'backgroundPrimaryDimmed',
  );
  assertRgb(dark.foregroundPrimary, 'rgb(255, 231, 217)', 'foregroundPrimary');
  assertRgb(
    dark.foregroundPrimaryDimmed,
    'rgb(157, 123, 117)',
    'foregroundPrimaryDimmed',
  );
  assertRgb(
    dark.foregroundSecondary,
    'rgb(212, 132, 119)',
    'foregroundSecondary',
  );
});

test('notionEventTheme: Notion yellow dark matches measured pixels', () => {
  const { dark } = notionEventTheme('#F4BE40');
  assertRgb(dark.backgroundSecondary, 'rgb(79, 63, 27)', 'backgroundSecondary');
  assertRgb(dark.foregroundPrimary, 'rgb(255, 250, 179)', 'foregroundPrimary');
  assertRgb(
    dark.foregroundSecondary,
    'rgb(184, 151, 79)',
    'foregroundSecondary',
  );
});

test('notionEventTheme: dimmed secondary is the secondary fill', () => {
  const theme = notionEventTheme('#ED201D');
  assert.equal(
    theme.light.backgroundSecondaryDimmed,
    theme.light.backgroundSecondary,
  );
  assert.equal(
    theme.dark.backgroundSecondaryDimmed,
    theme.dark.backgroundSecondary,
  );
});

test('notionEventTheme: accentInk is one of the two ink colors', () => {
  const theme = notionEventTheme('#ED201D');
  for (const tone of [theme.light, theme.dark]) {
    assert.ok(['#fafafa', '#1a1a1a'].includes(tone.accentInk));
  }
});

test('notionEventTheme: hex casing and short form hit the same cache entry', () => {
  assert.equal(notionEventTheme('#ed201d'), notionEventTheme('#ED201D'));
  assert.equal(notionEventTheme('#abc'), notionEventTheme('#aabbcc'));
});

// ── CSS vars ────────────────────────────────────────────────────────────

test('notionEventCssVars: every var maps to its tone field', () => {
  const vars = notionEventCssVars('#ED201D');
  const { light, dark } = notionEventTheme('#ED201D');
  assert.equal(vars['--ev-fill'], light.backgroundSecondary);
  assert.equal(vars['--ev-fill-dark'], dark.backgroundSecondary);
  assert.equal(vars['--ev-accent'], light.backgroundPrimary);
  assert.equal(vars['--ev-accent-dark'], dark.backgroundPrimary);
  assert.equal(vars['--ev-accent-dim'], light.backgroundPrimaryDimmed);
  assert.equal(vars['--ev-accent-dim-dark'], dark.backgroundPrimaryDimmed);
  assert.equal(vars['--ev-title'], light.foregroundPrimary);
  assert.equal(vars['--ev-title-dark'], dark.foregroundPrimary);
  assert.equal(vars['--ev-title-dim'], light.foregroundPrimaryDimmed);
  assert.equal(vars['--ev-title-dim-dark'], dark.foregroundPrimaryDimmed);
  assert.equal(vars['--ev-time'], light.foregroundSecondary);
  assert.equal(vars['--ev-time-dark'], dark.foregroundSecondary);
  assert.equal(vars['--ev-time-dim'], light.foregroundSecondaryDimmed);
  assert.equal(vars['--ev-time-dim-dark'], dark.foregroundSecondaryDimmed);
  assert.equal(vars['--ev-ink'], light.accentInk);
  assert.equal(vars['--ev-ink-dark'], dark.accentInk);
});

test('notionEventCssVars: same hex returns the same object', () => {
  assert.equal(notionEventCssVars('#ed201d'), notionEventCssVars('#ED201D'));
});

// ── Formatting across real data and edge inputs ────────────────────────

const FORMAT_HEXES = [
  '#616161',
  '#039be5',
  '#7cb342',
  '#d50000',
  '#e4c441',
  '#e67c73',
  '#c45a2c',
  '#009688',
  '#795548',
  '#ef6c00',
  '#4a5c8a',
  '#f4511e',
  '#7986cb',
  '#7a4a6a',
  '#8a6a2c',
  '#000000',
  '#ffffff',
  '#808080',
  '#b8b8b8',
  'not-a-hex',
  '',
  '#abc',
  '#gg0000',
];

test('notionEventTheme: both themes stay valid rgb() for every input', () => {
  for (const hex of FORMAT_HEXES) {
    const theme = notionEventTheme(hex);
    for (const mode of ['light', 'dark'] as const) {
      const tone = theme[mode];
      for (const field of TONE_FIELDS) {
        assertCssColor(tone[field], `${hex} ${mode} ${field}`);
      }
      assert.ok(['#fafafa', '#1a1a1a'].includes(tone.accentInk));
    }
  }
});

test('notionEventTheme: the passed hex is not remapped onto a palette', () => {
  assert.notEqual(notionEventTheme('#d50000'), notionEventTheme('#ed201d'));
  assert.notEqual(
    notionEventTheme('#d50000').dark.backgroundSecondary,
    'rgb(108, 41, 32)',
  );
  assert.notEqual(notionEventTheme('#616161'), notionEventTheme('#b8b8b8'));
});

// ── isEventPast ─────────────────────────────────────────────────────────

test('isEventPast: ends exactly now is past, one ms later is not', () => {
  assert.equal(isEventPast(1_000, 1_000), true);
  assert.equal(isEventPast(1_001, 1_000), false);
  assert.equal(isEventPast(999, 1_000), true);
});

test('isEventPast: non-finite instants are never past', () => {
  assert.equal(isEventPast(NaN, 1_000), false);
  assert.equal(isEventPast(1_000, NaN), false);
  assert.equal(isEventPast(Infinity, 1_000), false);
  assert.equal(isEventPast(1_000, Infinity), false);
});
