// Unit tests for calendar drag geometry helpers.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  DRAG_THRESHOLD_PX,
  RESIZE_HANDLE_PX,
  AUTOSCROLL_EDGE_PX,
  AUTOSCROLL_MAX_PX,
  PRECISION_GAIN_GRID,
  PRECISION_GAIN_SCRUB,
  SCRUB_RELEASE_LAG_MS,
  allDayGrabOffsetDays,
  allDayPreviewIndices,
  autoscrollDelta,
  civilDayShift,
  classifyTouchGesture,
  createTouchTracker,
  crossedTickBoundary,
  isTapCreatePointer,
  laggedPointer,
  pointerTravelPx,
  precisionCreate,
  precisionMove,
  precisionResize,
  resolveChipGesture,
  scaledMinuteDelta,
  scrubPreviewAt,
  snappedMinuteDelta,
  touchHandleGeometry,
  type HandleRect,
  TOUCH_HANDLE_HIT_PX,
  TOUCH_HANDLE_KNOB_PX,
  DELETE_HANDLE_MIN_CHIP_W,
  DELETE_HANDLE_MIN_CHIP_H,
  movedAllDayRange,
  movedEnough,
  movedRange,
  precisionModifierDown,
  rangeFromSlots,
  resizeEdgeAt,
  resizedRange,
  timedPreviewSegments,
  toAllDayRange,
  toTimedRange,
  type DragSlot,
  type PointerSample,
} from './calendar-drag';
import { HOUR_H_BASE, HOUR_H_MAX, SNAP_MINUTES, addDays } from './week-layout';

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

// ── precisionModifierDown ───────────────────────────────────────────────

test('precisionModifierDown: Apple platforms use Cmd, never Ctrl', () => {
  assert.equal(
    precisionModifierDown({ metaKey: true, ctrlKey: false }, 'Mac'),
    true,
  );
  // Ctrl-click opens the context menu on macOS.
  assert.equal(
    precisionModifierDown({ metaKey: false, ctrlKey: true }, 'Mac'),
    false,
  );
  // Both down is still a precision drag: Cmd is enough.
  assert.equal(
    precisionModifierDown({ metaKey: true, ctrlKey: true }, 'Mac'),
    true,
  );
});

test('precisionModifierDown: Apple platform names are case-insensitive', () => {
  for (const platform of ['MacIntel', 'mac', 'iPhone', 'iPad', 'iPod']) {
    assert.equal(
      precisionModifierDown({ metaKey: true, ctrlKey: false }, platform),
      true,
      platform,
    );
    assert.equal(
      precisionModifierDown({ metaKey: false, ctrlKey: true }, platform),
      false,
      platform,
    );
  }
});

test('precisionModifierDown: Windows / Linux use Ctrl, never Meta', () => {
  for (const platform of ['Win32', 'Linux x86_64', 'Linux armv8l']) {
    assert.equal(
      precisionModifierDown({ metaKey: false, ctrlKey: true }, platform),
      true,
      platform,
    );
    assert.equal(
      precisionModifierDown({ metaKey: true, ctrlKey: false }, platform),
      false,
      platform,
    );
  }
});

test('precisionModifierDown: unknown platform reads as Ctrl', () => {
  assert.equal(
    precisionModifierDown({ metaKey: false, ctrlKey: true }, ''),
    true,
  );
  assert.equal(
    precisionModifierDown({ metaKey: true, ctrlKey: false }, ''),
    false,
  );
  assert.equal(
    precisionModifierDown({ metaKey: false, ctrlKey: false }, 'Win32'),
    false,
  );
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
  assert.equal(r.start.getMinutes(), 0);
  assert.equal(r.end.getHours(), 9);
  assert.equal(r.end.getMinutes(), 1);
});

test('rangeFromSlots timed: equal slots on a 15-minute step fill 15 minutes', () => {
  const mon = localDay(2024, 0, 1);
  const r = rangeFromSlots(slot(mon, 9 * 60), slot(mon, 9 * 60), 'timed', 15);
  const durMin = (r.end.getTime() - r.start.getTime()) / 60_000;
  assert.equal(durMin, 15);
  assert.equal(r.start.getHours(), 9);
  assert.equal(r.start.getMinutes(), 0);
  assert.equal(r.end.getHours(), 9);
  assert.equal(r.end.getMinutes(), 15);
});

test('rangeFromSlots timed: explicit step snaps both slots', () => {
  const mon = localDay(2024, 0, 1);
  // 10:07 → 10:00 and 11:08 → 11:15 on the 15-minute grid.
  const r = rangeFromSlots(
    slot(mon, 10 * 60 + 7),
    slot(mon, 11 * 60 + 8),
    'timed',
    15,
  );
  assert.equal(r.start.getHours(), 10);
  assert.equal(r.start.getMinutes(), 0);
  assert.equal(r.end.getHours(), 11);
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

test('resizedRange: end dragged onto start → 0-min (start === end)', () => {
  const start = new Date(2024, 0, 1, 10, 0, 0, 0);
  const end = new Date(2024, 0, 1, 12, 0, 0, 0);
  const day = localDay(2024, 0, 1);
  const r = resizedRange(start, end, 'end', slot(day, 10 * 60));
  assert.equal(r.start.getTime(), start.getTime());
  assert.equal(r.end.getTime(), start.getTime());
});

test('resizedRange: end dragged past start clamps at start, never inverts', () => {
  const start = new Date(2024, 0, 1, 10, 0, 0, 0);
  const end = new Date(2024, 0, 1, 12, 0, 0, 0);
  const day = localDay(2024, 0, 1);
  const r = resizedRange(start, end, 'end', slot(day, 9 * 60));
  assert.equal(r.start.getTime(), start.getTime());
  assert.equal(r.end.getTime(), start.getTime());
});

test('resizedRange: start dragged onto end → 0-min (start === end)', () => {
  const start = new Date(2024, 0, 1, 10, 0, 0, 0);
  const end = new Date(2024, 0, 1, 12, 0, 0, 0);
  const day = localDay(2024, 0, 1);
  const r = resizedRange(start, end, 'start', slot(day, 12 * 60));
  assert.equal(r.end.getTime(), end.getTime());
  assert.equal(r.start.getTime(), end.getTime());
});

test('resizedRange: start dragged past end clamps at end, never inverts', () => {
  const start = new Date(2024, 0, 1, 10, 0, 0, 0);
  const end = new Date(2024, 0, 1, 12, 0, 0, 0);
  const day = localDay(2024, 0, 1);
  const r = resizedRange(start, end, 'start', slot(day, 13 * 60));
  assert.equal(r.end.getTime(), end.getTime());
  assert.equal(r.start.getTime(), end.getTime());
});

test('resizedRange: a 0-min event can be resized back out', () => {
  const t = new Date(2024, 0, 1, 10, 0, 0, 0);
  const day = localDay(2024, 0, 1);
  const r = resizedRange(t, t, 'end', slot(day, 10 * 60 + 5));
  assert.equal(r.end.getTime() - r.start.getTime(), 5 * 60_000);
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

test('resizedRange: end lands on a non-quarter minute', () => {
  const start = new Date(2024, 0, 1, 10, 0, 0, 0);
  const end = new Date(2024, 0, 1, 11, 0, 0, 0);
  const day = localDay(2024, 0, 1);
  // 11:07 keeps duration ≥ 15; 10:07 would clamp to start+15
  const r = resizedRange(start, end, 'end', slot(day, 11 * 60 + 7));
  assert.equal(r.start.getTime(), start.getTime());
  assert.equal(r.end.getHours(), 11);
  assert.equal(r.end.getMinutes(), 7);
});

test('resizedRange: start lands on a non-quarter minute', () => {
  const start = new Date(2024, 0, 1, 10, 0, 0, 0);
  const end = new Date(2024, 0, 1, 11, 0, 0, 0);
  const day = localDay(2024, 0, 1);
  const r = resizedRange(start, end, 'start', slot(day, 9 * 60 + 52));
  assert.equal(r.end.getTime(), end.getTime());
  assert.equal(r.start.getHours(), 9);
  assert.equal(r.start.getMinutes(), 52);
});

test('resizedRange: fixed edge is not re-snapped', () => {
  const start = new Date(2024, 0, 1, 10, 7, 0, 0);
  const end = new Date(2024, 0, 1, 11, 0, 0, 0);
  const day = localDay(2024, 0, 1);
  const r = resizedRange(start, end, 'end', slot(day, 12 * 60 + 23));
  assert.equal(r.start.getHours(), 10);
  assert.equal(r.start.getMinutes(), 7);
  assert.equal(r.end.getHours(), 12);
  assert.equal(r.end.getMinutes(), 23);
});

test('resizedRange: clamps to a non-quarter fixed edge (0-min)', () => {
  const start = new Date(2024, 0, 1, 10, 7, 0, 0);
  const end = new Date(2024, 0, 1, 11, 0, 0, 0);
  const day = localDay(2024, 0, 1);
  // Drag end to 10:03 (before start) → clamp to start = 10:07
  const r = resizedRange(start, end, 'end', slot(day, 10 * 60 + 3));
  assert.equal(r.start.getHours(), 10);
  assert.equal(r.start.getMinutes(), 7);
  assert.equal(r.end.getTime(), start.getTime());
});

test('resizedRange: overnight end snaps to 1 minute', () => {
  const start = new Date(2024, 0, 1, 22, 0, 0, 0);
  const end = new Date(2024, 0, 1, 23, 0, 0, 0);
  const tue = localDay(2024, 0, 2);
  const r = resizedRange(start, end, 'end', slot(tue, 7));
  assert.equal(r.start.getTime(), start.getTime());
  assert.equal(r.end.getDate(), 2);
  assert.equal(r.end.getHours(), 0);
  assert.equal(r.end.getMinutes(), 7);
});

test('resizedRange: fractional minutes snap nearest 1', () => {
  const start = new Date(2024, 0, 1, 10, 0, 0, 0);
  const end = new Date(2024, 0, 1, 11, 0, 0, 0);
  const day = localDay(2024, 0, 1);
  // 11:07.6 → 11:08; stays above the 15-minute floor
  const r = resizedRange(start, end, 'end', slot(day, 11 * 60 + 7.6));
  assert.equal(r.end.getHours(), 11);
  assert.equal(r.end.getMinutes(), 8);
});

test('resizedRange: active edge snaps to an explicit coarse step', () => {
  const start = new Date(2024, 0, 1, 10, 0, 0, 0);
  const end = new Date(2024, 0, 1, 11, 0, 0, 0);
  const day = localDay(2024, 0, 1);
  // 12:07 on a 15-minute step → 12:00; the fixed start is not re-snapped.
  const r = resizedRange(start, end, 'end', slot(day, 12 * 60 + 7), 15);
  assert.equal(r.start.getTime(), start.getTime());
  assert.equal(r.end.getHours(), 12);
  assert.equal(r.end.getMinutes(), 0);
});

test('resizedRange: coarse step still clamps at the fixed edge (0-min)', () => {
  const start = new Date(2024, 0, 1, 10, 7, 0, 0);
  const end = new Date(2024, 0, 1, 11, 0, 0, 0);
  const day = localDay(2024, 0, 1);
  // 10:03 snaps down to 10:00, before the 10:07 start → clamp, never invert.
  const r = resizedRange(start, end, 'end', slot(day, 10 * 60 + 3), 15);
  assert.equal(r.start.getTime(), start.getTime());
  assert.equal(r.end.getTime(), start.getTime());
});

test('movedRange: snaps to the 1-minute grid', () => {
  const originalStart = new Date(2024, 0, 1, 9, 0, 0, 0);
  const originalEnd = new Date(2024, 0, 1, 10, 0, 0, 0);
  const day = localDay(2024, 0, 1);
  // 10:07 is already on the 1-minute grid and keeps its duration.
  const r = movedRange(originalStart, originalEnd, slot(day, 10 * 60 + 7));
  assert.equal(r.start.getHours(), 10);
  assert.equal(r.start.getMinutes(), 7);
  assert.equal(r.end.getHours(), 11);
  assert.equal(r.end.getMinutes(), 7);
});

test('movedRange: fractional minutes snap to the nearest minute', () => {
  const originalStart = new Date(2024, 0, 1, 9, 0, 0, 0);
  const originalEnd = new Date(2024, 0, 1, 10, 0, 0, 0);
  const day = localDay(2024, 0, 1);
  // 10:07.6 → 10:08, duration still 60 min.
  const r = movedRange(originalStart, originalEnd, slot(day, 10 * 60 + 7.6));
  assert.equal(r.start.getHours(), 10);
  assert.equal(r.start.getMinutes(), 8);
  assert.equal(r.end.getHours(), 11);
  assert.equal(r.end.getMinutes(), 8);
  const durMin = (r.end.getTime() - r.start.getTime()) / 60_000;
  assert.equal(durMin, 60);
});

test('rangeFromSlots timed: snaps to the 1-minute grid', () => {
  const day = localDay(2024, 0, 1);
  const r = rangeFromSlots(
    slot(day, 10 * 60 + 7),
    slot(day, 11 * 60 + 7),
    'timed',
  );
  assert.equal(r.start.getHours(), 10);
  assert.equal(r.start.getMinutes(), 7);
  assert.equal(r.end.getHours(), 11);
  assert.equal(r.end.getMinutes(), 7);
});

test('movedRange: coarse slots keep the minute phase of an off-grid event', () => {
  const originalStart = new Date(2024, 0, 1, 10, 7, 0, 0);
  const originalEnd = new Date(2024, 0, 1, 11, 7, 0, 0);
  const day = localDay(2024, 0, 1);
  // Grabbed at 10:07 on a 15-minute grid: the slot rounds to 10:00, so the
  // grab offset is −7. Advancing one cell (10:15) keeps the :07 phase.
  const held = movedRange(originalStart, originalEnd, slot(day, 10 * 60), -7);
  assert.equal(held.start.getHours(), 10);
  assert.equal(held.start.getMinutes(), 7);
  const moved = movedRange(
    originalStart,
    originalEnd,
    slot(day, 10 * 60 + 15),
    -7,
  );
  assert.equal(moved.start.getHours(), 10);
  assert.equal(moved.start.getMinutes(), 22);
  const durMin = (moved.end.getTime() - moved.start.getTime()) / 60_000;
  assert.equal(durMin, 60);
});

// ── precision gain (Cmd / Ctrl) ─────────────────────────────────────────

test('precision gains: 1:5 for the grid, 1:10 for the gutter scrub', () => {
  assert.equal(PRECISION_GAIN_GRID, 5);
  assert.equal(PRECISION_GAIN_SCRUB, 10);
  assert.equal(SCRUB_RELEASE_LAG_MS, 80);
});

test('snappedMinuteDelta: the scrub locks one minute to 8px at hourH 48', () => {
  // 10 grid-minutes of travel at hourH 48 is 8px; at the scrub gain that is a
  // whole minute either way, which is what makes a finger land on one.
  assert.equal(snappedMinuteDelta(8, HOUR_H_BASE, PRECISION_GAIN_SCRUB), 1);
  assert.equal(snappedMinuteDelta(-8, HOUR_H_BASE, PRECISION_GAIN_SCRUB), -1);
});

test('pointerTravelPx: scrolling alone is not travel', () => {
  // Autoscroll: the finger is still, the grid moves under it. Content Y and
  // scrollTop move together, so the edge must not move at all.
  assert.equal(
    pointerTravelPx(
      { contentY: 10, scrollTop: 0 },
      { contentY: 25, scrollTop: 15 },
    ),
    0,
  );
});

test('pointerTravelPx: pointer travel with a still scroller', () => {
  assert.equal(
    pointerTravelPx(
      { contentY: 10, scrollTop: 4 },
      { contentY: 18, scrollTop: 4 },
    ),
    8,
  );
});

test('pointerTravelPx: travel and scroll at once cancel only the scroll', () => {
  // 27px down the content minus a 15px scroll → 12px of real travel.
  assert.equal(
    pointerTravelPx(
      { contentY: 10, scrollTop: 0 },
      { contentY: 37, scrollTop: 15 },
    ),
    12,
  );
});

test('pointerTravelPx: non-finite input → 0', () => {
  const anchor = { contentY: 10, scrollTop: 0 };
  assert.equal(pointerTravelPx(anchor, { contentY: NaN, scrollTop: 0 }), 0);
  assert.equal(
    pointerTravelPx(anchor, {
      contentY: 12,
      scrollTop: Number.POSITIVE_INFINITY,
    }),
    0,
  );
  assert.equal(
    pointerTravelPx(
      { contentY: NaN, scrollTop: 0 },
      { contentY: 12, scrollTop: 0 },
    ),
    0,
  );
  assert.equal(
    pointerTravelPx(anchor, { contentY: 0, scrollTop: Number.NaN }),
    0,
  );
});

test('scaledMinuteDelta: 5 grid-minutes of travel is one minute at gain 5', () => {
  // hourH 48 → one grid-minute is 0.8px, so 5 grid-minutes is 4px.
  assert.equal(scaledMinuteDelta(4, HOUR_H_BASE, PRECISION_GAIN_GRID), 1);
  // The scrub gain is the same travel at half the minutes (slice 3).
  assert.equal(scaledMinuteDelta(4, HOUR_H_BASE, PRECISION_GAIN_SCRUB), 0.5);
});

test('scaledMinuteDelta: max zoom, 5 grid-minutes of travel is exactly 1 minute', () => {
  const travel = (5 * HOUR_H_MAX) / 60;
  assert.equal(scaledMinuteDelta(travel, HOUR_H_MAX, PRECISION_GAIN_GRID), 1);
});

test('scaledMinuteDelta: unusable travel, hourH or gain → 0', () => {
  assert.equal(scaledMinuteDelta(NaN, HOUR_H_BASE, PRECISION_GAIN_GRID), 0);
  assert.equal(
    scaledMinuteDelta(
      Number.POSITIVE_INFINITY,
      HOUR_H_BASE,
      PRECISION_GAIN_GRID,
    ),
    0,
  );
  assert.equal(scaledMinuteDelta(4, NaN, PRECISION_GAIN_GRID), 0);
  assert.equal(scaledMinuteDelta(4, 0, PRECISION_GAIN_GRID), 0);
  assert.equal(scaledMinuteDelta(4, -HOUR_H_BASE, PRECISION_GAIN_GRID), 0);
  assert.equal(scaledMinuteDelta(4, HOUR_H_BASE, NaN), 0);
  assert.equal(scaledMinuteDelta(4, HOUR_H_BASE, 0), 0);
  assert.equal(scaledMinuteDelta(4, HOUR_H_BASE, -PRECISION_GAIN_GRID), 0);
});

test('snappedMinuteDelta: rounds to the nearest minute, half away from zero', () => {
  assert.equal(snappedMinuteDelta(4, HOUR_H_BASE, PRECISION_GAIN_GRID), 1);
  // Exactly half a minute of travel rounds away from zero, both ways.
  assert.equal(snappedMinuteDelta(2, HOUR_H_BASE, PRECISION_GAIN_GRID), 1);
  assert.equal(snappedMinuteDelta(-2, HOUR_H_BASE, PRECISION_GAIN_GRID), -1);
  assert.equal(snappedMinuteDelta(0, HOUR_H_BASE, PRECISION_GAIN_GRID), 0);
  // Under half a minute stays put: engaging precision must not jump.
  assert.equal(snappedMinuteDelta(1, HOUR_H_BASE, PRECISION_GAIN_GRID), 0);
  // `Math.round(-0.5)` returns -0; a delta of nothing is +0.
  assert.ok(
    Object.is(snappedMinuteDelta(-0, HOUR_H_BASE, PRECISION_GAIN_GRID), 0),
  );
});

test('civilDayShift: whole civil days, not elapsed hours', () => {
  assert.equal(
    civilDayShift(new Date(2024, 0, 1, 23, 30), new Date(2024, 0, 2, 0, 30)),
    1,
  );
  // Same civil day, 23 hours apart.
  assert.equal(
    civilDayShift(new Date(2024, 0, 1, 0, 30), new Date(2024, 0, 1, 23, 30)),
    0,
  );
  // `to` before `from` is negative.
  assert.equal(
    civilDayShift(new Date(2024, 0, 5, 12, 0), new Date(2024, 0, 2, 12, 0)),
    -3,
  );
});

test('civilDayShift: a DST-length day is still one civil day', () => {
  // 2024-03-10 is 23h in US zones and 24h elsewhere: one day either way.
  assert.equal(
    civilDayShift(new Date(2024, 2, 10, 12, 0), new Date(2024, 2, 11, 12, 0)),
    1,
  );
  assert.equal(
    civilDayShift(new Date(2024, 10, 3, 12, 0), new Date(2024, 10, 4, 12, 0)),
    1,
  );
});

test('civilDayShift: invalid dates → 0', () => {
  assert.equal(civilDayShift(new Date(NaN), new Date(2024, 0, 2)), 0);
  assert.equal(civilDayShift(new Date(2024, 0, 1), new Date(NaN)), 0);
});

test('precisionMove: 0/0 keeps the anchor instants, phase included', () => {
  const start = new Date(2024, 0, 1, 10, 7, 30);
  const end = new Date(2024, 0, 1, 11, 7, 30);
  const r = precisionMove({ start, end }, 0, 0);
  assert.equal(r.start.getTime(), start.getTime());
  assert.equal(r.end.getTime(), end.getTime());
});

test('precisionMove: minutes and civil days are different axes', () => {
  const start = new Date(2024, 0, 1, 23, 0, 0); // Monday 23:00
  const end = new Date(2024, 0, 2, 1, 0, 0); // overnight → 01:00 Tuesday
  // A long minute delta crosses midnight by itself: 23:00 + 120 → 01:00.
  const late = precisionMove({ start, end }, 120, 0);
  assert.equal(late.start.getDate(), 2);
  assert.equal(late.start.getHours(), 1);
  // A column shift alone keeps the clock time.
  const nextDay = precisionMove({ start, end }, 0, 1);
  assert.equal(nextDay.start.getDate(), 2);
  assert.equal(nextDay.start.getHours(), 23);
  // Both apply at once: Tuesday 01:00 + one civil day.
  const both = precisionMove({ start, end }, 120, 1);
  assert.equal(both.start.getDate(), 3);
  assert.equal(both.start.getHours(), 1);
  assert.equal(
    both.end.getTime() - both.start.getTime(),
    end.getTime() - start.getTime(),
  );
});

test('precisionMove: duration is preserved in both directions', () => {
  const start = new Date(2024, 0, 1, 9, 0, 0);
  const end = new Date(2024, 0, 1, 9, 45, 0);
  for (const [deltaMin, dayShift] of [
    [7, 0],
    [-7, 0],
    [0, 2],
    [-90, -1],
  ]) {
    const r = precisionMove({ start, end }, deltaMin, dayShift);
    assert.equal(
      r.end.getTime() - r.start.getTime(),
      45 * 60_000,
      `${deltaMin}/${dayShift}`,
    );
  }
});

test('precisionMove: non-finite delta or dayShift counts as 0', () => {
  const start = new Date(2024, 0, 1, 10, 0, 0);
  const end = new Date(2024, 0, 1, 11, 0, 0);
  const nanDelta = precisionMove({ start, end }, NaN, 1);
  assert.equal(
    nanDelta.start.getTime(),
    new Date(2024, 0, 2, 10, 0, 0).getTime(),
  );
  const nanDays = precisionMove({ start, end }, 30, NaN);
  assert.equal(
    nanDays.start.getTime(),
    new Date(2024, 0, 1, 10, 30, 0).getTime(),
  );
});

test('precisionResize: end +1 minute keeps the start untouched', () => {
  const start = new Date(2024, 0, 1, 10, 7, 0);
  const end = new Date(2024, 0, 1, 11, 0, 0);
  const r = precisionResize(start, end, 'end', 1, 0);
  assert.equal(r.start.getTime(), start.getTime());
  assert.equal(r.end.getTime(), end.getTime() + 60_000);
});

test('precisionResize: the fixed edge is neither moved nor re-snapped', () => {
  const start = new Date(2024, 0, 1, 10, 7, 0);
  const end = new Date(2024, 0, 1, 12, 23, 0);
  const r = precisionResize(start, end, 'end', 5, 0);
  assert.equal(r.start.getTime(), start.getTime());
  assert.equal(r.end.getTime(), end.getTime() + 5 * 60_000);
});

test('precisionResize: an edge dragged past the fixed one clamps, never inverts', () => {
  const start = new Date(2024, 0, 1, 10, 0, 0);
  const end = new Date(2024, 0, 1, 10, 5, 0);
  const past = precisionResize(start, end, 'end', -600, 0);
  assert.equal(past.start.getTime(), start.getTime());
  assert.equal(past.end.getTime(), start.getTime());
  const over = precisionResize(start, end, 'start', 600, 0);
  assert.equal(over.start.getTime(), end.getTime());
  assert.equal(over.end.getTime(), end.getTime());
});

test('precisionResize: a civil-day shift moves only the active edge', () => {
  const short = new Date(2024, 0, 1, 10, 0, 0);
  const shortEnd = new Date(2024, 0, 1, 11, 0, 0);
  // The end moves a day forward on its own; the start does not follow.
  const later = precisionResize(short, shortEnd, 'end', 0, 1);
  assert.equal(later.start.getTime(), short.getTime());
  assert.equal(later.end.getDate(), 2);
  assert.equal(later.end.getHours(), 11);

  // The start needs an event longer than a day to have room to move a day on.
  const start = new Date(2024, 0, 1, 10, 0, 0);
  const end = new Date(2024, 0, 3, 11, 0, 0);
  const shifted = precisionResize(start, end, 'start', 0, 1);
  assert.equal(shifted.start.getDate(), 2);
  assert.equal(shifted.start.getHours(), 10);
  assert.equal(shifted.start.getMinutes(), 0);
  assert.equal(shifted.end.getTime(), end.getTime());
});

test('precisionResize: a civil-day shift past the fixed edge clamps', () => {
  const start = new Date(2024, 0, 1, 10, 0, 0);
  const end = new Date(2024, 0, 1, 11, 0, 0);
  // One day forward is far past a one-hour event: clamp, do not invert.
  const r = precisionResize(start, end, 'start', 0, 1);
  assert.equal(r.start.getTime(), end.getTime());
  assert.equal(r.end.getTime(), end.getTime());
});

test('precisionResize: a scrub-backed delta walks the active edge past midnight', () => {
  // Monday 23:00 + 120 minutes → Tuesday 01:00, with no day axis involved.
  const start = new Date(2024, 0, 1, 23, 0, 0);
  const end = new Date(2024, 0, 2, 2, 0, 0);
  const later = precisionResize(start, end, 'start', 120, 0);
  assert.equal(later.start.getDate(), 2);
  assert.equal(later.start.getHours(), 1);
  assert.equal(later.end.getTime(), end.getTime());

  const rather = precisionResize(start, end, 'end', 120, 0);
  assert.equal(rather.end.getDate(), 2);
  assert.equal(rather.end.getHours(), 4);
  assert.equal(rather.start.getTime(), start.getTime());
});

test('precisionResize: 0/0 keeps the anchor instants', () => {
  const start = new Date(2024, 0, 1, 10, 7, 15);
  const end = new Date(2024, 0, 1, 11, 7, 15);
  const r = precisionResize(start, end, 'start', 0, 0);
  assert.equal(r.start.getTime(), start.getTime());
  assert.equal(r.end.getTime(), end.getTime());
});

test('precisionResize: non-finite delta or dayShift counts as 0', () => {
  const start = new Date(2024, 0, 1, 10, 0, 0);
  const end = new Date(2024, 0, 1, 11, 0, 0);
  const nanDelta = precisionResize(start, end, 'end', NaN, 1);
  assert.equal(
    nanDelta.end.getTime(),
    new Date(2024, 0, 2, 11, 0, 0).getTime(),
  );
  const nanDays = precisionResize(start, end, 'end', 5, NaN);
  assert.equal(nanDays.end.getTime(), new Date(2024, 0, 1, 11, 5, 0).getTime());
});

test('precisionCreate: equal instants are 1 minute, not 0', () => {
  const mon = localDay(2024, 0, 1);
  const r = precisionCreate(slot(mon, 10 * 60), slot(mon, 10 * 60), 0, mon);
  assert.equal(r.start.getHours(), 10);
  assert.equal(r.start.getMinutes(), 0);
  assert.equal(r.end.getTime() - r.start.getTime(), SNAP_MINUTES * 60_000);
});

test('precisionCreate: the moving edge takes the minute delta exactly', () => {
  const mon = localDay(2024, 0, 1);
  const r = precisionCreate(slot(mon, 10 * 60), slot(mon, 10 * 60), 3, mon);
  assert.equal(r.start.getHours(), 10);
  assert.equal(r.start.getMinutes(), 0);
  assert.equal(r.end.getHours(), 10);
  assert.equal(r.end.getMinutes(), 3);
});

test('precisionCreate: off-grid minutes are not snapped to 5', () => {
  const mon = localDay(2024, 0, 1);
  const r = precisionCreate(
    slot(mon, 10 * 60 + 2),
    slot(mon, 10 * 60 + 2),
    1,
    mon,
  );
  assert.equal(r.start.getMinutes(), 2);
  assert.equal(r.end.getMinutes(), 3);
});

test('precisionCreate: entering the next column moves the edge, not the origin', () => {
  const mon = localDay(2024, 0, 1);
  const tue = localDay(2024, 0, 2);
  const r = precisionCreate(slot(mon, 10 * 60), slot(mon, 10 * 60), 0, tue);
  assert.equal(r.start.getTime(), new Date(2024, 0, 1, 10, 0, 0, 0).getTime());
  assert.equal(r.end.getTime(), new Date(2024, 0, 2, 10, 0, 0, 0).getTime());
});

test('precisionCreate: an inverted drag still orders the range', () => {
  const mon = localDay(2024, 0, 1);
  const r = precisionCreate(slot(mon, 14 * 60), slot(mon, 10 * 60), 0, mon);
  assert.equal(r.start.getHours(), 10);
  assert.equal(r.end.getHours(), 14);
});

test('precisionCreate: the moving edge cannot overflow past midnight', () => {
  const mon = localDay(2024, 0, 1);
  // 23:00 + 300 minutes would spill into Tuesday: it clamps at the day's end
  // instead. Create only reaches the next column by the pointer going there.
  const r = precisionCreate(slot(mon, 22 * 60), slot(mon, 23 * 60), 300, mon);
  assert.equal(r.start.getTime(), new Date(2024, 0, 1, 22, 0, 0, 0).getTime());
  assert.equal(r.end.getTime(), new Date(2024, 0, 2, 0, 0, 0, 0).getTime());
});

// ── gutter scrub release lag ────────────────────────────────────────────

function sampleAt(t: number): PointerSample {
  return { contentY: t, scrollTop: 0, t };
}

test('laggedPointer: no samples → null', () => {
  assert.equal(laggedPointer([], 100, 80), null);
});

test('laggedPointer: a sample still inside the lag is not shown yet', () => {
  const grab = sampleAt(0);
  const moved = sampleAt(50);
  // t=50 is only 50ms old at now=100, so the grab is what the user sees.
  assert.equal(laggedPointer([grab, moved], 100, 80), grab);
});

test('laggedPointer: the newest sample clear of the lag wins', () => {
  const grab = sampleAt(0);
  const moved = sampleAt(100);
  assert.equal(laggedPointer([grab, moved], 180, 80), moved);
});

test('laggedPointer: a sample exactly `lagMs` old qualifies', () => {
  const grab = sampleAt(0);
  const at = sampleAt(20);
  // now - lag = 20, so the sample on the boundary is old enough.
  assert.equal(laggedPointer([grab, at], 100, 80), at);
});

test('laggedPointer: a gesture younger than the lag shows its only sample', () => {
  const only = sampleAt(90);
  assert.equal(laggedPointer([only], 100, 80), only);
});

test('laggedPointer: a non-finite now → null', () => {
  assert.equal(laggedPointer([sampleAt(0)], Number.NaN, 80), null);
  assert.equal(
    laggedPointer([sampleAt(0)], Number.POSITIVE_INFINITY, 80),
    null,
  );
});

test('laggedPointer: the default lag is the scrub release lag', () => {
  const grab = sampleAt(0);
  const moved = sampleAt(100);
  assert.equal(laggedPointer([grab, moved], 180), moved);
  assert.equal(laggedPointer([grab, moved], 100), grab);
});

// ── gutter scrub preview at an arbitrary time ───────────────────────────

// hourH 48 at the scrub gain: 8px of travel is a whole minute, so 16px is two.
const scrubGrab: PointerSample = { contentY: 0, scrollTop: 0, t: 0 };
const scrubStart = new Date(2024, 0, 1, 9, 0, 0, 0);
const scrubEnd = new Date(2024, 0, 1, 10, 0, 0, 0);

test('scrubPreviewAt: no samples → null', () => {
  assert.equal(
    scrubPreviewAt({
      samples: [],
      now: 1000,
      hourH: HOUR_H_BASE,
      edge: 'end',
      start: scrubStart,
      end: scrubEnd,
    }),
    null,
  );
});

test('scrubPreviewAt: a non-finite now → null', () => {
  assert.equal(
    scrubPreviewAt({
      samples: [scrubGrab],
      now: Number.NaN,
      hourH: HOUR_H_BASE,
      edge: 'end',
      start: scrubStart,
      end: scrubEnd,
    }),
    null,
  );
});

test('scrubPreviewAt: the resting sample is painted only once it is past the lag', () => {
  // The finger moved 2 minutes and stopped. The lag decides whose travel the
  // edge takes, which is why a lift-off inside the lag keeps the grab's edge.
  const resting: PointerSample = { contentY: 16, scrollTop: 0, t: 1000 };
  const at = (now: number) =>
    scrubPreviewAt({
      samples: [scrubGrab, resting],
      now,
      hourH: HOUR_H_BASE,
      edge: 'end',
      start: scrubStart,
      end: scrubEnd,
    });

  // 0ms old: the grab is what is painted.
  assert.equal(at(1000)?.end.getTime(), scrubEnd.getTime());
  // 40ms old: still inside the lag, so a lift here must not take it either.
  assert.equal(at(1040)?.end.getTime(), scrubEnd.getTime());
  // Idle past the lag: the settle converges the edge onto the resting finger.
  const settled = at(1080);
  assert.equal(
    settled?.end.getTime(),
    new Date(2024, 0, 1, 10, 2, 0, 0).getTime(),
  );
  assert.equal(settled?.start.getTime(), scrubStart.getTime());
});

test('scrubPreviewAt: a resting sample that only scrolled is not travel', () => {
  // contentY and scrollTop both +16: the grid moved under a still finger, so the
  // edge must not move at all.
  const rested: PointerSample = { contentY: 16, scrollTop: 16, t: 1000 };
  const r = scrubPreviewAt({
    samples: [scrubGrab, rested],
    now: 1080,
    hourH: HOUR_H_BASE,
    edge: 'end',
    start: scrubStart,
    end: scrubEnd,
  });
  assert.equal(r?.end.getTime(), scrubEnd.getTime());
});

test('scrubPreviewAt: an old enough start-edge travel moves the start back a minute', () => {
  // −8px of travel that is past the lag: one minute earlier. The range is wide
  // enough (9:00–11:00) that the moving start cannot invert past its end.
  const moved: PointerSample = { contentY: -8, scrollTop: 0, t: 1000 };
  const start = new Date(2024, 0, 1, 9, 0, 0, 0);
  const end = new Date(2024, 0, 1, 11, 0, 0, 0);
  const r = scrubPreviewAt({
    samples: [scrubGrab, moved],
    now: 1080,
    hourH: HOUR_H_BASE,
    edge: 'start',
    start,
    end,
  });
  assert.equal(r?.start.getTime(), new Date(2024, 0, 1, 8, 59, 0, 0).getTime());
  assert.equal(r?.end.getTime(), end.getTime());
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

test('touch tracker: single touch never latches', () => {
  const t = createTouchTracker();
  t.down(1, 'touch');
  assert.equal(t.latched, false);
  t.up(1);
  assert.equal(t.count, 0);
  assert.equal(t.latched, false);
});

test('touch tracker: 2nd touch latches until ALL lift', () => {
  const t = createTouchTracker();
  t.down(1, 'touch');
  t.down(2, 'touch');
  assert.equal(t.latched, true);
  t.up(1);
  assert.equal(t.latched, true); // one finger still down
  t.up(2);
  assert.equal(t.latched, false);
});

test('touch tracker: new touch after pinch while one finger remains stays latched', () => {
  const t = createTouchTracker();
  t.down(1, 'touch');
  t.down(2, 'touch');
  t.up(2);
  t.down(3, 'touch');
  assert.equal(t.latched, true);
  t.up(1);
  t.up(3);
  assert.equal(t.latched, false);
});

test('touch tracker: mouse/pen pointers are ignored', () => {
  const t = createTouchTracker();
  t.down(1, 'touch');
  t.down(2, 'mouse');
  t.down(3, 'pen');
  assert.equal(t.latched, false);
  assert.equal(t.count, 1);
  t.up(2); // unknown id: no-op
  assert.equal(t.count, 1);
});

test('touch tracker: fresh touch after a full pinch is not latched', () => {
  const t = createTouchTracker();
  t.down(1, 'touch');
  t.down(2, 'touch');
  t.up(1);
  t.up(2);
  t.down(5, 'touch');
  assert.equal(t.latched, false);
});

test('touch tracker: stale id then new primary touch is not latched', () => {
  const t = createTouchTracker();
  t.down(1, 'touch', true);
  // pointerup for 1 missed
  t.down(7, 'touch', true);
  assert.equal(t.latched, false);
  assert.equal(t.count, 1);
});

test('touch tracker: non-primary second touch still latches', () => {
  const t = createTouchTracker();
  t.down(1, 'touch', true);
  t.down(2, 'touch', false);
  assert.equal(t.latched, true);
});

test('touch tracker: reset clears ids and latch', () => {
  const t = createTouchTracker();
  t.down(1, 'touch', true);
  t.down(2, 'touch', false);
  t.reset();
  assert.equal(t.latched, false);
  assert.equal(t.count, 0);
});

test('classifyTouchGesture: chip-body pans hand off to scroll', () => {
  assert.equal(classifyTouchGesture(0, 0, 'chip-body'), 'pending');
  assert.equal(
    classifyTouchGesture(0, DRAG_THRESHOLD_PX, 'chip-body'),
    'scroll',
  );
  assert.equal(
    classifyTouchGesture(DRAG_THRESHOLD_PX, 0, 'chip-body'),
    'scroll',
  );
});

test('resolveChipGesture: touch body is tap-only, even on an edge', () => {
  const g = resolveChipGesture('touch', null, 1, 60);
  assert.deepEqual(g, { kind: 'move', claimImmediately: false, tapOnly: true });
});

test('resolveChipGesture: touch handles claim immediately', () => {
  assert.deepEqual(resolveChipGesture('touch', 'start', 30, 60), {
    kind: 'resize-start',
    claimImmediately: true,
    tapOnly: false,
  });
  assert.equal(resolveChipGesture('touch', 'end', 30, 60).kind, 'resize-end');
  assert.deepEqual(resolveChipGesture('touch', 'move', 1, 60), {
    kind: 'move',
    claimImmediately: true,
    tapOnly: false,
  });
});

test('resolveChipGesture: mouse/pen keep edge zones and body move', () => {
  for (const type of ['mouse', 'pen']) {
    assert.equal(resolveChipGesture(type, null, 2, 60).kind, 'resize-start');
    assert.equal(resolveChipGesture(type, null, 58, 60).kind, 'resize-end');
    assert.deepEqual(resolveChipGesture(type, null, 30, 60), {
      kind: 'move',
      claimImmediately: false,
      tapOnly: false,
    });
    assert.equal(resolveChipGesture(type, 'end', 30, 60).kind, 'resize-end');
  }
});

function intersects(a: HandleRect, b: HandleRect): boolean {
  return (
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
  );
}

test('touchHandleGeometry: no hit-box overlap, knobs belong to the chip', () => {
  const half = TOUCH_HANDLE_KNOB_PX / 2;
  for (const w of [45, 60, 110, 200]) {
    for (const h of [15, 30, 45, 90]) {
      const g = touchHandleGeometry(w, h);
      const label = `w=${w} h=${h}`;
      assert.ok(!intersects(g.start, g.end), `start/end ${label}`);
      assert.ok(!intersects(g.start, g.move), `start/move ${label}`);
      assert.ok(!intersects(g.end, g.move), `end/move ${label}`);
      if (g.remove) {
        assert.ok(!intersects(g.remove, g.start), `remove/start ${label}`);
        assert.ok(!intersects(g.remove, g.end), `remove/end ${label}`);
        assert.ok(!intersects(g.remove, g.move), `remove/move ${label}`);
      }
      const chip = { x: 0, y: 0, w, h };
      for (const [name, knobCentre] of Object.entries(g.knobs)) {
        if (!knobCentre) continue;
        const [cx, cy] = knobCentre;
        const knob = { x: cx - half, y: cy - half, w: half * 2, h: half * 2 };
        assert.ok(intersects(knob, chip), `${name} knob touches chip ${label}`);
        assert.ok(cx >= -half && cx <= w + half, `${name} knob x ${label}`);
      }
      // Top/bottom knobs sit at the horizontal middle of the chip.
      assert.equal(g.knobs.start[0], w / 2);
      assert.equal(g.knobs.end[0], w / 2);
      // Each knob centre is inside its own hit box.
      const inside = (r: HandleRect, [x, y]: [number, number]) =>
        x >= r.x && x <= r.x + r.w && y >= r.y && y <= r.y + r.h;
      assert.ok(inside(g.start, g.knobs.start), `start knob in box ${label}`);
      assert.ok(inside(g.end, g.knobs.end), `end knob in box ${label}`);
      assert.ok(inside(g.move, g.knobs.move), `move knob in box ${label}`);
      if (g.remove && g.knobs.remove) {
        assert.ok(
          inside(g.remove, g.knobs.remove),
          `remove knob in box ${label}`,
        );
        // Knobs stay visually distinct (no stacking on each other).
        for (const other of [g.knobs.start, g.knobs.end, g.knobs.move]) {
          const d = Math.hypot(
            g.knobs.remove[0] - other[0],
            g.knobs.remove[1] - other[1],
          );
          assert.ok(d >= TOUCH_HANDLE_KNOB_PX, `remove knob spacing ${label}`);
        }
      }
    }
  }
});

test('touchHandleGeometry: tall wide chips straddle the edge symmetrically', () => {
  const g = touchHandleGeometry(200, 200);
  assert.equal(g.start.y + g.start.h, TOUCH_HANDLE_HIT_PX / 2);
  assert.equal(Math.abs(g.knobs.start[1]), 0);
  assert.equal(g.move.x + g.move.w, 18);
});

test('touchHandleGeometry: delete mirrors move on the right of roomy chips', () => {
  const g = touchHandleGeometry(200, 200);
  assert.ok(g.remove && g.knobs.remove);
  assert.equal(g.remove.x, 200 - 18);
  assert.equal(g.remove.y, g.move.y);
  assert.equal(g.knobs.remove[1], 100);
});

test('touchHandleGeometry: delete never overlaps on phone-narrow columns', () => {
  // 7/5/3-day phone columns, tall and short chips.
  for (const w of [DELETE_HANDLE_MIN_CHIP_W, 40, 46, 70, 110]) {
    for (const h of [10, 20, 44, 87, 88, 120]) {
      const g = touchHandleGeometry(w, h);
      assert.ok(g.remove, `delete shown w=${w} h=${h}`);
      for (const other of [g.start, g.end, g.move]) {
        assert.ok(!intersects(g.remove, other), `w=${w} h=${h}`);
      }
    }
  }
});

test('touchHandleGeometry: delete hides on tiny chips, move/resize stay', () => {
  for (const [w, h] of [
    [DELETE_HANDLE_MIN_CHIP_W - 1, 60],
    [100, DELETE_HANDLE_MIN_CHIP_H - 1],
    [0, 0],
  ]) {
    const g = touchHandleGeometry(w, h);
    assert.equal(g.remove, null);
    assert.equal(g.knobs.remove, null);
    assert.ok(g.start && g.end && g.move);
  }
});

test('touchHandleGeometry: bad input is safe', () => {
  const g = touchHandleGeometry(NaN, NaN);
  assert.ok(Number.isFinite(g.start.x) && Number.isFinite(g.move.y));
});

// ── haptic tick boundaries ──────────────────────────────────────────────

test('crossedTickBoundary: same 15-min bucket does not tick', () => {
  const base = Date.UTC(2026, 0, 5, 10, 0);
  assert.equal(crossedTickBoundary(base, base, 15), false);
  assert.equal(
    crossedTickBoundary(base + 60_000, base + 14 * 60_000, 15),
    false,
  );
});

test('crossedTickBoundary: crossing a boundary ticks in either direction', () => {
  const base = Date.UTC(2026, 0, 5, 10, 0);
  assert.equal(
    crossedTickBoundary(base + 14 * 60_000, base + 15 * 60_000, 15),
    true,
  );
  assert.equal(
    crossedTickBoundary(base + 15 * 60_000, base + 14 * 60_000, 15),
    true,
  );
  assert.equal(crossedTickBoundary(base, base + 45 * 60_000, 15), true);
});

test('crossedTickBoundary: works before the epoch', () => {
  assert.equal(crossedTickBoundary(-60_000, 60_000, 15), true);
  assert.equal(crossedTickBoundary(-120_000, -60_000, 15), false);
});

test('crossedTickBoundary: ticks on the gesture step', () => {
  const base = Date.UTC(2026, 0, 5, 10, 0);
  // 5-minute step: a step-sized move ticks, a sub-step wobble does not.
  assert.equal(crossedTickBoundary(base + 60_000, base + 4 * 60_000, 5), false);
  assert.equal(
    crossedTickBoundary(base + 4 * 60_000, base + 5 * 60_000, 5),
    true,
  );
  // 1-minute step at max zoom: every minute is its own bucket.
  assert.equal(crossedTickBoundary(base, base + 60_000, 1), true);
  assert.equal(crossedTickBoundary(base, base + 60_000, 30), false);
});
