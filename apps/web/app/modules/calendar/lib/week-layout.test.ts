// Unit tests for the week time-grid pure helpers (date math, geometry,
// overlap packing). No React / DOM.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  ALLDAY_MAX,
  ALLDAY_MIN,
  CHIP_MIN_H,
  DAYS_PER_PERIOD,
  EMPTY_WHEEL_BURST,
  HOUR_H_BASE,
  HOUR_H_MAX,
  HOUR_H_MIN,
  SCROLL_AXIS_SLOP,
  STRIP_OVERSCAN,
  STRIP_REBASE_THRESHOLD,
  TIME_GUTTER_W,
  WHEEL_BURST_IDLE_MS,
  WHEEL_LINE_PX,
  allDaySectionHeight,
  clampMinutesToDay,
  clampPeriodLength,
  classifyScrollAxis,
  nextWheelBurst,
  parseStoredPeriodLength,
  wheelDeltaPx,
  colWidth,
  dayIndexFromScroll,
  eventHeightPx,
  eventTopPx,
  fitPeriodStart,
  formatDayRangeTitle,
  applyStartDate,
  applyEndTime,
  applyStartTime,
  formatEventDateLine,
  formatEventDuration,
  formatEventTimeDetail,
  formatWeekTitle,
  toDateInputValue,
  toTimeInputValue,
  gutterWithRemainder,
  contrastingInk,
  hexLuminance,
  hexToRgba,
  hourGridBackground,
  hourHeight,
  isCompactChip,
  isMultiDay,
  isStripWidthMeasured,
  monthGridDays,
  monthGridStart,
  nowLineY,
  packAllDayLanes,
  packDayEvents,
  periodLabel,
  rangeIso,
  scrollLeftForIndex,
  shiftWindowStart,
  shouldRebase,
  startOfWeek,
  stripDayCount,
  stripRebaseSuppressed,
  stripSnapDisabled,
  visibleStartIndex,
  weekRangeIso,
  snapMinutes,
  PACK_WIDEN_BELOW_MIN,
  RESIZE_SNAP_MINUTES,
  SNAP_MINUTES,
  minutesFromY,
  dateOnDay,
  defaultWritableCalendar,
  civilDateFromAllDayIso,
  timedRangeToAllDay,
  allDayRangeToTimed,
  applyAllDayStartDate,
  type StripLockReason,
  type WheelBurst,
} from './week-layout';

// ── startOfWeek ─────────────────────────────────────────────────────────

test('startOfWeek: Wednesday lands on the preceding Monday at 00:00 local', () => {
  // 2026-09-09 is a Wednesday.
  const wed = new Date(2026, 8, 9, 15, 30, 45, 123);
  const mon = startOfWeek(wed);
  assert.equal(mon.getFullYear(), 2026);
  assert.equal(mon.getMonth(), 8);
  assert.equal(mon.getDate(), 7); // Mon Sep 7
  assert.equal(mon.getHours(), 0);
  assert.equal(mon.getMinutes(), 0);
  assert.equal(mon.getSeconds(), 0);
  assert.equal(mon.getMilliseconds(), 0);
});

test('startOfWeek: Sunday lands on the preceding Monday at 00:00 local', () => {
  // 2026-09-13 is a Sunday.
  const sun = new Date(2026, 8, 13, 22, 0, 0, 0);
  const mon = startOfWeek(sun);
  assert.equal(mon.getFullYear(), 2026);
  assert.equal(mon.getMonth(), 8);
  assert.equal(mon.getDate(), 7); // Mon Sep 7
  assert.equal(mon.getHours(), 0);
  assert.equal(mon.getMinutes(), 0);
  assert.equal(mon.getSeconds(), 0);
  assert.equal(mon.getMilliseconds(), 0);
});

test('startOfWeek: Monday stays on itself at midnight', () => {
  const mon = new Date(2026, 8, 7, 9, 0, 0, 0);
  const start = startOfWeek(mon);
  assert.equal(start.getDate(), 7);
  assert.equal(start.getHours(), 0);
});

// ── weekRangeIso ────────────────────────────────────────────────────────

test('weekRangeIso: 7-day window; timeMax is next Monday midnight', () => {
  const weekStart = new Date(2026, 8, 7); // Mon Sep 7 local
  const { timeMin, timeMax } = weekRangeIso(weekStart);

  const min = new Date(timeMin);
  const max = new Date(timeMax);

  // Round-trip: local midnights of Mon Sep 7 and Mon Sep 14.
  const expectedMin = new Date(2026, 8, 7, 0, 0, 0, 0);
  const expectedMax = new Date(2026, 8, 14, 0, 0, 0, 0);
  assert.equal(min.getTime(), expectedMin.getTime());
  assert.equal(max.getTime(), expectedMax.getTime());

  // Exactly 7 days apart.
  assert.equal(max.getTime() - min.getTime(), 7 * 24 * 60 * 60 * 1000);
});

// ── formatWeekTitle ─────────────────────────────────────────────────────

test('formatWeekTitle: same-month range', () => {
  // Mon Sep 7 – Sun Sep 13, 2026
  assert.equal(formatWeekTitle(new Date(2026, 8, 7)), 'Sep 7–13, 2026');
});

test('formatWeekTitle: cross-month range', () => {
  // Mon Sep 28 – Sun Oct 4, 2026
  assert.equal(formatWeekTitle(new Date(2026, 8, 28)), 'Sep 28 – Oct 4, 2026');
});

test('formatDayRangeTitle: Wed–Tue cross-month', () => {
  // Wed Sep 30 – Tue Oct 6, 2026
  assert.equal(
    formatDayRangeTitle(new Date(2026, 8, 30), new Date(2026, 9, 6)),
    'Sep 30 – Oct 6, 2026',
  );
});

test('formatDayRangeTitle: cross-year', () => {
  assert.equal(
    formatDayRangeTitle(new Date(2025, 11, 29), new Date(2026, 0, 4)),
    'Dec 29, 2025 – Jan 4, 2026',
  );
});

test('formatDayRangeTitle: single day includes weekday', () => {
  // Tue Sep 8, 2026
  assert.equal(
    formatDayRangeTitle(new Date(2026, 8, 8), new Date(2026, 8, 8)),
    'Tue, Sep 8, 2026',
  );
});

// ── rangeIso ────────────────────────────────────────────────────────────

test('rangeIso: N-day window; timeMax is start+N midnights', () => {
  const start = new Date(2026, 8, 7); // Mon Sep 7 local
  const { timeMin, timeMax } = rangeIso(start, 21);

  const min = new Date(timeMin);
  const max = new Date(timeMax);
  const expectedMin = new Date(2026, 8, 7, 0, 0, 0, 0);
  const expectedMax = new Date(2026, 8, 28, 0, 0, 0, 0); // +21 days
  assert.equal(min.getTime(), expectedMin.getTime());
  assert.equal(max.getTime(), expectedMax.getTime());
  assert.equal(max.getTime() - min.getTime(), 21 * 24 * 60 * 60 * 1000);
});

// ── Period length ───────────────────────────────────────────────────────

test('clampPeriodLength: 0→1, 3→3, 9→7, NaN→7', () => {
  assert.equal(clampPeriodLength(0), 1);
  assert.equal(clampPeriodLength(3), 3);
  assert.equal(clampPeriodLength(9), 7);
  assert.equal(clampPeriodLength(NaN), 7);
  assert.equal(clampPeriodLength(Infinity), 7);
});

test('parseStoredPeriodLength: null / empty / non-numeric → null', () => {
  assert.equal(parseStoredPeriodLength(null), null);
  assert.equal(parseStoredPeriodLength(''), null);
  assert.equal(parseStoredPeriodLength('nope'), null);
});

test('parseStoredPeriodLength: valid number is clamped', () => {
  assert.equal(parseStoredPeriodLength('1'), 1);
  assert.equal(parseStoredPeriodLength('3'), 3);
  assert.equal(parseStoredPeriodLength('7'), 7);
  assert.equal(parseStoredPeriodLength('0'), 1);
  assert.equal(parseStoredPeriodLength('9'), 7);
});

test('periodLabel: 1 Day, 3 "3 days", 7 Week', () => {
  assert.equal(periodLabel(1), 'Day');
  assert.equal(periodLabel(3), '3 days');
  assert.equal(periodLabel(7), 'Week');
});

test('fitPeriodStart: short period keeps civil day; long snaps to Monday', () => {
  // 2026-09-09 is a Wednesday.
  const wed = new Date(2026, 8, 9, 15, 30, 0, 0);
  const short = fitPeriodStart(wed, 3);
  assert.equal(short.getFullYear(), 2026);
  assert.equal(short.getMonth(), 8);
  assert.equal(short.getDate(), 9); // that Wednesday
  assert.equal(short.getHours(), 0);
  assert.equal(short.getMinutes(), 0);
  assert.equal(short.getSeconds(), 0);
  assert.equal(short.getMilliseconds(), 0);

  const week = fitPeriodStart(wed, 7);
  assert.equal(week.getFullYear(), 2026);
  assert.equal(week.getMonth(), 8);
  assert.equal(week.getDate(), 7); // preceding Monday
  assert.equal(week.getHours(), 0);
});

// ── Infinite strip geometry ─────────────────────────────────────────────

// Regression gate for first-visit strip park (Aug 31–Sep 6 bug):
// mainWidth starts at 0; colWidth(0) returns the 16px fallback. Init park must
// wait for a real measured width (mainWidth > 0), not colW > 0.
test('isStripWidthMeasured: only finite positive widths count as measured', () => {
  assert.equal(isStripWidthMeasured(0), false);
  assert.equal(isStripWidthMeasured(-1), false);
  assert.equal(isStripWidthMeasured(NaN), false);
  assert.equal(isStripWidthMeasured(Infinity), false);
  assert.equal(isStripWidthMeasured(-Infinity), false);
  assert.equal(isStripWidthMeasured(1), true);
  assert.equal(isStripWidthMeasured(800), true);
  // Document why colW is the wrong gate: unmeasured → 16, which is > 0.
  assert.equal(colWidth(0), 16);
  assert.ok(colWidth(0) > 0);
  assert.equal(isStripWidthMeasured(0), false);
});

test('colWidth / gutterWithRemainder: available 800 → 7 cols + gutter = 800', () => {
  const available = 800;
  const colW = colWidth(available);
  const gutter = gutterWithRemainder(available, colW);
  assert.equal(colW, Math.floor((800 - TIME_GUTTER_W) / 7));
  assert.equal(gutter + colW * DAYS_PER_PERIOD, available);
  assert.ok(colW >= 16);
  assert.ok(gutter >= TIME_GUTTER_W);
});

test('colWidth / gutterWithRemainder: period 3 → gutter + colW * 3 = 800', () => {
  const available = 800;
  const colW = colWidth(available, 3);
  const gutter = gutterWithRemainder(available, colW, 3);
  assert.equal(colW, Math.floor((800 - TIME_GUTTER_W) / 3));
  assert.equal(gutter + colW * 3, available);
});

test('colWidth: never below 16', () => {
  assert.equal(colWidth(0), 16);
  assert.equal(colWidth(10), 16);
});

test('stripDayCount: period + overscan each side = 21', () => {
  assert.equal(stripDayCount(), DAYS_PER_PERIOD + STRIP_OVERSCAN * 2);
  assert.equal(stripDayCount(), 21);
});

test('stripDayCount: period 3 → 3 + 14 = 17', () => {
  assert.equal(stripDayCount(3), 3 + STRIP_OVERSCAN * 2);
  assert.equal(stripDayCount(3), 17);
});

test('dayIndexFromScroll / scrollLeftForIndex round-trip', () => {
  const colW = 100;
  for (const idx of [0, 3, 7, 14]) {
    const left = scrollLeftForIndex(idx, colW);
    assert.equal(left, idx * colW);
    assert.equal(dayIndexFromScroll(left, colW), idx);
    // Mid-column still floors to the same index.
    assert.equal(dayIndexFromScroll(left + colW / 2 - 1, colW), idx);
  }
});

test('visibleStartIndex: rounds and clamps to a full period', () => {
  const colW = 100;
  const dayCount = 21;
  assert.equal(visibleStartIndex(0, colW, dayCount), 0);
  assert.equal(visibleStartIndex(7 * colW, colW, dayCount), 7);
  // Halfway past a column snaps forward.
  assert.equal(visibleStartIndex(7 * colW + 50, colW, dayCount), 8);
  // Past the last full period clamps.
  assert.equal(
    visibleStartIndex(100 * colW, colW, dayCount),
    dayCount - DAYS_PER_PERIOD,
  );
});

test('shouldRebase: middle 0, near left -1, near right +1', () => {
  const dayCount = stripDayCount(); // 21
  // Visible start in the safe band → 0
  assert.equal(shouldRebase(STRIP_REBASE_THRESHOLD, dayCount), 0);
  assert.equal(shouldRebase(7, dayCount), 0);
  assert.equal(
    shouldRebase(dayCount - DAYS_PER_PERIOD - STRIP_REBASE_THRESHOLD, dayCount),
    0,
  );
  // Near left edge
  assert.equal(shouldRebase(0, dayCount), -1);
  assert.equal(shouldRebase(STRIP_REBASE_THRESHOLD - 1, dayCount), -1);
  // Near right edge: max start = 21-7=14; threshold when > 14-3=11
  assert.equal(shouldRebase(12, dayCount), 1);
  assert.equal(shouldRebase(14, dayCount), 1);
});

test('shouldRebase: period 3 — rebase near edges using period 3', () => {
  const period = 3;
  const dayCount = stripDayCount(period); // 17
  // Safe band: visibleStart >= 3 and <= 17-3-3 = 11
  assert.equal(shouldRebase(STRIP_REBASE_THRESHOLD, dayCount, period), 0);
  assert.equal(shouldRebase(7, dayCount, period), 0);
  assert.equal(
    shouldRebase(dayCount - period - STRIP_REBASE_THRESHOLD, dayCount, period),
    0,
  );
  // Near left
  assert.equal(shouldRebase(0, dayCount, period), -1);
  assert.equal(shouldRebase(STRIP_REBASE_THRESHOLD - 1, dayCount, period), -1);
  // Near right: max start = 17-3=14; threshold when > 14-3=11
  assert.equal(shouldRebase(12, dayCount, period), 1);
  assert.equal(shouldRebase(14, dayCount, period), 1);
});

test('shiftWindowStart: ±7 days', () => {
  const origin = new Date(2026, 8, 7); // Mon Sep 7
  const left = shiftWindowStart(origin, -1);
  const right = shiftWindowStart(origin, 1);
  assert.equal(left.getFullYear(), 2026);
  assert.equal(left.getMonth(), 7); // August
  assert.equal(left.getDate(), 31);
  assert.equal(right.getFullYear(), 2026);
  assert.equal(right.getMonth(), 8);
  assert.equal(right.getDate(), 14);
});

test('shiftWindowStart: period 3 → ±3 days', () => {
  const origin = new Date(2026, 8, 7); // Mon Sep 7
  const left = shiftWindowStart(origin, -1, 3);
  const right = shiftWindowStart(origin, 1, 3);
  assert.equal(left.getFullYear(), 2026);
  assert.equal(left.getMonth(), 8);
  assert.equal(left.getDate(), 4); // Fri Sep 4
  assert.equal(right.getFullYear(), 2026);
  assert.equal(right.getMonth(), 8);
  assert.equal(right.getDate(), 10); // Thu Sep 10
});

// ── Strip lock ownership ────────────────────────────────────────────────

test('stripSnapDisabled: true while any owner is held', () => {
  assert.equal(stripSnapDisabled(new Set<StripLockReason>()), false);
  for (const reason of ['drag', 'pinch', 'axis'] as const) {
    assert.equal(stripSnapDisabled(new Set<StripLockReason>([reason])), true);
  }
  assert.equal(
    stripSnapDisabled(new Set<StripLockReason>(['drag', 'pinch'])),
    true,
  );
});

test('stripRebaseSuppressed: drag / pinch yes, axis alone no', () => {
  assert.equal(stripRebaseSuppressed(new Set<StripLockReason>(['drag'])), true);
  assert.equal(
    stripRebaseSuppressed(new Set<StripLockReason>(['pinch'])),
    true,
  );
  assert.equal(
    stripRebaseSuppressed(new Set<StripLockReason>(['drag', 'pinch'])),
    true,
  );
  // axis holds snap off but leaves rebase free (the window may still slide).
  assert.equal(
    stripRebaseSuppressed(new Set<StripLockReason>(['axis'])),
    false,
  );
  assert.equal(
    stripRebaseSuppressed(new Set<StripLockReason>(['drag', 'axis'])),
    true,
  );
});

test('strip lock: releasing one owner leaves another owner in effect', () => {
  const held = new Set<StripLockReason>(['drag', 'axis']);
  assert.equal(stripSnapDisabled(held), true);
  assert.equal(stripRebaseSuppressed(held), true);

  // Drag ends while the axis owner still holds.
  held.delete('drag');
  assert.equal(stripSnapDisabled(held), true);
  assert.equal(stripRebaseSuppressed(held), false);

  // Last owner releases → snap re-engages.
  held.delete('axis');
  assert.equal(stripSnapDisabled(held), false);
  assert.equal(stripRebaseSuppressed(held), false);
});

// Set, not a counter: lock twice + unlock once must release.
test('strip lock: repeat holds are idempotent', () => {
  const held = new Set<StripLockReason>();
  held.add('pinch');
  held.add('pinch');
  held.delete('pinch');
  assert.equal(stripSnapDisabled(held), false);
  assert.equal(stripRebaseSuppressed(held), false);
});

test('strip lock: empty set means snap enabled and no suppression', () => {
  const held: ReadonlySet<StripLockReason> = new Set();
  assert.equal(stripSnapDisabled(held), false);
  assert.equal(stripRebaseSuppressed(held), false);
});

// ── classifyScrollAxis ──────────────────────────────────────────────────

test('classifyScrollAxis: pending until the slop is exceeded', () => {
  assert.equal(classifyScrollAxis(0, 0), 'pending');
  assert.equal(classifyScrollAxis(4, -3), 'pending');
  // Exactly at the slop is still pending: the gesture has not moved enough.
  assert.equal(classifyScrollAxis(SCROLL_AXIS_SLOP, 0), 'pending');
  assert.equal(classifyScrollAxis(0, SCROLL_AXIS_SLOP), 'pending');
  assert.equal(
    classifyScrollAxis(SCROLL_AXIS_SLOP, SCROLL_AXIS_SLOP),
    'pending',
  );
});

test('classifyScrollAxis: pure vertical commits to y', () => {
  assert.equal(classifyScrollAxis(0, 24), 'y');
  assert.equal(classifyScrollAxis(0, -24), 'y');
});

test('classifyScrollAxis: diagonal 1:1 commits to y', () => {
  assert.equal(classifyScrollAxis(12, 12), 'y');
  assert.equal(classifyScrollAxis(40, 40), 'y');
});

test('classifyScrollAxis: just under 2:1 stays vertical, past it goes x', () => {
  assert.equal(classifyScrollAxis(19, 10), 'y');
  assert.equal(classifyScrollAxis(21, 10), 'x');
});

test('classifyScrollAxis: an exact 2:1 tie stays vertical', () => {
  // 2:1 is the boundary; `>` keeps the tie on the day-preserving side.
  assert.equal(classifyScrollAxis(20, 10), 'y');
  assert.equal(classifyScrollAxis(160, 80), 'y');
});

test('classifyScrollAxis: negative deltas are symmetric', () => {
  assert.equal(classifyScrollAxis(-21, -10), 'x');
  assert.equal(classifyScrollAxis(-19, -10), 'y');
  assert.equal(classifyScrollAxis(21, -10), 'x');
  assert.equal(classifyScrollAxis(-21, 10), 'x');
  assert.equal(classifyScrollAxis(-12, 12), 'y');
  assert.equal(classifyScrollAxis(0, -30), 'y');
});

test('classifyScrollAxis: custom slop', () => {
  // Smaller slop: below it still pending, above it classified as usual.
  assert.equal(classifyScrollAxis(2, 2, 2), 'pending');
  assert.equal(classifyScrollAxis(3, 3, 2), 'y');
  // Larger slop keeps a small move pending.
  assert.equal(classifyScrollAxis(3, 3, 8), 'pending');
  assert.equal(classifyScrollAxis(5, 2, 4), 'x');
  assert.equal(classifyScrollAxis(2, 8, 6), 'y');
});

// ── wheelDeltaPx ────────────────────────────────────────────────────────

test('wheelDeltaPx: pixel deltas pass through untouched', () => {
  assert.equal(wheelDeltaPx(12, 0, 600), 12);
  assert.equal(wheelDeltaPx(-8.5, 0, 600), -8.5);
  assert.equal(wheelDeltaPx(0, 0, 0), 0, 'px mode needs no viewport size');
});

test('wheelDeltaPx: line deltas scale by the 16px line box', () => {
  assert.equal(WHEEL_LINE_PX, 16, 'zoomFactorFromWheel assumes the same box');
  assert.equal(wheelDeltaPx(3, 1, 600), 48);
  assert.equal(
    wheelDeltaPx(-1, 1, 0),
    -WHEEL_LINE_PX,
    'lines need no page size',
  );
});

test('wheelDeltaPx: page deltas scale by the caller-measured viewport', () => {
  assert.equal(wheelDeltaPx(2, 2, 600), 1200);
  assert.equal(wheelDeltaPx(-1, 2, 480), -480);
  // A page delta on an unmeasured scroller must still move something: the
  // handler has already preventDefault-ed the event by then.
  assert.equal(wheelDeltaPx(1, 2, 0), WHEEL_LINE_PX);
  assert.equal(wheelDeltaPx(1, 2, Number.NaN), WHEEL_LINE_PX);
});

test('wheelDeltaPx: non-finite deltas never scroll', () => {
  assert.equal(wheelDeltaPx(Number.NaN, 0, 600), 0);
  assert.equal(wheelDeltaPx(Number.POSITIVE_INFINITY, 1, 600), 0);
  assert.equal(wheelDeltaPx(Number.NEGATIVE_INFINITY, 2, 600), 0);
});

// ── nextWheelBurst ──────────────────────────────────────────────────────

test('nextWheelBurst: an idle gap starts a new burst with fresh sums and axis', () => {
  const first = nextWheelBurst(EMPTY_WHEEL_BURST, { dx: 0, dy: 40, t: 1000 });
  assert.equal(first.axis, 'y');

  // Horizontal-heavy within the same burst: the latch must hold — re-deciding
  // here would hand the strip back to native snap mid-gesture.
  const latched = nextWheelBurst(first, { dx: 200, dy: 0, t: 1050 });
  assert.equal(latched.axis, 'y');
  assert.equal(latched.sumX, 200);
  assert.equal(latched.sumY, 40);

  // 200ms later: a new gesture, so this event's delta decides the axis.
  const restarted = nextWheelBurst(latched, { dx: 60, dy: 0, t: 1250 });
  assert.equal(restarted.axis, 'x');
  assert.equal(restarted.sumX, 60, 'sums restart with the burst');
  assert.equal(restarted.sumY, 0);
  assert.equal(restarted.lastT, 1250);
});

test('nextWheelBurst: stays pending while the burst is under the slop', () => {
  let burst: WheelBurst = EMPTY_WHEEL_BURST;
  for (let i = 0; i < 4; i++) {
    burst = nextWheelBurst(burst, { dx: 1, dy: 2, t: 1000 + i * 10 });
  }
  assert.equal(burst.axis, 'pending', '4× (1, 2) is still under 8px');
  assert.equal(burst.sumX, 4);
  assert.equal(burst.sumY, 8);

  // The 5th event crosses the slop on the accumulated delta and commits.
  burst = nextWheelBurst(burst, { dx: 1, dy: 2, t: 1050 });
  assert.equal(burst.axis, 'y');
  assert.equal(burst.sumX, 5);
  assert.equal(burst.sumY, 10);
});

test('nextWheelBurst: latches x on a clear horizontal start and keeps it', () => {
  const first = nextWheelBurst(EMPTY_WHEEL_BURST, { dx: -60, dy: 4, t: 0 });
  assert.equal(first.axis, 'x');

  // Vertical-heavy momentum after a horizontal flick must not flip the latch.
  const later = nextWheelBurst(first, { dx: -10, dy: -80, t: 16 });
  assert.equal(later.axis, 'x');
  assert.equal(later.sumY, -76);
});

test('nextWheelBurst: a gap over WHEEL_BURST_IDLE_MS is the only restart', () => {
  const first = nextWheelBurst(EMPTY_WHEEL_BURST, { dx: 0, dy: 40, t: 1000 });

  const justUnder = nextWheelBurst(first, {
    dx: 80,
    dy: 0,
    t: 1000 + WHEEL_BURST_IDLE_MS - 1,
  });
  assert.equal(justUnder.axis, 'y', 'a slow burst keeps its latch');
  assert.equal(justUnder.sumY, 40, 'and keeps accumulating its delta');

  const atBoundary = nextWheelBurst(first, {
    dx: 80,
    dy: 0,
    t: 1000 + WHEEL_BURST_IDLE_MS,
  });
  assert.equal(
    atBoundary.axis,
    'y',
    'the boundary gap still continues the burst',
  );
  assert.equal(atBoundary.sumY, 40);

  const justOver = nextWheelBurst(first, {
    dx: 80,
    dy: 0,
    t: 1000 + WHEEL_BURST_IDLE_MS + 1,
  });
  assert.equal(justOver.axis, 'x');
  assert.equal(justOver.sumY, 0, 'the new burst starts from this event only');
});

test('nextWheelBurst: is pure, and an unusable timestamp restarts', () => {
  const seeded = nextWheelBurst(EMPTY_WHEEL_BURST, { dx: 1, dy: 2, t: 500 });
  const snapshot = { ...seeded };
  // A bad timestamp must start a fresh burst, never continue stale state.
  const nan = nextWheelBurst(seeded, { dx: 60, dy: 0, t: Number.NaN });
  assert.equal(nan.axis, 'x');
  assert.equal(nan.sumY, 0);
  assert.deepEqual(seeded, snapshot, 'the reducer does not mutate its input');
});

// ── clampMinutesToDay ───────────────────────────────────────────────────

test('clampMinutesToDay: fully inside the day', () => {
  const day = new Date(2026, 8, 8); // Tue
  const start = new Date(2026, 8, 8, 9, 0, 0, 0);
  const end = new Date(2026, 8, 8, 10, 30, 0, 0);
  const r = clampMinutesToDay(start, end, day);
  assert.ok(r);
  assert.equal(r!.startMin, 9 * 60);
  assert.equal(r!.endMin, 10 * 60 + 30);
});

test('clampMinutesToDay: starts previous night → from midnight', () => {
  const day = new Date(2026, 8, 9); // Wed
  // Tue 11pm → Wed 1am
  const start = new Date(2026, 8, 8, 23, 0, 0, 0);
  const end = new Date(2026, 8, 9, 1, 0, 0, 0);
  const r = clampMinutesToDay(start, end, day);
  assert.ok(r);
  assert.equal(r!.startMin, 0);
  assert.equal(r!.endMin, 60);
});

test('clampMinutesToDay: ends next morning → through end of day', () => {
  const day = new Date(2026, 8, 8); // Tue
  // Tue 11pm → Wed 1am
  const start = new Date(2026, 8, 8, 23, 0, 0, 0);
  const end = new Date(2026, 8, 9, 1, 0, 0, 0);
  const r = clampMinutesToDay(start, end, day);
  assert.ok(r);
  assert.equal(r!.startMin, 23 * 60);
  assert.equal(r!.endMin, 24 * 60);
});

test('clampMinutesToDay: no overlap → null', () => {
  const day = new Date(2026, 8, 10); // Thu
  const start = new Date(2026, 8, 8, 9, 0, 0, 0);
  const end = new Date(2026, 8, 8, 10, 0, 0, 0);
  assert.equal(clampMinutesToDay(start, end, day), null);
});

// ── eventTopPx / eventHeightPx ──────────────────────────────────────────

test('eventTopPx / eventHeightPx: 60 min at hourH=88', () => {
  const hourH = 88;
  // 1:00 → top = 88
  assert.equal(eventTopPx(60, hourH), 88);
  // 60 min → height = 88 - 3 = 85
  assert.equal(eventHeightPx(60, 120, hourH), 85);
});

test('eventHeightPx: 15 min gets CHIP_MIN_H', () => {
  // 15/60 * 88 - 3 = 19; still above min. Use a short duration at small hourH.
  // 5 min at hourH=48 → 5/60*48 - 3 = 1 → CHIP_MIN_H
  assert.equal(eventHeightPx(0, 5, 48), CHIP_MIN_H);
  // Zero duration also mins out.
  assert.equal(eventHeightPx(100, 100, 88), CHIP_MIN_H);
});

test('clampMinutesToDay: zero-length event at midnight stays on that day', () => {
  const day = new Date(2026, 8, 8);
  const t = new Date(2026, 8, 8, 0, 0, 0, 0);
  assert.deepEqual(clampMinutesToDay(t, t, day), { startMin: 0, endMin: 0 });
  assert.equal(clampMinutesToDay(t, t, new Date(2026, 8, 7)), null);
  const noon = new Date(2026, 8, 8, 12, 0, 0, 0);
  assert.deepEqual(clampMinutesToDay(noon, noon, day), {
    startMin: 720,
    endMin: 720,
  });
});

// ── packDayEvents (Notion cascade — notion-event-packing.md examples 1–6) ─

const PACK_EPS = 1e-9;

function assertPackClose(
  actual: {
    leftPercent: number;
    widthPercent: number;
    layerIndex: number;
    leftPixels: number;
  },
  expected: {
    leftPercent: number;
    widthPercent: number;
    layerIndex: number;
    leftPixels?: number;
  },
  label: string,
) {
  assert.ok(
    Math.abs(actual.leftPercent - expected.leftPercent) < PACK_EPS,
    `${label}.leftPercent: got ${actual.leftPercent}, want ${expected.leftPercent}`,
  );
  assert.ok(
    Math.abs(actual.widthPercent - expected.widthPercent) < PACK_EPS,
    `${label}.widthPercent: got ${actual.widthPercent}, want ${expected.widthPercent}`,
  );
  assert.equal(actual.layerIndex, expected.layerIndex, `${label}.layerIndex`);
  assert.equal(
    actual.leftPixels,
    expected.leftPixels ?? 0,
    `${label}.leftPixels`,
  );
}

// Spec § Worked example 1 — No overlap
test('packDayEvents: example 1 — no overlap → full width each cluster', () => {
  const packed = packDayEvents([
    { id: 'a', startMin: 9 * 60, endMin: 10 * 60 },
    { id: 'b', startMin: 11 * 60, endMin: 12 * 60 },
    { id: 'c', startMin: 14 * 60, endMin: 15 * 60 },
  ]);
  assert.equal(packed.length, 3);
  for (const p of packed) {
    assertPackClose(
      p,
      { leftPercent: 0, widthPercent: 1, layerIndex: 1 },
      p.id,
    );
  }
});

// Spec § Worked example 2 — Two overlap
test('packDayEvents: example 2 — two overlap → peek + steal', () => {
  const packed = packDayEvents([
    { id: 'a', startMin: 9 * 60, endMin: 11 * 60 },
    { id: 'b', startMin: 10 * 60, endMin: 12 * 60 },
  ]);
  const byId = Object.fromEntries(packed.map((p) => [p.id, p]));
  assertPackClose(
    byId.a,
    { leftPercent: 0, widthPercent: 1, layerIndex: 1 },
    'a',
  );
  assertPackClose(
    byId.b,
    { leftPercent: 0.05, widthPercent: 0.95, layerIndex: 2 },
    'b',
  );
});

// Spec § Worked example 3 — Same start, longer first
test('packDayEvents: example 3 — same start, longer first → half-peek', () => {
  const packed = packDayEvents([
    { id: 'short', startMin: 9 * 60, endMin: 10 * 60 },
    { id: 'long', startMin: 9 * 60, endMin: 12 * 60 },
  ]);
  const byId = Object.fromEntries(packed.map((p) => [p.id, p]));
  assertPackClose(
    byId.long,
    { leftPercent: 0, widthPercent: 0.75, layerIndex: 1 },
    'long',
  );
  assertPackClose(
    byId.short,
    { leftPercent: 0.5, widthPercent: 0.5, layerIndex: 2 },
    'short',
  );
});

// Spec § Worked example 4 — Chain
test('packDayEvents: example 4 — chain A/B/C with stolen cascade', () => {
  const packed = packDayEvents([
    { id: 'a', startMin: 9 * 60, endMin: 12 * 60 },
    { id: 'b', startMin: 10 * 60, endMin: 11 * 60 },
    { id: 'c', startMin: 10 * 60 + 30, endMin: 13 * 60 },
  ]);
  const byId = Object.fromEntries(packed.map((p) => [p.id, p]));
  // A: base 1/3; full peek B +1/3; full peek C +1/3 → 1
  assertPackClose(
    byId.a,
    { leftPercent: 0, widthPercent: 1, layerIndex: 1 },
    'a',
  );
  // B: left = 0.05; width = 0.5 + 1/3 - 0.05 = 5/6 - 0.05
  assertPackClose(
    byId.b,
    { leftPercent: 0.05, widthPercent: 0.5 + 1 / 3 - 0.05, layerIndex: 2 },
    'b',
  );
  // C: left = 1/3 + 0.05; width = 2/3 - 0.05
  assertPackClose(
    byId.c,
    { leftPercent: 1 / 3 + 0.05, widthPercent: 2 / 3 - 0.05, layerIndex: 3 },
    'c',
  );
});

// Spec § Worked example 5 — B and C share a column
test('packDayEvents: example 5 — B and C share column after left-compact', () => {
  const packed = packDayEvents([
    { id: 'a', startMin: 9 * 60, endMin: 12 * 60 },
    { id: 'b', startMin: 9 * 60, endMin: 10 * 60 },
    { id: 'c', startMin: 11 * 60, endMin: 12 * 60 },
  ]);
  const byId = Object.fromEntries(packed.map((p) => [p.id, p]));
  assertPackClose(
    byId.a,
    { leftPercent: 0, widthPercent: 0.75, layerIndex: 1 },
    'a',
  );
  assertPackClose(
    byId.b,
    { leftPercent: 0.5, widthPercent: 0.5, layerIndex: 2 },
    'b',
  );
  assertPackClose(
    byId.c,
    { leftPercent: 0.05, widthPercent: 0.95, layerIndex: 2 },
    'c',
  );
});

// Spec § Worked example 6 — Touching endpoints (half-open)
test('packDayEvents: example 6 — touching endpoints → separate full-width clusters', () => {
  const packed = packDayEvents([
    { id: 'a', startMin: 9 * 60, endMin: 10 * 60 },
    { id: 'b', startMin: 10 * 60, endMin: 11 * 60 },
  ]);
  const byId = Object.fromEntries(packed.map((p) => [p.id, p]));
  assertPackClose(
    byId.a,
    { leftPercent: 0, widthPercent: 1, layerIndex: 1 },
    'a',
  );
  assertPackClose(
    byId.b,
    { leftPercent: 0, widthPercent: 1, layerIndex: 1 },
    'b',
  );
});

test('packDayEvents: two 0-min events at the same instant sit side by side', () => {
  const packed = packDayEvents(
    [
      { id: 'a', startMin: 600, endMin: 600 },
      { id: 'b', startMin: 600, endMin: 600 },
    ],
    22.5,
    PACK_WIDEN_BELOW_MIN,
  );
  assert.equal(packed.length, 2);
  for (const p of packed) {
    assert.ok(
      Number.isFinite(p.leftPercent) && Number.isFinite(p.widthPercent),
    );
    assert.ok(p.widthPercent > 0);
  }
  assert.notEqual(packed[0].leftPercent, packed[1].leftPercent);
});

test('packDayEvents: 0-min at start of another event overlaps; at its end does not', () => {
  const atStart = packDayEvents(
    [
      { id: 'a', startMin: 540, endMin: 600 },
      { id: 'z', startMin: 540, endMin: 540 },
    ],
    22.5,
  );
  const byStart = Object.fromEntries(atStart.map((p) => [p.id, p]));
  assert.ok(byStart.a.widthPercent < 1 + 1e-9);
  assert.notEqual(byStart.a.leftPercent, byStart.z.leftPercent);

  const atEnd = packDayEvents(
    [
      { id: 'a', startMin: 540, endMin: 600 },
      { id: 'z', startMin: 600, endMin: 600 },
    ],
    22.5,
  );
  for (const p of atEnd) {
    assert.equal(p.leftPercent, 0);
    assert.equal(p.widthPercent, 1);
  }
});

test('packDayEvents: min painted height makes nearby 0-min events collide', () => {
  const items = [
    { id: 'a', startMin: 600, endMin: 600 },
    { id: 'b', startMin: 610, endMin: 610 },
  ];
  // True durations: ε only → no overlap, both full width.
  assert.equal(
    packDayEvents(items).every((p) => p.widthPercent === 1),
    true,
  );
  // Painted height 22.5 min covers b → they share the row.
  const packed = packDayEvents(items, 22.5);
  assert.equal(
    packed.some((p) => p.widthPercent < 1 + 1e-9 && p.leftPercent > 0),
    true,
  );
});

test('packDayEvents: back-to-back 15-min events keep full width at hourH=48', () => {
  const packed = packDayEvents(
    [
      { id: 'a', startMin: 600, endMin: 615 },
      { id: 'b', startMin: 615, endMin: 630 },
    ],
    (CHIP_MIN_H / 48) * 60,
    PACK_WIDEN_BELOW_MIN,
  );
  for (const p of packed) {
    assert.equal(p.leftPercent, 0);
    assert.equal(p.widthPercent, 1);
  }
});

test('packDayEvents: widen threshold is 15 min, not the 1-min snap', () => {
  assert.equal(PACK_WIDEN_BELOW_MIN, 15);

  // 14 min → widened to the 22.5-min painted height, so it collides with a
  // neighbour that starts inside that height.
  const widened = packDayEvents(
    [
      { id: 'a', startMin: 600, endMin: 614 },
      { id: 'b', startMin: 620, endMin: 640 },
    ],
    22.5,
    PACK_WIDEN_BELOW_MIN,
  );
  // Overlap signal from example 2: the later chip peeks, so a widened 14-min
  // event must leave at least one chip shifted right and narrower than full.
  assert.ok(
    widened.some((p) => p.leftPercent > 0 && p.widthPercent < 1),
    'the 14-min event must widen and collide with its neighbour',
  );

  // 15 min → not widened; it packs exactly and the 620 neighbour clears it.
  const exact = packDayEvents(
    [
      { id: 'a', startMin: 600, endMin: 615 },
      { id: 'b', startMin: 620, endMin: 640 },
    ],
    22.5,
    PACK_WIDEN_BELOW_MIN,
  );
  for (const p of exact) {
    assert.equal(p.leftPercent, 0);
    assert.equal(p.widthPercent, 1);
  }
});

// ── nowLineY ────────────────────────────────────────────────────────────

test('nowLineY: noon at hourH=48 → 576', () => {
  const noon = new Date(2026, 8, 8, 12, 0, 0, 0);
  assert.equal(nowLineY(noon, 48), 576);
});

// ── hourHeight ──────────────────────────────────────────────────────────

test('hourHeight: short viewport stays at base (≥48)', () => {
  // 600px / 24 = 25 → max(48, 25) = 48, within [16, 208]
  assert.equal(hourHeight(600), HOUR_H_BASE);
  assert.ok(hourHeight(600) >= HOUR_H_BASE);
});

test('hourHeight: huge viewport caps at 208', () => {
  // 10000 / 24 ≈ 416 → clamp to 208
  assert.equal(hourHeight(10_000), HOUR_H_MAX);
});

test('hourHeight: never below 16; tall-enough stretches above base', () => {
  assert.ok(hourHeight(1) >= HOUR_H_MIN);
  // 24 * 88 = 2112 → stretched = 88
  assert.equal(hourHeight(2112), 88);
  // Invalid / zero → base
  assert.equal(hourHeight(0), HOUR_H_BASE);
  assert.equal(hourHeight(-10), HOUR_H_BASE);
});

// ── hourGridBackground ──────────────────────────────────────────────────

test('hourGridBackground: period uses hourH and hourH-1', () => {
  const bg = hourGridBackground(48);
  assert.ok(bg.includes('48px'));
  assert.ok(bg.includes('47px'));
  assert.ok(bg.startsWith('repeating-linear-gradient'));
});

// ── monthGridStart / monthGridDays ──────────────────────────────────────

test('monthGridStart: Sep 2026 (Tue 1st) → Mon Aug 31', () => {
  // 2026-09-01 is a Tuesday → grid starts Mon Aug 31.
  const start = monthGridStart(new Date(2026, 8, 15));
  assert.equal(start.getFullYear(), 2026);
  assert.equal(start.getMonth(), 7); // August
  assert.equal(start.getDate(), 31);
  assert.equal(start.getHours(), 0);
  assert.equal(start.getDay(), 1); // Monday
});

test('monthGridDays: always 42 local midnights', () => {
  const days = monthGridDays(new Date(2026, 8, 1));
  assert.equal(days.length, 42);
  const first = monthGridStart(new Date(2026, 8, 1));
  assert.equal(days[0].getTime(), first.getTime());
  assert.equal(
    days[41].getTime() - days[0].getTime(),
    41 * 24 * 60 * 60 * 1000,
  );
  for (const d of days) {
    assert.equal(d.getHours(), 0);
    assert.equal(d.getMinutes(), 0);
  }
});

// ── isMultiDay ──────────────────────────────────────────────────────────

test('isMultiDay: 2h overnight is false (stays on time grid)', () => {
  // Tue 11pm → Wed 1am
  const start = new Date(2026, 8, 8, 23, 0, 0, 0);
  const end = new Date(2026, 8, 9, 1, 0, 0, 0);
  assert.equal(isMultiDay(start, end), false);
});

test('isMultiDay: 36h trip is true', () => {
  const start = new Date(2026, 8, 8, 8, 0, 0, 0);
  const end = new Date(2026, 8, 9, 20, 0, 0, 0); // 36h
  assert.equal(isMultiDay(start, end), true);
});

test('isMultiDay: same-day 8h is false', () => {
  const start = new Date(2026, 8, 8, 9, 0, 0, 0);
  const end = new Date(2026, 8, 8, 17, 0, 0, 0);
  assert.equal(isMultiDay(start, end), false);
});

// ── packAllDayLanes ─────────────────────────────────────────────────────

test('packAllDayLanes: non-overlapping same-week spans share lane 0', () => {
  const packed = packAllDayLanes([
    { id: 'a', startDay: 0, endDay: 1 }, // Mon–Tue
    { id: 'b', startDay: 3, endDay: 5 }, // Thu–Sat
  ]);
  const byId = Object.fromEntries(packed.map((p) => [p.id, p]));
  assert.equal(byId.a.lane, 0);
  assert.equal(byId.b.lane, 0);
});

test('packAllDayLanes: overlapping spans get lanes 0 and 1', () => {
  const packed = packAllDayLanes([
    { id: 'a', startDay: 0, endDay: 3 }, // Mon–Thu
    { id: 'b', startDay: 2, endDay: 5 }, // Wed–Sat
  ]);
  const byId = Object.fromEntries(packed.map((p) => [p.id, p]));
  assert.equal(byId.a.lane, 0);
  assert.equal(byId.b.lane, 1);
});

// ── allDaySectionHeight ─────────────────────────────────────────────────

test('allDaySectionHeight: null → ALLDAY_MIN (25)', () => {
  assert.equal(allDaySectionHeight(null), ALLDAY_MIN);
  assert.equal(allDaySectionHeight(null), 25);
});

test('allDaySectionHeight: lane 0 → ≥25', () => {
  // PAD + (CHIP+GAP)*1 + 1 = 3 + 21 + 1 = 25
  assert.ok(allDaySectionHeight(0) >= ALLDAY_MIN);
  assert.equal(allDaySectionHeight(0), 25);
});

test('allDaySectionHeight: high lane capped at ALLDAY_MAX (137.5)', () => {
  // lane 5 → 3 + 21*6 + 1 = 130 (under cap)
  assert.ok(allDaySectionHeight(5) <= ALLDAY_MAX);
  // lane 6 → 3 + 21*7 + 1 = 151 → clamp 137.5
  assert.equal(allDaySectionHeight(6), ALLDAY_MAX);
  assert.equal(allDaySectionHeight(20), ALLDAY_MAX);
});

// ── hexToRgba ───────────────────────────────────────────────────────────

test('hexToRgba: #2a5c8a @ 0.22', () => {
  assert.equal(hexToRgba('#2a5c8a', 0.22), 'rgba(42, 92, 138, 0.22)');
});

test('hexToRgba: short #rgb expands', () => {
  assert.equal(hexToRgba('#f00', 0.5), 'rgba(255, 0, 0, 0.5)');
});

// ── hexLuminance / contrastingInk ───────────────────────────────────────

test('hexLuminance: black ≈ 0, white ≈ 1', () => {
  assert.ok(Math.abs(hexLuminance('#000000')) < 1e-9);
  assert.ok(Math.abs(hexLuminance('#ffffff') - 1) < 1e-9);
});

test('hexLuminance: dark work-blue < 0.55', () => {
  assert.ok(hexLuminance('#2a5c8a') < 0.55);
});

test('contrastingInk: dark fill → light ink', () => {
  assert.equal(contrastingInk('#2a5c8a'), '#fafafa');
});

test('contrastingInk: light banana → dark ink', () => {
  assert.equal(contrastingInk('#f6bf26'), '#1a1a1a');
});

test('contrastingInk: invalid hex still returns a string', () => {
  const ink = contrastingInk('not-a-hex');
  assert.equal(typeof ink, 'string');
  assert.ok(ink.length > 0);
});

// ── isCompactChip ───────────────────────────────────────────────────────

test('isCompactChip: Notion threshold (inner < timeLine + timeMarginTop)', () => {
  // inner = h - padTop(1) - padBottom(1) - timeLine(11) - marginBottom(3)
  //       = h - 16
  // compact when inner < 11 + 2 = 13  →  h < 29
  assert.equal(isCompactChip(28), true);
  assert.equal(isCompactChip(29), false);
  assert.equal(isCompactChip(CHIP_MIN_H), true); // 18
  assert.equal(isCompactChip(48), false);
});

// ── snapMinutes / minutesFromY / dateOnDay ──────────────────────────────

test('snapMinutes: nearest 1 minute, clamp 0..1440', () => {
  assert.equal(SNAP_MINUTES, 1);
  assert.equal(snapMinutes(7), 7);
  assert.equal(snapMinutes(8), 8);
  assert.equal(snapMinutes(22), 22);
  assert.equal(snapMinutes(23), 23);
  // Half-up: fractional minutes round to the nearest minute.
  assert.equal(snapMinutes(7.4), 7);
  assert.equal(snapMinutes(7.5), 8);
  assert.equal(snapMinutes(7.6), 8);
  assert.equal(snapMinutes(1440), 1440);
  assert.equal(snapMinutes(-5), 0);
  assert.equal(snapMinutes(0), 0);
  assert.equal(snapMinutes(15), 15);
});

test('snapMinutes: optional 1-minute step (resize)', () => {
  // nearest 1-minute
  assert.equal(snapMinutes(10 * 60 + 7.4, RESIZE_SNAP_MINUTES), 607);
  assert.equal(snapMinutes(10 * 60 + 7.6, RESIZE_SNAP_MINUTES), 608);
  assert.equal(snapMinutes(7, 1), 7);
  assert.equal(snapMinutes(8, 1), 8);
  // clamp
  assert.equal(snapMinutes(-5, 1), 0);
  assert.equal(snapMinutes(1440, 1), 1440);
  assert.equal(snapMinutes(1440.2, 1), 1440);
});

test('snapMinutes: explicit 15-minute step still quantizes', () => {
  assert.equal(snapMinutes(7, 15), 0);
  assert.equal(snapMinutes(8, 15), 15);
  assert.equal(snapMinutes(22, 15), 15);
  assert.equal(snapMinutes(23, 15), 30);
});

test('minutesFromY: 88px at hourH=88 → 60', () => {
  assert.equal(minutesFromY(88, 88), 60);
  assert.equal(minutesFromY(0, 88), 0);
  assert.equal(minutesFromY(44, 88), 30);
});

test('dateOnDay: 9:00 on a known local date', () => {
  const day = new Date(2026, 8, 8); // Tue Sep 8 local midnight-ish
  const at9 = dateOnDay(day, 9 * 60);
  assert.equal(at9.getFullYear(), 2026);
  assert.equal(at9.getMonth(), 8);
  assert.equal(at9.getDate(), 8);
  assert.equal(at9.getHours(), 9);
  assert.equal(at9.getMinutes(), 0);
  assert.equal(at9.getSeconds(), 0);
});

// ── formatEventDuration ─────────────────────────────────────────────────

test('formatEventDuration: 15 minutes → "15min"', () => {
  const start = new Date(2026, 8, 13, 5, 0);
  const end = new Date(2026, 8, 13, 5, 15);
  assert.equal(formatEventDuration(start, end), '15min');
});

test('formatEventDuration: exactly 60 minutes → "1h"', () => {
  const start = new Date(2026, 8, 13, 5, 0);
  const end = new Date(2026, 8, 13, 6, 0);
  assert.equal(formatEventDuration(start, end), '1h');
});

test('formatEventDuration: 90 minutes → "1h 30min"', () => {
  const start = new Date(2026, 8, 13, 5, 0);
  const end = new Date(2026, 8, 13, 6, 30);
  assert.equal(formatEventDuration(start, end), '1h 30min');
});

test('formatEventDuration: 5 hours → "5h"', () => {
  const start = new Date(2026, 8, 13, 5, 0);
  const end = new Date(2026, 8, 13, 10, 0);
  assert.equal(formatEventDuration(start, end), '5h');
});

test('formatEventDuration: zero-length → "0min"', () => {
  const start = new Date(2026, 8, 13, 5, 15);
  assert.equal(formatEventDuration(start, start), '0min');
});

test('formatEventDuration: inverted range clamps to "0min"', () => {
  const start = new Date(2026, 8, 13, 6, 0);
  const end = new Date(2026, 8, 13, 5, 0);
  assert.equal(formatEventDuration(start, end), '0min');
});

// ── formatEventTimeDetail ───────────────────────────────────────────────

test('formatEventTimeDetail: 5:00–6:30 → range + duration', () => {
  const start = new Date(2026, 8, 13, 5, 0);
  const end = new Date(2026, 8, 13, 6, 30);
  assert.equal(formatEventTimeDetail(start, end), '5 AM – 6:30 AM · 1h 30min');
});

test('formatEventTimeDetail: exactly 60 minutes on the hour → "5 AM – 6 AM · 1h"', () => {
  const start = new Date(2026, 8, 13, 5, 0);
  const end = new Date(2026, 8, 13, 6, 0);
  assert.equal(formatEventTimeDetail(start, end), '5 AM – 6 AM · 1h');
});

test('formatEventTimeDetail: inverted range still formats (duration 0min)', () => {
  const start = new Date(2026, 8, 13, 6, 0);
  const end = new Date(2026, 8, 13, 5, 0);
  assert.equal(formatEventTimeDetail(start, end), '6 AM – 5 AM · 0min');
});

// ── formatEventDateLine ─────────────────────────────────────────────────

test('formatEventDateLine: same civil day Sunday Sep 13 2026', () => {
  const start = new Date(2026, 8, 13, 5, 15);
  const end = new Date(2026, 8, 13, 6, 30);
  assert.equal(formatEventDateLine(start, end), 'Sun Sep 13');
});

test('formatEventDateLine: cross midnight uses arrow between civil dates', () => {
  const start = new Date(2026, 8, 13, 22, 0);
  const end = new Date(2026, 8, 14, 0, 30);
  assert.equal(formatEventDateLine(start, end), 'Sun Sep 13 → Mon Sep 14');
});

test('formatEventDateLine: Monday uses Sun-first names (not WEEK_DAYS)', () => {
  // 2026-09-07 is a Monday.
  const start = new Date(2026, 8, 7, 9, 0);
  const end = new Date(2026, 8, 7, 10, 0);
  assert.equal(formatEventDateLine(start, end), 'Mon Sep 7');
});

// ── toTimeInputValue / toDateInputValue ──────────────────────────────────

test('toTimeInputValue: zero-pads local 24h HH:mm', () => {
  assert.equal(toTimeInputValue(new Date(2026, 8, 13, 5, 15)), '05:15');
  assert.equal(toTimeInputValue(new Date(2026, 8, 13, 0, 0)), '00:00');
  assert.equal(toTimeInputValue(new Date(2026, 8, 13, 23, 5)), '23:05');
});

test('toDateInputValue: local civil YYYY-MM-DD', () => {
  assert.equal(toDateInputValue(new Date(2026, 8, 13, 5, 15)), '2026-09-13');
  assert.equal(toDateInputValue(new Date(2026, 0, 5, 23, 59)), '2026-01-05');
});

// ── applyStartTime / applyEndTime / applyStartDate ───────────────────────

test('applyStartTime: preserves duration; invalid → null', () => {
  const start = new Date(2026, 8, 13, 5, 15);
  const end = new Date(2026, 8, 13, 5, 30);
  const next = applyStartTime(start, end, '06:00');
  assert.ok(next);
  assert.equal(next.start.getHours(), 6);
  assert.equal(next.start.getMinutes(), 0);
  assert.equal(next.end.getHours(), 6);
  assert.equal(next.end.getMinutes(), 15);
  assert.equal(next.start.getDate(), 13);
  assert.equal(applyStartTime(start, end, ''), null);
  assert.equal(applyStartTime(start, end, 'garbage'), null);
  assert.equal(applyStartTime(start, end, '25:00'), null);
});

test('applyEndTime: keeps start; end === start → 0-min; end < start clamps to start; invalid → null', () => {
  const start = new Date(2026, 8, 13, 5, 15);
  const end = new Date(2026, 8, 13, 5, 30);

  const later = applyEndTime(start, end, '07:00');
  assert.ok(later);
  assert.equal(later.start.getTime(), start.getTime());
  assert.equal(later.end.getHours(), 7);
  assert.equal(later.end.getMinutes(), 0);

  const clamped = applyEndTime(start, end, '04:00');
  assert.ok(clamped);
  assert.equal(clamped.start.getTime(), start.getTime());
  assert.equal(clamped.end.getTime(), start.getTime());

  const equal = applyEndTime(start, end, '05:15');
  assert.ok(equal);
  assert.equal(equal.start.getTime(), start.getTime());
  assert.equal(equal.end.getTime(), start.getTime());

  // Overnight: end civil date kept when editing end clock.
  const nightStart = new Date(2026, 8, 13, 23, 0);
  const nightEnd = new Date(2026, 8, 14, 1, 0);
  const overnight = applyEndTime(nightStart, nightEnd, '01:30');
  assert.ok(overnight);
  assert.equal(overnight.end.getDate(), 14);
  assert.equal(overnight.end.getHours(), 1);
  assert.equal(overnight.end.getMinutes(), 30);

  assert.equal(applyEndTime(start, end, ''), null);
  assert.equal(applyEndTime(start, end, 'xx:yy'), null);
});

test('applyStartDate: shifts both instants by whole-day delta; invalid → null', () => {
  const start = new Date(2026, 8, 13, 5, 15);
  const end = new Date(2026, 8, 13, 5, 30);
  const next = applyStartDate(start, end, '2026-09-14');
  assert.ok(next);
  assert.equal(next.start.getFullYear(), 2026);
  assert.equal(next.start.getMonth(), 8);
  assert.equal(next.start.getDate(), 14);
  assert.equal(next.start.getHours(), 5);
  assert.equal(next.start.getMinutes(), 15);
  assert.equal(next.end.getDate(), 14);
  assert.equal(next.end.getHours(), 5);
  assert.equal(next.end.getMinutes(), 30);

  // Overnight: both shift, end stays next day relative.
  const nightStart = new Date(2026, 8, 13, 23, 0);
  const nightEnd = new Date(2026, 8, 14, 1, 0);
  const shifted = applyStartDate(nightStart, nightEnd, '2026-09-15');
  assert.ok(shifted);
  assert.equal(shifted.start.getDate(), 15);
  assert.equal(shifted.start.getHours(), 23);
  assert.equal(shifted.end.getDate(), 16);
  assert.equal(shifted.end.getHours(), 1);

  assert.equal(applyStartDate(start, end, ''), null);
  assert.equal(applyStartDate(start, end, 'not-a-date'), null);
  assert.equal(applyStartDate(start, end, '2026-02-30'), null);
});

// ── defaultWritableCalendar ─────────────────────────────────────────────

test('defaultWritableCalendar: prefers primary writer; skips reader', () => {
  const calendars = [
    {
      id: 'r1',
      is_primary: false,
      access_role: 'reader',
      summary: 'Shared',
    },
    {
      id: 'w1',
      is_primary: false,
      access_role: 'writer',
      summary: 'Work',
    },
    {
      id: 'p1',
      is_primary: true,
      access_role: 'owner',
      summary: 'Primary',
    },
  ];
  const pick = defaultWritableCalendar(calendars);
  assert.ok(pick);
  assert.equal(pick!.id, 'p1');
});

test('defaultWritableCalendar: falls back to first writer when primary is reader', () => {
  const calendars = [
    {
      id: 'p-ro',
      is_primary: true,
      access_role: 'reader',
      summary: 'Primary RO',
    },
    {
      id: 'w1',
      is_primary: false,
      access_role: 'writer',
      summary: 'Work',
    },
  ];
  const pick = defaultWritableCalendar(calendars);
  assert.ok(pick);
  assert.equal(pick!.id, 'w1');
});

test('defaultWritableCalendar: undefined when only readers', () => {
  assert.equal(
    defaultWritableCalendar([
      { id: 'r1', is_primary: true, access_role: 'reader', summary: 'RO' },
    ]),
    undefined,
  );
});

// ── All-day civil helpers ───────────────────────────────────────────────

test('civilDateFromAllDayIso: uses ISO prefix, not local Date(iso) shift', () => {
  // UTC midnight Sep 13 is previous evening in US zones — prefix must win.
  const d = civilDateFromAllDayIso('2026-09-13T00:00:00Z');
  assert.ok(d);
  assert.equal(d!.getFullYear(), 2026);
  assert.equal(d!.getMonth(), 8);
  assert.equal(d!.getDate(), 13);
  assert.equal(d!.getHours(), 0);
  assert.equal(d!.getMinutes(), 0);
  assert.equal(civilDateFromAllDayIso('not-a-date'), null);
  assert.equal(civilDateFromAllDayIso(''), null);
});

test('timedRangeToAllDay: same-day timed → exclusive next civil day', () => {
  const start = new Date(2026, 8, 13, 5, 15);
  const end = new Date(2026, 8, 13, 5, 30);
  const next = timedRangeToAllDay(start, end);
  assert.equal(next.startDate, '2026-09-13');
  assert.equal(next.endDate, '2026-09-14');
  assert.equal(next.startIso, '2026-09-13T00:00:00Z');
  assert.equal(next.endIso, '2026-09-14T00:00:00Z');
});

test('allDayRangeToTimed: civil start at 09:00–10:00 local', () => {
  const next = allDayRangeToTimed(
    '2026-09-13T00:00:00Z',
    '2026-09-14T00:00:00Z',
  );
  assert.ok(next);
  assert.equal(next!.start.getFullYear(), 2026);
  assert.equal(next!.start.getMonth(), 8);
  assert.equal(next!.start.getDate(), 13);
  assert.equal(next!.start.getHours(), 9);
  assert.equal(next!.start.getMinutes(), 0);
  assert.equal(next!.end.getHours(), 10);
  assert.equal(next!.end.getMinutes(), 0);
});

test('applyAllDayStartDate: shifts exclusive end by same day delta', () => {
  const next = applyAllDayStartDate(
    '2026-09-13T00:00:00Z',
    '2026-09-14T00:00:00Z',
    '2026-09-15',
  );
  assert.ok(next);
  assert.equal(next!.startIso, '2026-09-15T00:00:00Z');
  assert.equal(next!.endIso, '2026-09-16T00:00:00Z');
});
