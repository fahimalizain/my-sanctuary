// The strip's landing window is a pure function of the local civil day and the
// period length. Every fixture is built with local Date constructors and read
// back in the browser's zone — the machine is not UTC, so a `Z` literal would
// assert the wrong day.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  LOCAL_MIDNIGHT_SKEW_MS,
  initialStripRange,
  initialStripWindowStart,
  msUntilLocalMidnight,
} from './initial-strip-range';
import {
  STRIP_OVERSCAN,
  addDays,
  rangeIso,
  stripDayCount,
} from './week-layout';

const MINUTE = 60 * 1000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** Saturday 2026-10-17, 15:30 local — the instant every window below lands on. */
const saturday = new Date(2026, 9, 17, 15, 30, 0);

function assertIsLocalMidnight(iso: string): void {
  const d = new Date(iso);
  assert.deepEqual(
    [d.getHours(), d.getMinutes(), d.getSeconds(), d.getMilliseconds()],
    [0, 0, 0, 0],
    `${iso} must be local midnight`,
  );
}

// ── Local midnight clock ────────────────────────────────────────────────

test('msUntilLocalMidnight: counts to the next local midnight', () => {
  assert.equal(
    msUntilLocalMidnight(new Date(2026, 9, 17, 23, 30, 0)),
    30 * MINUTE,
  );
  // Exactly midnight is a full day, never 0: a timer must wait for the
  // following midnight instead of refiring on the one it just passed.
  assert.equal(msUntilLocalMidnight(new Date(2026, 9, 17, 0, 0, 0, 0)), DAY);
  assert.equal(
    msUntilLocalMidnight(new Date(2026, 9, 17, 20, 59, 59, 0)),
    3 * HOUR + 1000,
  );
});

test('msUntilLocalMidnight: the skew lands past the boundary, never on it', () => {
  const now = new Date(2026, 9, 17, 23, 59, 59, 999);
  const midnight = new Date(2026, 9, 18, 0, 0, 0, 0);
  const fired =
    now.getTime() + msUntilLocalMidnight(now) + LOCAL_MIDNIGHT_SKEW_MS;

  // A timer 1ms early would recompute yesterday's window and then wait another
  // day, so the fire has to clear the new midnight — inside the skew window.
  assert.ok(fired > midnight.getTime(), 'fires strictly after the midnight');
  assert.ok(
    fired <= midnight.getTime() + LOCAL_MIDNIGHT_SKEW_MS,
    'and no later than the skew allows',
  );
  assert.deepEqual(new Date(fired), new Date(2026, 9, 18, 0, 0, 1, 0));
});

// ── Landing window ──────────────────────────────────────────────────────

test('initialStripWindowStart: fitted period start, one overscan earlier', () => {
  // Period 7 Monday-snaps: Sat Oct 17 → Mon Oct 12, then a week of overscan.
  assert.deepEqual(
    initialStripWindowStart(saturday, 7),
    addDays(new Date(2026, 9, 12), -STRIP_OVERSCAN),
  );
  // Period 1 does not snap: the civil day itself is the left edge.
  assert.deepEqual(
    initialStripWindowStart(saturday, 1),
    addDays(new Date(2026, 9, 17), -STRIP_OVERSCAN),
  );
  // Non-finite falls back to 7 (clampPeriodLength).
  assert.deepEqual(
    initialStripWindowStart(saturday, Number.NaN),
    addDays(new Date(2026, 9, 12), -STRIP_OVERSCAN),
  );
});

test('initialStripRange: period 7 is a Monday-snapped 21-day window', () => {
  const range = initialStripRange(saturday, 7);
  assertIsLocalMidnight(range.timeMin);
  assertIsLocalMidnight(range.timeMax);
  // Mon Oct 12 (week of the Saturday) minus STRIP_OVERSCAN 7 → Oct 5.
  assert.deepEqual(new Date(range.timeMin), new Date(2026, 9, 5));
  // Plus stripDayCount(7) = 7 + 14 = 21 days → Oct 26.
  assert.deepEqual(new Date(range.timeMax), new Date(2026, 9, 26));
});

test('initialStripRange: period 5 keeps the Monday snap, 19 days wide', () => {
  const range = initialStripRange(saturday, 5);
  assertIsLocalMidnight(range.timeMin);
  assert.deepEqual(new Date(range.timeMin), new Date(2026, 9, 5));
  assert.deepEqual(new Date(range.timeMax), new Date(2026, 9, 24));
});

test('initialStripRange: period 1 is day-anchored, 15 days wide', () => {
  const range = initialStripRange(saturday, 1);
  assertIsLocalMidnight(range.timeMin);
  assertIsLocalMidnight(range.timeMax);
  // Sat Oct 17 minus STRIP_OVERSCAN 7 → Oct 10, no Monday snap.
  assert.deepEqual(new Date(range.timeMin), new Date(2026, 9, 10));
  // Plus stripDayCount(1) = 1 + 14 = 15 days → Oct 25.
  assert.deepEqual(new Date(range.timeMax), new Date(2026, 9, 25));
});

test('initialStripRange: periods 3 and 4 share the day-anchored start', () => {
  for (const n of [3, 4]) {
    const range = initialStripRange(saturday, n);
    assertIsLocalMidnight(range.timeMin);
    assert.deepEqual(new Date(range.timeMin), new Date(2026, 9, 10), `${n}`);
    assertIsLocalMidnight(range.timeMax);
  }
});

test('initialStripRange: a junk period length clamps to 1 or 7', () => {
  assert.deepEqual(
    initialStripRange(saturday, 0),
    initialStripRange(saturday, 1),
  );
  assert.deepEqual(
    initialStripRange(saturday, 99),
    initialStripRange(saturday, 7),
  );
  assert.deepEqual(
    initialStripRange(saturday, Number.NaN),
    initialStripRange(saturday, 7),
  );
});

test('initialStripRange: is exactly the strip mount composition for 1…7', () => {
  for (let n = 1; n <= 7; n += 1) {
    // The composition lock: the strip builds its first key as
    // rangeIso(windowStart, stripDayCount(periodLength)), so background sync
    // must produce the identical pair. The civil-date cases above are what
    // keep this from being a tautology.
    assert.deepEqual(
      initialStripRange(saturday, n),
      rangeIso(initialStripWindowStart(saturday, n), stripDayCount(n)),
      `period ${n}`,
    );
  }
});
