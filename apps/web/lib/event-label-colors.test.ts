import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  DEFAULT_EVENT_LABEL_COLOR,
  EVENT_LABEL_COLORS,
  isEventLabelHex,
} from './event-label-colors';

test('palette is 24 distinct lowercase #rrggbb hexes', () => {
  assert.equal(EVENT_LABEL_COLORS.length, 24);
  assert.equal(new Set(EVENT_LABEL_COLORS).size, 24, 'no duplicates');
  for (const hex of EVENT_LABEL_COLORS) {
    assert.match(hex, /^#[0-9a-f]{6}$/, `${hex} is lowercase #rrggbb`);
  }
});

test('default color is peacock and a palette member', () => {
  assert.equal(DEFAULT_EVENT_LABEL_COLOR, '#039be5');
  assert.ok(isEventLabelHex(DEFAULT_EVENT_LABEL_COLOR));
});

test('every palette hex is a member', () => {
  for (const hex of EVENT_LABEL_COLORS) {
    assert.ok(isEventLabelHex(hex), hex);
  }
});

test('membership trims and folds case', () => {
  assert.ok(isEventLabelHex('#039BE5'));
  assert.ok(isEventLabelHex('  #039be5  '));
  assert.ok(isEventLabelHex('\t#4285F4\n'));
});

test('non-palette hexes are not members (no shorthand expansion)', () => {
  // Parseable hexes outside the 24 — including the old seed hex.
  for (const hex of ['#535050', '#2a5c8a', '#3a3a3a', '#abc', '#000000']) {
    assert.equal(isEventLabelHex(hex), false, hex);
  }
});

test('non-hex strings are not members', () => {
  for (const hex of ['blue', '', '#', '#gg0000', '2a5c8a', '#12345']) {
    assert.equal(isEventLabelHex(hex), false, JSON.stringify(hex));
  }
});
