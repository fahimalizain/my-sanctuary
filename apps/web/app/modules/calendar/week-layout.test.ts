// Unit tests for the week time-grid pure helpers (date math, geometry,
// overlap packing). No React / DOM.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  ALLDAY_MAX,
  ALLDAY_MIN,
  CHIP_MIN_H,
  DAYS_PER_PERIOD,
  HOUR_H_BASE,
  HOUR_H_MAX,
  HOUR_H_MIN,
  STRIP_OVERSCAN,
  STRIP_REBASE_THRESHOLD,
  TIME_GUTTER_W,
  allDaySectionHeight,
  clampMinutesToDay,
  colWidth,
  dayIndexFromScroll,
  eventHeightPx,
  eventTopPx,
  formatDayRangeTitle,
  formatWeekTitle,
  gutterWithRemainder,
  hexToRgba,
  hourHeight,
  isCompactChip,
  isMultiDay,
  monthGridDays,
  monthGridStart,
  nowLineY,
  packAllDayLanes,
  packDayEvents,
  rangeIso,
  scrollLeftForIndex,
  shiftWindowStart,
  shouldRebase,
  startOfWeek,
  stripDayCount,
  visibleStartIndex,
  weekRangeIso,
  snapMinutes,
  minutesFromY,
  dateOnDay,
  defaultWritableCalendar,
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
  assert.equal(
    formatWeekTitle(new Date(2026, 8, 28)),
    'Sep 28 – Oct 4, 2026',
  );
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

// ── Infinite strip geometry ─────────────────────────────────────────────

test('colWidth / gutterWithRemainder: available 800 → 7 cols + gutter = 800', () => {
  const available = 800;
  const colW = colWidth(available);
  const gutter = gutterWithRemainder(available, colW);
  assert.equal(colW, Math.floor((800 - TIME_GUTTER_W) / 7));
  assert.equal(gutter + colW * DAYS_PER_PERIOD, available);
  assert.ok(colW >= 16);
  assert.ok(gutter >= TIME_GUTTER_W);
});

test('colWidth: never below 16', () => {
  assert.equal(colWidth(0), 16);
  assert.equal(colWidth(10), 16);
});

test('stripDayCount: period + overscan each side = 21', () => {
  assert.equal(stripDayCount(), DAYS_PER_PERIOD + STRIP_OVERSCAN * 2);
  assert.equal(stripDayCount(), 21);
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

// ── packDayEvents ───────────────────────────────────────────────────────

test('packDayEvents: no overlap → all col 0, cols 1', () => {
  const packed = packDayEvents([
    { id: 'a', startMin: 9 * 60, endMin: 10 * 60 },
    { id: 'b', startMin: 11 * 60, endMin: 12 * 60 },
    { id: 'c', startMin: 14 * 60, endMin: 15 * 60 },
  ]);
  assert.equal(packed.length, 3);
  for (const p of packed) {
    assert.equal(p.col, 0);
    assert.equal(p.cols, 1);
    assert.equal(p.span, 1);
  }
});

test('packDayEvents: two overlapping → cols 2, different col', () => {
  const packed = packDayEvents([
    { id: 'a', startMin: 9 * 60, endMin: 11 * 60 },
    { id: 'b', startMin: 10 * 60, endMin: 12 * 60 },
  ]);
  const byId = Object.fromEntries(packed.map((p) => [p.id, p]));
  assert.equal(byId.a.cols, 2);
  assert.equal(byId.b.cols, 2);
  assert.notEqual(byId.a.col, byId.b.col);
  // a starts earlier → col 0
  assert.equal(byId.a.col, 0);
  assert.equal(byId.b.col, 1);
});

test('packDayEvents: three nested / chain overlaps', () => {
  // A 9-12, B 10-11, C 10:30-13 → all one cluster, 3 cols
  const packed = packDayEvents([
    { id: 'a', startMin: 9 * 60, endMin: 12 * 60 },
    { id: 'b', startMin: 10 * 60, endMin: 11 * 60 },
    { id: 'c', startMin: 10 * 60 + 30, endMin: 13 * 60 },
  ]);
  const byId = Object.fromEntries(packed.map((p) => [p.id, p]));
  assert.equal(byId.a.cols, 3);
  assert.equal(byId.b.cols, 3);
  assert.equal(byId.c.cols, 3);
  // a earliest → col 0; b next → col 1; c → col 2
  assert.equal(byId.a.col, 0);
  assert.equal(byId.b.col, 1);
  assert.equal(byId.c.col, 2);
});

test('packDayEvents: same start, longer first (longer gets col 0)', () => {
  const packed = packDayEvents([
    { id: 'short', startMin: 9 * 60, endMin: 10 * 60 },
    { id: 'long', startMin: 9 * 60, endMin: 12 * 60 },
  ]);
  const byId = Object.fromEntries(packed.map((p) => [p.id, p]));
  assert.equal(byId.long.col, 0);
  assert.equal(byId.short.col, 1);
  assert.equal(byId.long.cols, 2);
  assert.equal(byId.short.cols, 2);
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

test('snapMinutes: nearest 15, clamp 0..1440', () => {
  assert.equal(snapMinutes(7), 0);
  assert.equal(snapMinutes(8), 15);
  assert.equal(snapMinutes(22), 15);
  assert.equal(snapMinutes(23), 30);
  assert.equal(snapMinutes(1440), 1440);
  assert.equal(snapMinutes(-5), 0);
  assert.equal(snapMinutes(0), 0);
  assert.equal(snapMinutes(15), 15);
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
