// Unit tests for calendar drag geometry helpers.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  DRAG_THRESHOLD_PX,
  RESIZE_HANDLE_PX,
  AUTOSCROLL_EDGE_PX,
  AUTOSCROLL_MAX_PX,
  allDayGrabOffsetDays,
  allDayPreviewIndices,
  autoscrollDelta,
  classifyTouchGesture,
  isTapCreatePointer,
  movedAllDayRange,
  movedEnough,
  movedRange,
  rangeFromSlots,
  resizeEdgeAt,
  resizedRange,
  timedPreviewSegments,
  toAllDayRange,
  toTimedRange,
  type DragSlot,
} from './calendar-drag';
import { SNAP_MINUTES, addDays } from './week-layout';

function localDay(y: number, m: number, d: number): Date {
  return new Date(y, m, d);
}

function slot(day: Date, minutes: number): DragSlot {
  return { day, minutes };
}

// ── movedEnough ─────────────────────────────────────────────────────────

test('movedEnough: under threshold is false', () => {
  assert.equal(movedEnough(3, 0), false);
  assert.equal(movedEnough(0, 3), false);
  // 3-4-5 triangle: hypot(2.4, 1.8) = 3 < 4
  assert.equal(movedEnough(2.4, 1.8), false);
});

test('movedEnough: at threshold is true', () => {
  assert.equal(movedEnough(DRAG_THRESHOLD_PX, 0), true);
  assert.equal(movedEnough(0, DRAG_THRESHOLD_PX), true);
  assert.equal(movedEnough(4, 0), true);
});

// ── isTapCreatePointer ──────────────────────────────────────────────────

test('isTapCreatePointer: touch → true', () => {
  assert.equal(isTapCreatePointer('touch'), true);
});

test('isTapCreatePointer: mouse → false', () => {
  assert.equal(isTapCreatePointer('mouse'), false);
});

test('isTapCreatePointer: pen → false', () => {
  assert.equal(isTapCreatePointer('pen'), false);
});

test('isTapCreatePointer: empty → false', () => {
  assert.equal(isTapCreatePointer(''), false);
});

// ── classifyTouchGesture ────────────────────────────────────────────────

test('classifyTouchGesture: under threshold → pending (create and chip)', () => {
  assert.equal(classifyTouchGesture(2, 1, 'create'), 'pending');
  assert.equal(classifyTouchGesture(1, 2, 'chip'), 'pending');
  assert.equal(classifyTouchGesture(0, 0, 'create'), 'pending');
});

test('classifyTouchGesture: create + horizontal-dominant → scroll', () => {
  assert.equal(classifyTouchGesture(10, 2, 'create'), 'scroll');
});

test('classifyTouchGesture: create + vertical-dominant → drag', () => {
  assert.equal(classifyTouchGesture(2, 10, 'create'), 'drag');
});

test('classifyTouchGesture: create + 45° tie past threshold → drag', () => {
  assert.equal(classifyTouchGesture(5, 5, 'create'), 'drag');
});

test('classifyTouchGesture: create + exactly threshold on X only → scroll', () => {
  assert.equal(classifyTouchGesture(DRAG_THRESHOLD_PX, 0, 'create'), 'scroll');
});

test('classifyTouchGesture: create + exactly threshold on Y only → drag', () => {
  assert.equal(classifyTouchGesture(0, DRAG_THRESHOLD_PX, 'create'), 'drag');
});

test('classifyTouchGesture: chip + horizontal-dominant → drag', () => {
  assert.equal(classifyTouchGesture(10, 2, 'chip'), 'drag');
});

test('classifyTouchGesture: chip + vertical-dominant → drag', () => {
  assert.equal(classifyTouchGesture(2, 10, 'chip'), 'drag');
});

test('classifyTouchGesture: chip under threshold → pending', () => {
  assert.equal(classifyTouchGesture(3, 0, 'chip'), 'pending');
});

test('classifyTouchGesture: create + horizontal + allowHorizontalDrag → drag', () => {
  assert.equal(classifyTouchGesture(10, 2, 'create', true), 'drag');
});

// ── autoscrollDelta ─────────────────────────────────────────────────────

test('autoscrollDelta: center → 0', () => {
  assert.equal(autoscrollDelta(100, 0, 200), 0);
});

test('autoscrollDelta: at start edge → -max', () => {
  assert.equal(autoscrollDelta(0, 0, 400), -AUTOSCROLL_MAX_PX);
});

test('autoscrollDelta: at end edge → +max', () => {
  assert.equal(autoscrollDelta(400, 0, 400), AUTOSCROLL_MAX_PX);
});

test('autoscrollDelta: halfway into start edge → half speed', () => {
  const pointer = AUTOSCROLL_EDGE_PX / 2;
  assert.equal(autoscrollDelta(pointer, 0, 400), -AUTOSCROLL_MAX_PX / 2);
});

test('autoscrollDelta: inverted bounds → 0', () => {
  assert.equal(autoscrollDelta(10, 100, 0), 0);
});

// ── toAllDayRange / toTimedRange / movedAllDayRange ─────────────────────

test('toAllDayRange: Tue → Tue 00:00 to Wed 00:00', () => {
  const tue = localDay(2024, 0, 2);
  const r = toAllDayRange(slot(tue, 9 * 60));
  assert.equal(r.start.getDate(), 2);
  assert.equal(r.start.getHours(), 0);
  assert.equal(r.end.getDate(), 3);
  assert.equal(r.end.getHours(), 0);
});

test('toTimedRange: 14:00 → 14:00–14:30', () => {
  const tue = localDay(2024, 0, 2);
  const r = toTimedRange(slot(tue, 14 * 60));
  assert.equal(r.start.getHours(), 14);
  assert.equal(r.start.getMinutes(), 0);
  assert.equal(r.end.getHours(), 14);
  assert.equal(r.end.getMinutes(), 30);
});

test('allDayGrabOffsetDays: grab Wed of Mon-start → 2', () => {
  const mon = localDay(2024, 0, 1);
  const wed = localDay(2024, 0, 3);
  assert.equal(allDayGrabOffsetDays(mon, wed), 2);
});

test('movedAllDayRange: Mon–Wed grabbed on Wed, drop Fri → Wed–Fri', () => {
  const mon = localDay(2024, 0, 1);
  const thu = localDay(2024, 0, 4); // exclusive end
  const fri = localDay(2024, 0, 5);
  const r = movedAllDayRange(mon, thu, slot(fri, 0), 2);
  assert.equal(r.start.getDate(), 3); // Wed
  assert.equal(r.end.getDate(), 6); // Sat exclusive → Fri last occupied
});

// ── resizeEdgeAt ────────────────────────────────────────────────────────

test('resizeEdgeAt: top handle → start', () => {
  assert.equal(resizeEdgeAt(2, 80), 'start');
  assert.equal(resizeEdgeAt(0, 80), 'start');
  assert.equal(resizeEdgeAt(RESIZE_HANDLE_PX, 80), 'start');
});

test('resizeEdgeAt: bottom handle → end', () => {
  assert.equal(resizeEdgeAt(78, 80), 'end');
  assert.equal(resizeEdgeAt(80, 80), 'end');
  assert.equal(resizeEdgeAt(80 - RESIZE_HANDLE_PX, 80), 'end');
});

test('resizeEdgeAt: middle → null', () => {
  assert.equal(resizeEdgeAt(40, 80), null);
  assert.equal(resizeEdgeAt(RESIZE_HANDLE_PX + 1, 80), null);
});

test('resizeEdgeAt: short chip splits at mid', () => {
  // height 10 < 2*6 → mid = 5
  assert.equal(resizeEdgeAt(2, 10), 'start');
  assert.equal(resizeEdgeAt(7, 10), 'end');
  assert.equal(resizeEdgeAt(4.9, 10), 'start');
  assert.equal(resizeEdgeAt(5, 10), 'end');
});

// ── rangeFromSlots timed ────────────────────────────────────────────────

test('rangeFromSlots timed: same day 9:00–10:30', () => {
  const mon = localDay(2024, 0, 1); // Mon Jan 1 2024
  const r = rangeFromSlots(slot(mon, 9 * 60), slot(mon, 10 * 60 + 30), 'timed');
  assert.equal(r.start.getHours(), 9);
  assert.equal(r.start.getMinutes(), 0);
  assert.equal(r.end.getHours(), 10);
  assert.equal(r.end.getMinutes(), 30);
  assert.equal(r.start.getDate(), 1);
  assert.equal(r.end.getDate(), 1);
});

test('rangeFromSlots timed: inverted pointers still ordered', () => {
  const mon = localDay(2024, 0, 1);
  const r = rangeFromSlots(slot(mon, 14 * 60), slot(mon, 12 * 60), 'timed');
  assert.equal(r.start.getHours(), 12);
  assert.equal(r.end.getHours(), 14);
});

test('rangeFromSlots timed: same slot → SNAP_MINUTES duration', () => {
  const mon = localDay(2024, 0, 1);
  const r = rangeFromSlots(slot(mon, 9 * 60), slot(mon, 9 * 60), 'timed');
  const durMin = (r.end.getTime() - r.start.getTime()) / 60_000;
  assert.equal(durMin, SNAP_MINUTES);
  assert.equal(r.start.getHours(), 9);
  assert.equal(r.end.getHours(), 9);
  assert.equal(r.end.getMinutes(), 15);
});

// ── rangeFromSlots allday ───────────────────────────────────────────────

test('rangeFromSlots allday: Mon–Wed → Mon 00:00 to Thu 00:00', () => {
  const mon = localDay(2024, 0, 1);
  const wed = localDay(2024, 0, 3);
  const r = rangeFromSlots(slot(mon, 0), slot(wed, 0), 'allday');
  assert.equal(r.start.getFullYear(), 2024);
  assert.equal(r.start.getMonth(), 0);
  assert.equal(r.start.getDate(), 1);
  assert.equal(r.start.getHours(), 0);
  // exclusive end = Thu Jan 4
  assert.equal(r.end.getDate(), 4);
  assert.equal(r.end.getHours(), 0);
});

test('rangeFromSlots allday: inverted days still ordered', () => {
  const mon = localDay(2024, 0, 1);
  const wed = localDay(2024, 0, 3);
  const r = rangeFromSlots(slot(wed, 0), slot(mon, 0), 'allday');
  assert.equal(r.start.getDate(), 1);
  assert.equal(r.end.getDate(), 4);
});

test('rangeFromSlots allday: single day click → next midnight exclusive', () => {
  const mon = localDay(2024, 0, 1);
  const r = rangeFromSlots(slot(mon, 0), slot(mon, 0), 'allday');
  assert.equal(r.start.getDate(), 1);
  assert.equal(r.end.getDate(), 2);
  assert.equal(r.end.getHours(), 0);
});

// ── movedRange ──────────────────────────────────────────────────────────

test('movedRange: 60-min event dropped at Tue 14:00 → 14:00–15:00', () => {
  const mon = localDay(2024, 0, 1);
  const originalStart = new Date(2024, 0, 1, 10, 0, 0, 0);
  const originalEnd = new Date(2024, 0, 1, 11, 0, 0, 0);
  const tue = localDay(2024, 0, 2);
  // default grabOffsetMin = 0 places start at the slot
  const r = movedRange(originalStart, originalEnd, slot(tue, 14 * 60));
  assert.equal(r.start.getDate(), 2);
  assert.equal(r.start.getHours(), 14);
  assert.equal(r.start.getMinutes(), 0);
  assert.equal(r.end.getDate(), 2);
  assert.equal(r.end.getHours(), 15);
  assert.equal(r.end.getMinutes(), 0);
  // silence unused
  void mon;
});

test('movedRange: grab 15 min in, drop at 14:00 → 13:45–14:45', () => {
  const originalStart = new Date(2024, 0, 1, 10, 0, 0, 0);
  const originalEnd = new Date(2024, 0, 1, 11, 0, 0, 0);
  const tue = localDay(2024, 0, 2);
  const r = movedRange(originalStart, originalEnd, slot(tue, 14 * 60), 15);
  assert.equal(r.start.getDate(), 2);
  assert.equal(r.start.getHours(), 13);
  assert.equal(r.start.getMinutes(), 45);
  assert.equal(r.end.getDate(), 2);
  assert.equal(r.end.getHours(), 14);
  assert.equal(r.end.getMinutes(), 45);
  const durMin = (r.end.getTime() - r.start.getTime()) / 60_000;
  assert.equal(durMin, 60);
});

// ── resizedRange ────────────────────────────────────────────────────────

test('resizedRange: end dragged earlier than start+15 → clamped', () => {
  const start = new Date(2024, 0, 1, 10, 0, 0, 0);
  const end = new Date(2024, 0, 1, 12, 0, 0, 0);
  const day = localDay(2024, 0, 1);
  // Drag end to 10:00 (same as start) → clamp to 10:15
  const r = resizedRange(start, end, 'end', slot(day, 10 * 60));
  assert.equal(r.start.getTime(), start.getTime());
  assert.equal(r.end.getHours(), 10);
  assert.equal(r.end.getMinutes(), 15);
});

test('resizedRange: start dragged later than end-15 → clamped', () => {
  const start = new Date(2024, 0, 1, 10, 0, 0, 0);
  const end = new Date(2024, 0, 1, 12, 0, 0, 0);
  const day = localDay(2024, 0, 1);
  // Drag start to 12:00 → clamp to 11:45
  const r = resizedRange(start, end, 'start', slot(day, 12 * 60));
  assert.equal(r.end.getTime(), end.getTime());
  assert.equal(r.start.getHours(), 11);
  assert.equal(r.start.getMinutes(), 45);
});

test('resizedRange: end extended freely', () => {
  const start = new Date(2024, 0, 1, 10, 0, 0, 0);
  const end = new Date(2024, 0, 1, 11, 0, 0, 0);
  const day = localDay(2024, 0, 1);
  const r = resizedRange(start, end, 'end', slot(day, 14 * 60));
  assert.equal(r.start.getHours(), 10);
  assert.equal(r.end.getHours(), 14);
});

test('resizedRange: start pulled earlier', () => {
  const start = new Date(2024, 0, 1, 10, 0, 0, 0);
  const end = new Date(2024, 0, 1, 11, 0, 0, 0);
  const day = localDay(2024, 0, 1);
  const r = resizedRange(start, end, 'start', slot(day, 8 * 60));
  assert.equal(r.start.getHours(), 8);
  assert.equal(r.end.getHours(), 11);
});

// ── preview geometry ────────────────────────────────────────────────────

test('timedPreviewSegments: splits overnight range across two days', () => {
  const mon = localDay(2024, 0, 1);
  const tue = localDay(2024, 0, 2);
  const days = [mon, tue];
  const preview = {
    start: new Date(2024, 0, 1, 22, 0, 0, 0),
    end: new Date(2024, 0, 2, 2, 0, 0, 0),
  };
  const segs = timedPreviewSegments(preview, 'timed', days, 60);
  assert.ok(segs.has(mon.toDateString()));
  assert.ok(segs.has(tue.toDateString()));
  // Mon: 22:00–24:00 → top = 22*60 = 1320 at hourH=60
  assert.equal(segs.get(mon.toDateString())!.top, 22 * 60);
  // Tue: 00:00–02:00 → top = 0
  assert.equal(segs.get(tue.toDateString())!.top, 0);
});

test('allDayPreviewIndices: Mon–Wed exclusive end → indices 0..2', () => {
  const mon = localDay(2024, 0, 1);
  const days = Array.from({ length: 7 }, (_, i) => addDays(mon, i));
  const preview = {
    start: mon,
    end: addDays(mon, 3), // exclusive Thu
  };
  const idx = allDayPreviewIndices(preview, 'allday', days);
  assert.deepEqual(idx, { startDay: 0, endDay: 2 });
});

test('allDayPreviewIndices: null for timed zone', () => {
  const mon = localDay(2024, 0, 1);
  const days = [mon];
  const preview = { start: mon, end: addDays(mon, 1) };
  assert.equal(allDayPreviewIndices(preview, 'timed', days), null);
});
