// Pure drag geometry helpers for the week grid pointer model.
// No React. Deterministic. Native Date only.

import {
  DEFAULT_EVENT_DURATION_MIN,
  MINUTES_PER_DAY,
  RESIZE_SNAP_MINUTES,
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
export const AUTOSCROLL_EDGE_PX = 48;
export const AUTOSCROLL_MAX_PX = 18;
export const ALLDAY_CREATE_HOLD_MS = 220;

export type DragZone = 'timed' | 'allday';

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
  /**
   * Minutes since midnight in [0, 1440].
   * Create/move snap to `SNAP_MINUTES`; resize snaps to `RESIZE_SNAP_MINUTES`.
   */
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

/**
 * Tracks active touch pointers and a multi-touch latch. The latch is set when
 * 2+ touches are down and cleared only once ALL touches have lifted, so a
 * pinch can never leak into create/move/resize/tap handling.
 */
export interface TouchTracker {
  /** A primary touch means no other touch is active: drops stale ids. */
  down: (pointerId: number, pointerType: string, isPrimary?: boolean) => void;
  up: (pointerId: number) => void;
  /** Forget all touches (page hidden / window blur). */
  reset: () => void;
  /** True from the 2nd touch landing until every touch has lifted. */
  readonly latched: boolean;
  readonly count: number;
}

export function createTouchTracker(): TouchTracker {
  const ids = new Set<number>();
  let latched = false;
  return {
    down(pointerId, pointerType, isPrimary = false) {
      if (pointerType !== 'touch') return;
      if (isPrimary) {
        ids.clear();
        latched = false;
      }
      ids.add(pointerId);
      if (ids.size >= 2) latched = true;
    },
    up(pointerId) {
      if (!ids.delete(pointerId)) return;
      if (ids.size === 0) latched = false;
    },
    reset() {
      ids.clear();
      latched = false;
    },
    get latched() {
      return latched;
    },
    get count() {
      return ids.size;
    },
  };
}

export type TouchGestureIntent = 'pending' | 'scroll' | 'drag';

/** Touch target of the fat selected-chip handles (visual knob is smaller). */
export const TOUCH_HANDLE_HIT_PX = 44;
export const TOUCH_HANDLE_KNOB_PX = 20;

export type ChipHandle = 'start' | 'end' | 'move';

export interface ChipGesture {
  kind: 'move' | 'resize-start' | 'resize-end';
  /** Touch only: claim at pointerdown (no scroll-vs-drag ambiguity). */
  claimImmediately: boolean;
  /** Touch body press: a tap selects, a pan scrolls; it never moves. */
  tapOnly: boolean;
}

/**
 * What a pointerdown on a chip means.
 * - touch on a handle: that handle's action, claimed immediately
 * - touch elsewhere: tap-only (scroll natively; no body move, no edge resize)
 * - mouse/pen: explicit handle, else the 6px edge zones, else move
 */
export function resolveChipGesture(
  pointerType: string,
  handle: ChipHandle | null,
  localY: number,
  chipHeight: number,
): ChipGesture {
  const isTouch = pointerType === 'touch';
  if (isTouch && !handle) {
    return { kind: 'move', claimImmediately: false, tapOnly: true };
  }
  const edge =
    handle === 'start' || handle === 'end'
      ? handle
      : handle === 'move'
        ? null
        : resizeEdgeAt(localY, chipHeight);
  const kind =
    edge === 'start' ? 'resize-start' : edge === 'end' ? 'resize-end' : 'move';
  return { kind, claimImmediately: isTouch, tapOnly: false };
}

/**
 * Vertical geometry of the top/bottom touch handles for a chip of
 * `chipHeight`. Each hit box reaches `hitInto` px inside the chip (never past
 * the midline, so short chips push them outward instead of overlapping), and
 * the knob centre sits `knobShift` px outside the edge (clamped so it still
 * straddles the edge).
 */
export function touchHandleGeometry(chipHeight: number): {
  hitInto: number;
  knobShift: number;
} {
  const h = Number.isFinite(chipHeight) ? Math.max(0, chipHeight) : 0;
  const half = TOUCH_HANDLE_HIT_PX / 2;
  const hitInto = Math.min(half, h / 2);
  const knobShift = Math.min(half - hitInto, TOUCH_HANDLE_KNOB_PX / 2 - 4);
  return { hitInto, knobShift };
}

/**
 * Classify a touch pan before claiming the gesture.
 * Create: horizontal-dominant past threshold → scroll; else → drag.
 * Chip: any movement past threshold → drag (cancel lets flicks scroll).
 * Chip-body: any movement past threshold → scroll (tap-only press).
 */
export function classifyTouchGesture(
  dx: number,
  dy: number,
  mode: 'create' | 'chip' | 'chip-body',
  allowHorizontalDrag = false,
): TouchGestureIntent {
  if (!movedEnough(dx, dy)) return 'pending';
  // Chip body never drags on touch: any pan hands off to native scroll.
  if (mode === 'chip-body') return 'scroll';
  if (
    mode === 'create' &&
    Math.abs(dx) > Math.abs(dy) &&
    !allowHorizontalDrag
  ) {
    return 'scroll';
  }
  return 'drag';
}

/**
 * Pixels to scroll this frame toward an edge. Negative = toward `start`.
 * Pointer past an edge still scrolls at full speed (overshoot).
 */
export function autoscrollDelta(
  pointer: number,
  start: number,
  end: number,
  edgePx = AUTOSCROLL_EDGE_PX,
  maxPx = AUTOSCROLL_MAX_PX,
): number {
  if (!(end > start) || edgePx <= 0 || maxPx <= 0) return 0;
  const distStart = pointer - start;
  const distEnd = end - pointer;
  if (distStart < edgePx && distStart <= distEnd) {
    const t = 1 - Math.max(0, distStart) / edgePx;
    return -maxPx * t;
  }
  if (distEnd < edgePx) {
    const t = 1 - Math.max(0, distEnd) / edgePx;
    return maxPx * t;
  }
  return 0;
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

function slotInstant(slot: DragSlot, step = SNAP_MINUTES): Date {
  return dateOnDay(startOfDay(slot.day), snapMinutes(slot.minutes, step));
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

/** One civil day, midnight → next midnight, on `slot.day`. */
export function toAllDayRange(slot: DragSlot): TimedRange {
  const start = startOfDay(slot.day);
  return { start, end: addDays(start, 1) };
}

/**
 * Default timed range at `slot` (same rules as click-to-create).
 * Kept inside the civil day.
 */
export function toTimedRange(slot: DragSlot): TimedRange {
  const startMin = snapMinutes(slot.minutes);
  const endMin = Math.min(
    MINUTES_PER_DAY,
    startMin + DEFAULT_EVENT_DURATION_MIN,
  );
  const adjustedStart =
    endMin - startMin < DEFAULT_EVENT_DURATION_MIN && startMin > 0
      ? Math.max(0, MINUTES_PER_DAY - DEFAULT_EVENT_DURATION_MIN)
      : startMin;
  const adjustedEnd = Math.min(
    MINUTES_PER_DAY,
    adjustedStart + DEFAULT_EVENT_DURATION_MIN,
  );
  return {
    start: dateOnDay(slot.day, adjustedStart),
    end: dateOnDay(slot.day, adjustedEnd),
  };
}

/**
 * Inclusive day offset from the event's start civil day to the grab day.
 */
export function allDayGrabOffsetDays(
  originalStart: Date,
  grabDay: Date,
): number {
  const msPerDay = 24 * 60 * 60 * 1000;
  return Math.round(
    (startOfDay(grabDay).getTime() - startOfDay(originalStart).getTime()) /
      msPerDay,
  );
}

/**
 * Shift an all-day range so the grabbed day stays under the pointer.
 * Duration is whole exclusive-end days (at least 1).
 */
export function movedAllDayRange(
  originalStart: Date,
  originalEnd: Date,
  slot: DragSlot,
  grabOffsetDays = 0,
): TimedRange {
  const start0 = startOfDay(originalStart);
  let durationDays = Math.round(
    (originalEnd.getTime() - start0.getTime()) / (24 * 60 * 60 * 1000),
  );
  if (!Number.isFinite(durationDays) || durationDays < 1) durationDays = 1;
  const offset = Number.isFinite(grabOffsetDays)
    ? Math.round(grabOffsetDays)
    : 0;
  const start = addDays(startOfDay(slot.day), -offset);
  return { start, end: addDays(start, durationDays) };
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
 * The active edge snaps to RESIZE_SNAP_MINUTES (may cross days).
 * The fixed edge is not re-snapped.
 */
export function resizedRange(
  originalStart: Date,
  originalEnd: Date,
  edge: 'start' | 'end',
  slot: DragSlot,
): TimedRange {
  const minMs = SNAP_MINUTES * 60_000;
  const slotTime = slotInstant(slot, RESIZE_SNAP_MINUTES);

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
  zone: DragZone | null,
  days: Date[],
  hourH: number,
): Map<string, { top: number; height: number }> {
  const map = new Map<string, { top: number; height: number }>();
  if (!preview || zone !== 'timed') return map;
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
  zone: DragZone | null,
  days: Date[],
): { startDay: number; endDay: number } | null {
  if (!preview || zone !== 'allday') return null;
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
