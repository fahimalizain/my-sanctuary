// Pure drag geometry helpers for the week grid pointer model.
// No React. Deterministic. Native Date only.

import {
  DEFAULT_EVENT_DURATION_MIN,
  MINUTES_PER_DAY,
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

/** Pointer travel of N grid-minutes moves a Cmd/Ctrl edge by N/5. */
export const PRECISION_GAIN_GRID = 5;
/** Touch gutter scrub (slice 3 will consume this): N grid-minutes → N/10. */
export const PRECISION_GAIN_SCRUB = 10;

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
   * Minutes since midnight in [0, 1440], hit-tested on the gesture's live step
   * (`snapStepMinutes`), so create, move, and resize share one coarse grid.
   * Precision (Cmd/Ctrl) does not re-snap it: it moves an edge by the scaled
   * `snappedMinuteDelta` instead of a finer absolute grid.
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

/** Apple platform strings: "Mac" / "MacIntel", "iPhone", "iPad", "iPod". */
const APPLE_PLATFORM = /^(mac|iphone|ipad|ipod)/i;

/**
 * Whether the precision modifier (a non-touch drag moves at the reduced
 * `PRECISION_GAIN_GRID` gain) is down.
 *
 * Apple platforms use Cmd only: Ctrl there is the context-menu gesture, so a
 * Ctrl-drag must stay coarse. Everywhere else — including an unknown or empty
 * `platform` — the key is Ctrl, never Meta.
 */
export function precisionModifierDown(
  keys: { metaKey: boolean; ctrlKey: boolean },
  platform: string,
): boolean {
  return APPLE_PLATFORM.test(platform) ? keys.metaKey : keys.ctrlKey;
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
/** Narrower chips drop the delete handle: three knobs no longer fit apart. */
export const DELETE_HANDLE_MIN_CHIP_W = 36;
/** Degenerate-height chips drop it too (the box would be all outside). */
export const DELETE_HANDLE_MIN_CHIP_H = 10;

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

export interface HandleRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface TouchHandleGeometry {
  /** Hit boxes in chip-local px (origin = chip top-left). */
  start: HandleRect;
  end: HandleRect;
  move: HandleRect;
  /** Tap-only delete box; null when the chip is too small to host it. */
  remove: HandleRect | null;
  /** Knob centres in chip-local px. */
  knobs: {
    start: [number, number];
    end: [number, number];
    move: [number, number];
    remove: [number, number] | null;
  };
}

/**
 * Chip-local geometry of the four touch handles.
 * - start/end: centred on the chip's horizontal middle; each hit box reaches
 *   at most to the chip midline, so short chips push knobs outward instead of
 *   overlapping.
 * - move: left edge, vertically centred. Where it could meet the start/end
 *   boxes (short or narrow chips) it slides outside the left edge instead.
 * - remove: mirror of move on the right edge; where it could meet start/end it
 *   slides outside the right edge, and it never reaches left of move's box.
 *   Hidden (null) below DELETE_HANDLE_MIN_CHIP_W / _H so it can't crowd the
 *   move/resize knobs; those always stay.
 */
export function touchHandleGeometry(
  chipWidth: number,
  chipHeight: number,
): TouchHandleGeometry {
  const w = Number.isFinite(chipWidth) ? Math.max(0, chipWidth) : 0;
  const h = Number.isFinite(chipHeight) ? Math.max(0, chipHeight) : 0;
  const hit = TOUCH_HANDLE_HIT_PX;
  const half = hit / 2;
  const knob = TOUCH_HANDLE_KNOB_PX;
  const into = Math.min(half, h / 2);
  const shift = Math.min(half - into, knob / 2 - 4);

  const midX = w / 2;
  const start = { x: midX - half, y: into - hit, w: hit, h: hit };
  const end = { x: midX - half, y: h - into, w: hit, h: hit };

  // On tall chips the move box's vertical range clears start/end already.
  const mayMeet = h < hit * 2;
  const moveRight = mayMeet ? Math.min(18, start.x) : 18;
  const move = { x: moveRight - hit, y: h / 2 - half, w: hit, h: hit };

  // Mirror of `move`: clear of start/end when they could meet, else of move.
  const removeLeft = Math.max(w - 18, mayMeet ? start.x + hit : move.x + hit);
  const hasRemove =
    w >= DELETE_HANDLE_MIN_CHIP_W && h >= DELETE_HANDLE_MIN_CHIP_H;
  const remove = hasRemove
    ? { x: removeLeft, y: h / 2 - half, w: hit, h: hit }
    : null;

  return {
    start,
    end,
    move,
    remove,
    knobs: {
      start: [midX, -shift],
      end: [midX, h + shift],
      // Keep the knob straddling the left edge even when the hit box slid out.
      move: [Math.max(moveRight - half, -(knob / 2 - 4)), h / 2],
      remove: remove
        ? [Math.min(removeLeft + half, w + (knob / 2 - 4)), h / 2]
        : null,
    },
  };
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
 * - timed: snapped instants at `step`; equal slots → `step` minutes, so a drag
 *   that never leaves its cell still fills one cell of the zoom grid
 *   (drag-create never yields a 0-min event; only resize / inspector edits do)
 * - allday: earlier day 00:00 → later day + 1 day 00:00 (exclusive end)
 */
export function rangeFromSlots(
  a: DragSlot,
  b: DragSlot,
  mode: 'timed' | 'allday',
  step = SNAP_MINUTES,
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

  const minMin = Number.isFinite(step) && step > 0 ? step : SNAP_MINUTES;
  const tA = slotInstant(a, step);
  const tB = slotInstant(b, step);
  let start = tA.getTime() <= tB.getTime() ? tA : tB;
  let end = tA.getTime() <= tB.getTime() ? tB : tA;

  if (end.getTime() <= start.getTime()) {
    end = new Date(start.getTime() + minMin * 60_000);
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
 *
 * Absolute moves only. A precision (Cmd/Ctrl) move does not come through here:
 * it adds a scaled minute delta to the frozen ghost instead, so the edge drifts
 * off the pointer rather than being re-snapped onto a finer grid.
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
 * Resize one edge; keep the other fixed. Min duration is 0: dragging past
 * the fixed edge clamps onto it (start === end), never inverts.
 * The active edge snaps to `step` (default `SNAP_MINUTES`; may cross days).
 * The fixed edge is not re-snapped.
 */
export function resizedRange(
  originalStart: Date,
  originalEnd: Date,
  edge: 'start' | 'end',
  slot: DragSlot,
  step = SNAP_MINUTES,
): TimedRange {
  const slotTime = slotInstant(slot, step);

  if (edge === 'start') {
    const end = originalEnd;
    const start = slotTime.getTime() > end.getTime() ? end : slotTime;
    return { start, end };
  }

  const start = originalStart;
  const end = slotTime.getTime() < start.getTime() ? start : slotTime;
  return { start, end };
}

// ── precision (Cmd / Ctrl) reduced gain ─────────────────────────────────
//
// Precision is not a finer absolute grid: the edge is anchored where it was
// when the modifier came down and only a scaled pointer delta moves it, so the
// edge drifts off the pointer on purpose.

/**
 * Where the pointer was, in hours-content space: Y inside the grid's content
 * column, plus the scroller's own offset so scrolling can be subtracted out.
 */
export interface PointerAnchor {
  /** Y in hours-content space. Increases when the grid scrolls down. */
  contentY: number;
  scrollTop: number;
}

/**
 * Pointer travel (px) since `anchor`, with the scroller's own movement removed.
 * Autoscrolling changes `contentY` and `scrollTop` by the same amount, so a
 * stationary pointer adds 0 — scrolling must never nudge a precision edge.
 *
 * Deliberately not raw `clientY`: if the scroller moves on screen (layout shift,
 * sticky header) `clientY` changes with no movement inside the grid.
 * Non-finite input → 0.
 */
export function pointerTravelPx(
  anchor: PointerAnchor,
  current: PointerAnchor,
): number {
  if (
    !Number.isFinite(anchor.contentY) ||
    !Number.isFinite(anchor.scrollTop) ||
    !Number.isFinite(current.contentY) ||
    !Number.isFinite(current.scrollTop)
  ) {
    return 0;
  }
  return (
    current.contentY - anchor.contentY - (current.scrollTop - anchor.scrollTop)
  );
}

/**
 * Grid minutes the pointer traveled, divided by `gain`: the reduced gain itself.
 * `gain` is `PRECISION_GAIN_GRID` for the timed grid (1 minute per 5 minutes of
 * travel) and `PRECISION_GAIN_SCRUB` for the touch gutter scrub.
 * Non-finite travel, or a non-finite/non-positive `hourH` or `gain` → 0.
 */
export function scaledMinuteDelta(
  travelPx: number,
  hourH: number,
  gain: number,
): number {
  if (
    !Number.isFinite(travelPx) ||
    !Number.isFinite(hourH) ||
    hourH <= 0 ||
    !Number.isFinite(gain) ||
    gain <= 0
  ) {
    return 0;
  }
  return (travelPx * 60) / (hourH * gain);
}

/**
 * `scaledMinuteDelta` rounded to a whole minute, half away from zero: +0.5 → +1
 * and −0.5 → −1. `Math.round` alone is not symmetric on negatives
 * (`Math.round(-0.5) === -0`), which would let a minute of travel render as no
 * movement. 0 travel → 0, so engaging precision never jumps.
 */
export function snappedMinuteDelta(
  travelPx: number,
  hourH: number,
  gain: number,
): number {
  const scaled = scaledMinuteDelta(travelPx, hourH, gain);
  if (scaled === 0) return 0;
  return scaled < 0 ? -Math.round(-scaled) : Math.round(scaled);
}

/**
 * Whole civil days from `startOfDay(from)` to `startOfDay(to)`. Rounded ms over
 * 86400000, so a 23h or 25h DST day still counts as one day. Same day → 0,
 * `to` before `from` → negative. Invalid dates → 0.
 */
export function civilDayShift(from: Date, to: Date): number {
  const a = startOfDay(from).getTime();
  const b = startOfDay(to).getTime();
  if (!Number.isFinite(a) || !Number.isFinite(b)) return 0;
  return Math.round((b - a) / 86_400_000);
}

/**
 * Precision move: shift the whole anchor range by `deltaMin` minutes and then by
 * `dayShift` civil days. Duration is preserved. A 0/0 call returns the anchor's
 * instants unchanged (sub-minute phase kept), which is what makes the engage
 * step a no-op rather than a jump. Non-finite delta or dayShift → 0.
 */
export function precisionMove(
  anchor: TimedRange,
  deltaMin: number,
  dayShift: number,
): TimedRange {
  const delta = Number.isFinite(deltaMin) ? deltaMin : 0;
  const days = Number.isFinite(dayShift) ? dayShift : 0;
  const deltaMs = delta * 60_000;
  const start = new Date(anchor.start.getTime() + deltaMs);
  const end = new Date(anchor.end.getTime() + deltaMs);
  return days === 0
    ? { start, end }
    : { start: addDays(start, days), end: addDays(end, days) };
}

/**
 * Precision resize: move only the active edge, by `deltaMin` minutes and
 * `dayShift` civil days. The other edge is not moved and not re-snapped, and the
 * active edge clamps onto it when it passes (start === end — never inverted).
 * Non-finite delta or dayShift → 0.
 */
export function precisionResize(
  anchorStart: Date,
  anchorEnd: Date,
  edge: 'start' | 'end',
  deltaMin: number,
  dayShift: number,
): TimedRange {
  const delta = Number.isFinite(deltaMin) ? deltaMin : 0;
  const days = Number.isFinite(dayShift) ? dayShift : 0;
  const moved = (d: Date): Date => {
    const at = new Date(d.getTime() + delta * 60_000);
    return days === 0 ? at : addDays(at, days);
  };

  if (edge === 'start') {
    const start = moved(anchorStart);
    return {
      start: start.getTime() > anchorEnd.getTime() ? anchorEnd : start,
      end: anchorEnd,
    };
  }

  const end = moved(anchorEnd);
  return {
    start: anchorStart,
    end: end.getTime() < anchorStart.getTime() ? anchorStart : end,
  };
}

/**
 * Precision create: the origin slot is frozen (day and minutes) and the moving
 * edge sits on `hitDay` with `anchorCurrent.minutes + deltaMin`, clamped to the
 * civil day before the instant is built — create never spills into the next
 * column by overflowing one; the pointer has to enter that column.
 *
 * Ordering and minimum duration match `rangeFromSlots(..., 'timed', 1)`: equal
 * instants become 1 minute, never 0. No 5-minute snapping: the moving minutes
 * are exact.
 */
export function precisionCreate(
  origin: DragSlot,
  anchorCurrent: DragSlot,
  deltaMin: number,
  hitDay: Date,
): TimedRange {
  const delta = Number.isFinite(deltaMin) ? deltaMin : 0;
  const originAt = dateOnDay(
    startOfDay(origin.day),
    clampDayMinutes(origin.minutes),
  );
  const movingAt = dateOnDay(
    startOfDay(hitDay),
    clampDayMinutes(anchorCurrent.minutes + delta),
  );

  const originFirst = originAt.getTime() <= movingAt.getTime();
  const start = originFirst ? originAt : movingAt;
  const end = originFirst ? movingAt : originAt;
  return end.getTime() <= start.getTime()
    ? { start, end: new Date(start.getTime() + SNAP_MINUTES * 60_000) }
    : { start, end };
}

/** Minutes since midnight, clamped into the civil day; non-finite → 0. */
function clampDayMinutes(minutes: number): number {
  if (!Number.isFinite(minutes)) return 0;
  return Math.min(MINUTES_PER_DAY, Math.max(0, minutes));
}

/** What the drag ghost needs to label itself (kept out of DayColumn props). */
export interface DragPreviewInfo {
  range: TimedRange;
  kind: DragKind;
  pointerType: string;
}

/** True when `nextMs` sits in a different `stepMin` bucket than `prevMs`. */
export function crossedTickBoundary(
  prevMs: number,
  nextMs: number,
  stepMin: number,
): boolean {
  const stepMs = stepMin * 60_000;
  return Math.floor(prevMs / stepMs) !== Math.floor(nextMs / stepMs);
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
