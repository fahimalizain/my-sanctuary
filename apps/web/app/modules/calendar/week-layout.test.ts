// Unit tests for the week time-grid pure helpers (date math, geometry,
// overlap packing). No React / DOM.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  CHIP_MIN_H,
  HOUR_H_BASE,
  HOUR_H_MAX,
  HOUR_H_MIN,
  clampMinutesToDay,
  eventHeightPx,
  eventTopPx,
  formatWeekTitle,
  hourHeight,
  nowLineY,
  packDayEvents,
  startOfWeek,
  weekRangeIso,
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
