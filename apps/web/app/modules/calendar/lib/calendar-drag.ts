// Pure drag geometry helpers for the week grid pointer model.
// No React. Deterministic. Native Date only.

import {
  SNAP_MINUTES,
  addDays,
  clampMinutesToDay,
  dateOnDay,
  eventHeightPx,
  eventTopPx,
  lastOccupiedCivilDate,
  snapMinutes,
  startOfDay,
} from './week-layout';

export const DRAG_THRESHOLD_PX = 4;
export const RESIZE_HANDLE_PX = 6;

export type DragKind =
  | 'create'
  | 'move'
  | 'resize-start'
  | 'resize-end'
  | 'allday-create';

/** A snapped position on the timed grid (or all-day: minutes = 0). */
export interface DragSlot {
  /** Local midnight of the civil day. */
  day: Date;
  /** Minutes since midnight, snapped to SNAP_MINUTES, in [0, 1440]. */
  minutes: number;
}

export interface TimedRange {
  start: Date;
  end: Date;
}

/** True once the pointer has moved far enough to count as a drag. */
export function movedEnough(dx: number, dy: number): boolean {
  return Math.hypot(dx, dy) >= DRAG_THRESHOLD_PX;
}

/** Touch tap on empty grid still creates. Mouse/pen click does not. */
export function isTapCreatePointer(pointerType: string): boolean {
  return pointerType === 'touch';
}

export type TouchGestureIntent = 'pending' | 'scroll' | 'drag';

/**
 * Classify a touch pan before claiming the gesture.
 * Create: horizontal-dominant past threshold → scroll; else → drag.
 * Chip: any movement past threshold → drag (cancel lets flicks scroll).
 */
export function classifyTouchGesture(
  dx: number,
  dy: number,
  mode: 'create' | 'chip',
): TouchGestureIntent {
  if (!movedEnough(dx, dy)) return 'pending';
  if (mode === 'create' && Math.abs(dx) > Math.abs(dy)) return 'scroll';
  return 'drag';
}

/**
 * Which resize edge (if any) is under the pointer inside a chip.
 * `localY` is Y relative to the chip top; `chipHeight` is the chip's height.
 * When the chip is shorter than 2× handle, the mid-line splits the hit zones.
 */
export function resizeEdgeAt(
  localY: number,
  chipHeight: number,
): 'start' | 'end' | null {
  if (
    !Number.isFinite(localY) ||
    !Number.isFinite(chipHeight) ||
    chipHeight <= 0
  ) {
    return null;
  }
  const handle = RESIZE_HANDLE_PX;
  if (chipHeight < handle * 2) {
    const mid = chipHeight / 2;
    if (localY < mid) return 'start';
    return 'end';
  }
  if (localY >= 0 && localY <= handle) return 'start';
  if (localY >= chipHeight - handle && localY <= chipHeight) return 'end';
  return null;
}

function slotInstant(slot: DragSlot): Date {
  return dateOnDay(startOfDay(slot.day), snapMinutes(slot.minutes));
}

/**
 * Drag-create / all-day-create: order the two slots and enforce min duration.
 * - timed: snapped instants; equal slots → SNAP_MINUTES duration
 * - allday: earlier day 00:00 → later day + 1 day 00:00 (exclusive end)
 */
export function rangeFromSlots(
  a: DragSlot,
  b: DragSlot,
  mode: 'timed' | 'allday',
): TimedRange {
  if (mode === 'allday') {
    const dayA = startOfDay(a.day);
    const dayB = startOfDay(b.day);
    const earlier = dayA.getTime() <= dayB.getTime() ? dayA : dayB;
    const later = dayA.getTime() <= dayB.getTime() ? dayB : dayA;
    return {
      start: earlier,
      end: addDays(later, 1),
    };
  }

  const tA = slotInstant(a);
  const tB = slotInstant(b);
  let start = tA.getTime() <= tB.getTime() ? tA : tB;
  let end = tA.getTime() <= tB.getTime() ? tB : tA;

  if (end.getTime() <= start.getTime()) {
    end = new Date(start.getTime() + SNAP_MINUTES * 60_000);
  }

  return { start, end };
}

/**
 * Move: keep duration, place start so the grab point stays under the pointer.
 * `grabOffsetMin` is minutes from the (painted) event start to the pointer at
 * pointerdown; default 0 places start at the slot (legacy / no-offset callers).
 */
export function movedRange(
  originalStart: Date,
  originalEnd: Date,
  slot: DragSlot,
  grabOffsetMin = 0,
): TimedRange {
  const durationMs = Math.max(
    0,
    originalEnd.getTime() - originalStart.getTime(),
  );
  const offset = Number.isFinite(grabOffsetMin) ? grabOffsetMin : 0;
  const start = slotInstant({
    day: slot.day,
    minutes: slot.minutes - offset,
  });
  return {
    start,
    end: new Date(start.getTime() + durationMs),
  };
}

/**
 * Resize one edge; keep the other fixed. Min duration is SNAP_MINUTES.
 * The active edge is placed at the snapped slot (may cross days).
 */
export function resizedRange(
  originalStart: Date,
  originalEnd: Date,
  edge: 'start' | 'end',
  slot: DragSlot,
): TimedRange {
  const minMs = SNAP_MINUTES * 60_000;
  const slotTime = slotInstant(slot);

  if (edge === 'start') {
    const end = originalEnd;
    let start = slotTime;
    if (end.getTime() - start.getTime() < minMs) {
      start = new Date(end.getTime() - minMs);
    }
    return { start, end };
  }

  const start = originalStart;
  let end = slotTime;
  if (end.getTime() - start.getTime() < minMs) {
    end = new Date(start.getTime() + minMs);
  }
  return { start, end };
}

/** Per-day ghost geometry for a timed preview range. */
export function timedPreviewSegments(
  preview: TimedRange | null,
  kind: DragKind | null,
  days: Date[],
  hourH: number,
): Map<string, { top: number; height: number }> {
  const map = new Map<string, { top: number; height: number }>();
  if (!preview || kind === 'allday-create') return map;
  for (const day of days) {
    const clamped = clampMinutesToDay(preview.start, preview.end, day);
    if (!clamped) continue;
    map.set(day.toDateString(), {
      top: eventTopPx(clamped.startMin, hourH),
      height: eventHeightPx(clamped.startMin, clamped.endMin, hourH),
    });
  }
  return map;
}

/**
 * Inclusive day indices of an all-day preview relative to `days[0]`.
 * Returns null when out of the rendered window.
 */
export function allDayPreviewIndices(
  preview: TimedRange | null,
  kind: DragKind | null,
  days: Date[],
): { startDay: number; endDay: number } | null {
  if (!preview || kind !== 'allday-create') return null;
  const origin = days[0];
  if (!origin) return null;
  const msPerDay = 24 * 60 * 60 * 1000;
  const first = startOfDay(preview.start);
  const last = lastOccupiedCivilDate(preview.start, preview.end);
  let startDay = Math.round((first.getTime() - origin.getTime()) / msPerDay);
  let endDay = Math.round((last.getTime() - origin.getTime()) / msPerDay);
  const lastIdx = days.length - 1;
  if (endDay < 0 || startDay > lastIdx) return null;
  startDay = Math.max(0, Math.min(lastIdx, startDay));
  endDay = Math.max(0, Math.min(lastIdx, endDay));
  if (endDay < startDay) return null;
  return { startDay, endDay };
}
