// Unit tests for the late-hours nav fade helper. No React / DOM.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { shouldHideNavForLateHours } from './nav-late-hours';
import { COL_HEADER_H } from './week-layout';

const hourH = 60;
const allDayHeight = 20;
const clientHeight = 600;
// 11 PM row top in content coords.
const rowTop = COL_HEADER_H + allDayHeight + 23 * hourH;
const base = { clientHeight, hourH, allDayHeight, hysteresis: 8 };

test('visible nav while 11 PM is below the viewport', () => {
  assert.equal(shouldHideNavForLateHours({ ...base, scrollTop: 0 }), false);
});

test('hides once 11 PM row enters the viewport', () => {
  const scrollTop = rowTop - clientHeight + 20;
  assert.equal(shouldHideNavForLateHours({ ...base, scrollTop }), true);
});

test('hysteresis: boundary scroll keeps the previous state', () => {
  const edge = rowTop - clientHeight; // row top exactly at viewport bottom
  assert.equal(
    shouldHideNavForLateHours({
      ...base,
      scrollTop: edge,
      currentlyHidden: false,
    }),
    false,
  );
  assert.equal(
    shouldHideNavForLateHours({
      ...base,
      scrollTop: edge,
      currentlyHidden: true,
    }),
    true,
  );
  // Far enough out, hidden releases.
  assert.equal(
    shouldHideNavForLateHours({
      ...base,
      scrollTop: edge - 9,
      currentlyHidden: true,
    }),
    false,
  );
});

test('zoom: larger hourH pushes 11 PM further down', () => {
  const scrollTop = rowTop - clientHeight + 20;
  assert.equal(
    shouldHideNavForLateHours({ ...base, scrollTop, hourH: 80 }),
    false,
  );
});

test('all-day height shifts the row', () => {
  const scrollTop = rowTop - clientHeight + 20;
  assert.equal(
    shouldHideNavForLateHours({ ...base, scrollTop, allDayHeight: 60 }),
    false,
  );
});

test('row hidden under the sticky band does not count', () => {
  const scrollTop = rowTop + hourH + 100; // scrolled past (short content edge case)
  assert.equal(shouldHideNavForLateHours({ ...base, scrollTop }), false);
});

test('unmeasured viewport never hides', () => {
  assert.equal(
    shouldHideNavForLateHours({ ...base, scrollTop: 5000, clientHeight: 0 }),
    false,
  );
});
