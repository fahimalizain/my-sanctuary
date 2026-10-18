// Pointer-session hook for week-grid drag create / move / resize / all-day create.
// Window-level move/up so dragging across day columns works.

import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
} from 'react';
import {
  type DragKind,
  type DragSlot,
  type DragZone,
  type PointerAnchor,
  type PointerSample,
  type TimedRange,
  ALLDAY_CREATE_HOLD_MS,
  PRECISION_GAIN_GRID,
  PRECISION_GAIN_SCRUB,
  allDayGrabOffsetDays,
  autoscrollDelta,
  civilDayShift,
  classifyTouchGesture,
  createTouchTracker,
  crossedTickBoundary,
  isTapCreatePointer,
  laggedPointer,
  movedAllDayRange,
  movedEnough,
  movedRange,
  pointerTravelPx,
  precisionCreate,
  precisionModifierDown,
  precisionMove,
  precisionResize,
  rangeFromSlots,
  resolveChipGesture,
  resizedRange,
  snappedMinuteDelta,
  toAllDayRange,
  toTimedRange,
} from '../lib/calendar-drag';
import { tick } from '../lib/haptics';
import {
  COL_HEADER_H,
  SNAP_MINUTES,
  dateOnDay,
  minutesFromY,
  minutesSinceMidnight,
  snapMinutes,
  snapStepMinutes,
  startOfDay,
  type StripLockReason,
} from '../lib/week-layout';
import type { CalendarEvent } from '@/app/types';

export interface UseCalendarDragOptions {
  hourH: number;
  /** Take / release the infinite strip's drag lock (snap off, no rebase). */
  lockStrip: (reason: StripLockReason) => void;
  unlockStrip: (reason: StripLockReason) => void;
  onClickCreate: (slot: DragSlot) => void;
  onDragCreate: (range: TimedRange) => void;
  onMove: (eventId: string, range: TimedRange) => void;
  onResize: (eventId: string, range: TimedRange) => void;
  onAllDayCreate: (range: TimedRange) => void;
  /** All-day tap without drag; defaults to `onAllDayCreate`. */
  onAllDayTapCreate?: (range: TimedRange) => void;
  /** Chip tap without drag — open inspector. */
  onChipTap: (eventId: string) => void;
  /**
   * Desktop mouse/pen empty-cell click (no drag). Used to discard an
   * unpersisted draft without deselecting a real event.
   */
  onEmptyClick?: () => void;
}

export interface CalendarDragApi {
  onColumnPointerDown: (e: ReactPointerEvent<HTMLElement>, day: Date) => void;
  onChipPointerDown: (
    e: ReactPointerEvent<HTMLElement>,
    event: CalendarEvent,
    chipEl: HTMLElement,
  ) => void;
  onAllDayPointerDown: (e: ReactPointerEvent<HTMLElement>, day: Date) => void;
  onAllDayChipPointerDown: (
    e: ReactPointerEvent<HTMLElement>,
    event: CalendarEvent,
    chipEl: HTMLElement,
  ) => void;
  /**
   * Grab a timed event's start / end pill in the gutter: the coarse chip
   * handles stay the quick drags, this is the fine one.
   */
  onGutterEdgePointerDown: (
    e: ReactPointerEvent<HTMLElement>,
    edge: 'start' | 'end',
    event: CalendarEvent,
  ) => void;
  preview: TimedRange | null;
  previewKind: DragKind | null;
  previewPointerType: string | null;
  previewZone: DragZone | null;
  /** Event being moved/resized (null for create gestures). */
  activeEventId: string | null;
  isDragging: boolean;
  /**
   * Precision modifier is live: a non-touch pointer is holding Cmd (macOS) /
   * Ctrl on the timed grid, so the drag runs at the reduced 1:5 gain.
   */
  precisionSnap: boolean;
  /** True after a drag commit so a synthetic click can be ignored. */
  suppressNextClick: () => boolean;
  /** Drop an in-progress gesture without committing create/move/resize. */
  cancel: () => void;
}

/**
 * A precision (Cmd / Ctrl) drag freezes the edge where it was when the modifier
 * came down and moves it one minute per `PRECISION_GAIN_GRID` minutes of pointer
 * travel — so the edge drifts off the pointer, which is the point. The anchor is
 * captured once, at engage.
 */
interface PrecisionHold {
  /** Content-space pointer anchor; all travel is measured against it. */
  anchor: PointerAnchor;
  /**
   * Civil day under the pointer when precision engaged. Frozen: the column shift
   * is measured from here, so a pointer that has moved two columns right shifts
   * the range two days.
   */
  pointerDay: Date;
  /**
   * Frozen base the delta is applied to: the absolute ghost at engage when a
   * drag was already running, else the original event's own edges.
   */
  range: TimedRange;
  /** What the ghost shows now: `range` + the cumulative delta / day shift. */
  preview: TimedRange;
  /** Create only: the origin slot, frozen at the engage point. */
  originSlot?: DragSlot;
  /** Create only: the moving edge's minute anchor (the pointer at engage). */
  currentSlot?: DragSlot;
}

interface Session {
  kind: DragKind;
  /**
   * Coarse grid step (minutes) for the gesture, hit-tested with it. Re-read from
   * the zoom on every pointer move — never frozen at pointerdown — but never
   * from the modifier: precision drives its own hold instead.
   */
  snapStep: number;
  /** Precision modifier is down (always false for touch). */
  precision: boolean;
  /**
   * Reduced-gain hold while the modifier is down on the timed grid; null
   * otherwise (released key, all-day zone, or a grid that cannot be measured).
   */
  precisionHold?: PrecisionHold | null;
  originSlot: DragSlot;
  currentSlot: DragSlot;
  /**
   * Unsnapped minutes at pointerdown (timed gestures). Every re-snap rebuilds
   * the origin slot from these; the origin day is never re-read from clientY,
   * because autoscroll would move it under a stationary pointer.
   */
  originMinutesRaw?: number;
  originX: number;
  originY: number;
  eventId?: string;
  originalStart?: Date;
  originalEnd?: Date;
  /**
   * Timed move only: painted chip start (`data-start-min`). The absolute grab
   * offset is that paint error against the snapped origin, recomputed on every
   * preview. A precision move ignores it: it shifts the frozen ghost instead.
   */
  paintedStartMin?: number;
  /** Civil-day offset from all-day event start to the grab day. */
  grabOffsetDays?: number;
  /**
   * Gutter scrub only: what the pills show now. Its active edge follows the
   * lagged pointer sample, never the raw one.
   */
  scrubPreview?: TimedRange;
  originZone: DragZone;
  currentZone: DragZone;
  originTime: number;
  pointerId: number;
  /** touch waits for classify; mouse/pen is claimed immediately. */
  pointerType: string;
  /** Touch chip-body press: tap selects, pan scrolls, never moves. */
  tapOnly?: boolean;
}

/** Live modifier state of the gesture, from a pointer or key event. */
interface ModifierKeys {
  metaKey: boolean;
  ctrlKey: boolean;
}

/**
 * A gutter scrub advances only on pointer move, and the sample list is the
 * gesture's own buffer: `[grab, ...moves]`, capped.
 */
interface ScrubStep {
  samples: PointerSample[];
  /** Event time of the step; `applyPoint` without a pointer event stamps now. */
  t: number;
}

/** Samples kept per scrub: enough history to cover the release lag. */
const SCRUB_MAX_SAMPLES = 40;

/** Gutter-edge scrub: a timed resize with no absolute grid and no day axis. */
function isScrubKind(kind: DragKind): boolean {
  return kind === 'scrub-start' || kind === 'scrub-end';
}

/** The edge a gesture moves; null for every kind that moves a whole range. */
function movableEdge(kind: DragKind): 'start' | 'end' | null {
  if (kind === 'resize-start' || kind === 'scrub-start') return 'start';
  if (kind === 'resize-end' || kind === 'scrub-end') return 'end';
  return null;
}

/** Keys whose press / release changes the precision modifier. */
function isModifierKey(key: string): boolean {
  return key === 'Meta' || key === 'OS' || key === 'Control';
}

/**
 * Apple layout of this device, for `precisionModifierDown`. Cached: it cannot
 * change during a session.
 *
 * `navigator.platform` is deprecated but still the only source that yields the
 * short tokens the helper reads: "MacIntel" (macOS and iPad alike), "iPhone",
 * "Win32", "Linux x86_64". A missing value stays '' and reads as Ctrl rather
 * than guessing Meta, so the modifier is never on by accident.
 */
let cachedPlatform: string | null = null;
function platformName(): string {
  if (cachedPlatform === null) {
    cachedPlatform =
      typeof navigator === 'undefined' ? '' : navigator.platform || '';
  }
  return cachedPlatform;
}

/**
 * Modifier state for a gesture: touch never gets the precision gain, even on a
 * device with a keyboard attached.
 */
function precisionForPointer(
  pointerType: string,
  keys: ModifierKeys,
  platform: string,
): boolean {
  return pointerType === 'touch'
    ? false
    : precisionModifierDown(keys, platform);
}

/**
 * Live grid for a session: whether the precision modifier is down, and the
 * coarse step the absolute path hit-tests with.
 *
 * Precision no longer changes the step. The absolute grid is the zoom's step
 * whatever the modifier says, and a precision drag is a scaled delta on a
 * frozen edge (`snappedMinuteDelta`), not a finer grid under the pointer.
 */
function dragGrid(
  hourH: number,
  pointerType: string,
  keys: ModifierKeys,
): { precision: boolean; snapStep: number } {
  const precision = precisionForPointer(pointerType, keys, platformName());
  return { precision, snapStep: snapStepMinutes(hourH) };
}

function parseDayAttr(value: string | null | undefined): Date | null {
  if (!value) return null;
  const d = new Date(value);
  if (Number.isNaN(d.getTime())) return null;
  return startOfDay(d);
}

function rawMinutesAtColumn(
  clientY: number,
  colEl: Element,
  hourH: number,
): number {
  const top = colEl.getBoundingClientRect().top;
  return minutesFromY(clientY - top, hourH);
}

function slotFromColumn(
  clientY: number,
  colEl: Element,
  day: Date,
  hourH: number,
  step: number,
): DragSlot {
  return {
    day: startOfDay(day),
    minutes: snapMinutes(rawMinutesAtColumn(clientY, colEl, hourH), step),
  };
}

function hitTestTimed(
  clientX: number,
  clientY: number,
  hourH: number,
  step: number,
): DragSlot | null {
  const el = document.elementFromPoint(clientX, clientY);
  if (!el) return null;
  const col = el.closest('[data-day-col]') as HTMLElement | null;
  if (!col) return null;
  const day = parseDayAttr(col.getAttribute('data-day-col'));
  if (!day) return null;
  return slotFromColumn(clientY, col, day, hourH, step);
}

function dayFromAllDayCell(cell: Element): Date | null {
  return parseDayAttr(cell.getAttribute('data-allday-day'));
}

function hitTestAllDay(clientX: number, clientY: number): DragSlot | null {
  const el = document.elementFromPoint(clientX, clientY);
  if (!el) return null;
  const cell = el.closest('[data-allday-day]');
  if (cell) {
    const day = dayFromAllDayCell(cell);
    if (day) return { day, minutes: 0 };
  }
  const row = el.closest('[data-allday-row]');
  if (!row) return null;
  for (const c of row.querySelectorAll('[data-allday-day]')) {
    const r = c.getBoundingClientRect();
    if (clientX >= r.left && clientX < r.right) {
      const day = dayFromAllDayCell(c);
      if (day) return { day, minutes: 0 };
    }
  }
  return null;
}

function hitTestAny(
  clientX: number,
  clientY: number,
  hourH: number,
  step: number,
): { slot: DragSlot; zone: DragZone } | null {
  const allDay = hitTestAllDay(clientX, clientY);
  if (allDay) return { slot: allDay, zone: 'allday' };
  const timed = hitTestTimed(clientX, clientY, hourH, step);
  if (timed) return { slot: timed, zone: 'timed' };
  return null;
}

function computePreview(session: Session): TimedRange | null {
  const {
    kind,
    snapStep,
    precisionHold,
    originZone,
    currentZone,
    originSlot,
    currentSlot,
    originalStart,
    originalEnd,
    paintedStartMin,
    grabOffsetDays,
    scrubPreview,
  } = session;
  // Precision owns the ghost: the hold carries the frozen edge plus the scaled
  // delta, and the absolute slots below are only the hit-test scaffolding.
  if (precisionHold && currentZone === 'timed') return precisionHold.preview;
  switch (kind) {
    case 'scrub-start':
    case 'scrub-end':
      // The pills paint the lagged edge; the absolute slots are never read.
      return scrubPreview ?? null;
    case 'create':
      if (currentZone === 'allday') return toAllDayRange(currentSlot);
      return rangeFromSlots(originSlot, currentSlot, 'timed', snapStep);
    case 'allday-create':
      return rangeFromSlots(originSlot, currentSlot, 'allday');
    case 'move':
      if (!originalStart || !originalEnd) return null;
      if (currentZone === 'allday') {
        if (originZone === 'allday') {
          return movedAllDayRange(
            originalStart,
            originalEnd,
            currentSlot,
            grabOffsetDays ?? 0,
          );
        }
        return toAllDayRange(currentSlot);
      }
      if (originZone === 'allday') return toTimedRange(currentSlot);
      // `movedRange` keeps its own 1-minute snap, so a coarse step would jump
      // an off-grid 10:07 event onto the grid on the first move. The slot is
      // quantized to the live step instead, and the grab offset is the paint
      // error the pointer grabbed with, so the chip stays under the pointer.
      // Absolute moves only: a precision move adds a scaled delta to a frozen
      // ghost and never re-derives the grab offset (see `precisionMove`).
      return movedRange(
        originalStart,
        originalEnd,
        currentSlot,
        paintedStartMin === undefined
          ? 0
          : originSlot.minutes - paintedStartMin,
      );
    case 'resize-start':
      if (!originalStart || !originalEnd) return null;
      return resizedRange(
        originalStart,
        originalEnd,
        'start',
        currentSlot,
        snapStep,
      );
    case 'resize-end':
      if (!originalStart || !originalEnd) return null;
      return resizedRange(
        originalStart,
        originalEnd,
        'end',
        currentSlot,
        snapStep,
      );
    default:
      return null;
  }
}

/**
 * The instant whose `snapStep` bucket drives haptic ticks: the dragged edge
 * for resizes and scrubs (the clamped commit, not the raw slot), else the
 * current slot.
 */
function tickInstantMs(session: Session): number | null {
  const edge = movableEdge(session.kind);
  if (edge) {
    const range = computePreview(session);
    if (!range) return null;
    return (edge === 'start' ? range.start : range.end).getTime();
  }
  const { day, minutes } = session.currentSlot;
  return dateOnDay(day, minutes).getTime();
}

/**
 * One gutter scrub step. The sample is appended, then the active edge moves by
 * the travel from the grab to the sample the lag says the user is looking at —
 * so a lift-off commits the edge that was on screen.
 *
 * The grab is the anchor, never a moving one: an autoscroll moves `contentY` and
 * `scrollTop` together, so `pointerTravelPx` subtracts it back to no travel.
 *
 * Mutates `samples` — the gesture's own buffer, not render state — keeping the
 * grab so the anchor survives the cap. Returns null when nothing the gutter
 * would paint changed, so a move inside the same minute costs no render.
 */
function stepScrub(
  session: Session,
  edge: 'start' | 'end',
  samples: PointerSample[],
  t: number,
  clientY: number,
  hourH: number,
): Session | null {
  const grab = samples[0];
  const { originalStart, originalEnd } = session;
  if (!grab || !originalStart || !originalEnd) return null;

  const measured = measurePointerAnchor(clientY);
  // No measurable grid: hold the edge still rather than jump on a layout change.
  if (!measured) return null;

  const at = Number.isFinite(t) ? t : performance.now();
  samples.push({ ...measured, t: at });
  if (samples.length > SCRUB_MAX_SAMPLES) {
    samples.splice(1, samples.length - SCRUB_MAX_SAMPLES);
  }

  const lagged = laggedPointer(samples, at) ?? grab;
  const delta = snappedMinuteDelta(
    pointerTravelPx(grab, lagged),
    hourH,
    PRECISION_GAIN_SCRUB,
  );
  // Day shift is always 0 — the gutter has no day axis, so midnight is only the
  // Date math inside `precisionResize`.
  const preview = precisionResize(originalStart, originalEnd, edge, delta, 0);
  if (session.scrubPreview && sameRange(preview, session.scrubPreview)) {
    return null;
  }
  return { ...session, scrubPreview: preview };
}

function nextSlotFromPoint(
  session: Session,
  clientX: number,
  clientY: number,
  hourH: number,
  step: number,
): { slot: DragSlot; zone: DragZone } | null {
  if (session.kind === 'allday-create') {
    const slot = hitTestAllDay(clientX, clientY);
    return slot ? { slot, zone: 'allday' } : null;
  }
  if (session.kind === 'resize-start' || session.kind === 'resize-end') {
    const slot = hitTestTimed(clientX, clientY, hourH, step);
    return slot ? { slot, zone: 'timed' } : null;
  }
  return hitTestAny(clientX, clientY, hourH, step);
}

/**
 * Pointer anchor in hours-content space — Y inside the grid's content column,
 * plus the scroller's own offset so a scroll can be subtracted out again.
 *
 * Measured from the FIRST `[data-day-col]` rather than the column under the
 * pointer: every column shares a top, and the first one is still there when the
 * pointer is over the gutter or outside the grid.
 */
function measurePointerAnchor(clientY: number): PointerAnchor | null {
  const col = document.querySelector('[data-day-col]');
  const scroller = document.querySelector('[data-calendar-scroller]');
  if (!(col instanceof HTMLElement) || !(scroller instanceof HTMLElement)) {
    return null;
  }
  return {
    contentY: clientY - col.getBoundingClientRect().top,
    scrollTop: scroller.scrollTop,
  };
}

function sameRange(a: TimedRange, b: TimedRange): boolean {
  return (
    a.start.getTime() === b.start.getTime() &&
    a.end.getTime() === b.end.getTime()
  );
}

/**
 * Anchor a precision hold at the moment the modifier came down. `ghost` is the
 * absolute range the gesture is showing (or is about to show); a null ghost
 * means there is nothing to freeze, so precision stays off.
 *
 * - Create before the drag threshold: there is no edge yet, so both slots are
 *   the pointer itself snapped to `SNAP_MINUTES` — that point is where the
 *   finger is, and the gain applies only once it moves.
 * - Move / resize before the threshold: the ORIGINAL event's own edges, not the
 *   slot under the pointer (the event is where the chip already is, so nothing
 *   jumps when the ghost appears). An all-day event dragged into the timed grid
 *   has no timed original — its ghost is the timed default — so it uses `ghost`.
 * - Already dragging: `ghost`, captured before this step's snap lands.
 */
function engagePrecisionHold(
  session: Session,
  ghost: TimedRange | null,
  anchor: PointerAnchor,
  pointerDay: Date,
  didDrag: boolean,
  hourH: number,
): PrecisionHold | null {
  if (session.kind === 'create' && !didDrag) {
    const point: DragSlot = {
      day: pointerDay,
      minutes: snapMinutes(minutesFromY(anchor.contentY, hourH), SNAP_MINUTES),
    };
    const range = precisionCreate(point, point, 0, point.day);
    return {
      anchor,
      pointerDay,
      range,
      preview: range,
      originSlot: point,
      currentSlot: point,
    };
  }

  const original =
    session.originalStart && session.originalEnd
      ? {
          start: new Date(session.originalStart.getTime()),
          end: new Date(session.originalEnd.getTime()),
        }
      : null;
  const range =
    !didDrag && session.originZone === 'timed' && original ? original : ghost;
  if (!range) return null;

  return {
    anchor,
    pointerDay,
    range,
    preview: range,
    originSlot: session.kind === 'create' ? session.originSlot : undefined,
    currentSlot: session.kind === 'create' ? session.currentSlot : undefined,
  };
}

/**
 * One precision step: pointer travel since engage, scaled down by the gain and
 * applied to the frozen anchor. `hitDay` is the civil day under the pointer — or,
 * on a miss, the last column the pointer was on, so the horizontal axis holds
 * still instead of falling back to the engage day. It stacks with the vertical
 * minute delta, because crossing midnight and crossing a column are different
 * moves.
 *
 * Returns the same hold object when the range did not change, so `stepSession`
 * can skip the render.
 */
function movePrecisionHold(
  hold: PrecisionHold,
  session: Session,
  clientY: number,
  hourH: number,
  hitDay: Date,
): PrecisionHold {
  const measured = measurePointerAnchor(clientY);
  // No measurable grid (or none at engage): travel is 0, so the edge holds
  // still instead of jumping on a layout change.
  const travel = measured ? pointerTravelPx(hold.anchor, measured) : 0;
  const deltaMin = snappedMinuteDelta(travel, hourH, PRECISION_GAIN_GRID);
  const dayShift = civilDayShift(hold.pointerDay, hitDay);

  let preview: TimedRange;
  switch (session.kind) {
    case 'move':
      preview = precisionMove(hold.range, deltaMin, dayShift);
      break;
    case 'resize-start':
      preview = precisionResize(
        hold.range.start,
        hold.range.end,
        'start',
        deltaMin,
        dayShift,
      );
      break;
    case 'resize-end':
      preview = precisionResize(
        hold.range.start,
        hold.range.end,
        'end',
        deltaMin,
        dayShift,
      );
      break;
    case 'create':
      if (!hold.originSlot || !hold.currentSlot) return hold;
      preview = precisionCreate(
        hold.originSlot,
        hold.currentSlot,
        deltaMin,
        hitDay,
      );
      break;
    default:
      // allday-create never reaches here: its zone is always all-day.
      return hold;
  }

  return sameRange(preview, hold.preview) ? hold : { ...hold, preview };
}

/**
 * Re-snap an active gesture at a point for the pointer's current modifier state.
 * Returns null when nothing a render would show changed, so a held modifier key
 * repeat, or a precision pointer wobble inside the same minute, costs no render.
 *
 * The origin is rebuilt from `originMinutesRaw` with the live zoom step rather
 * than reused, so a mid-drag zoom change cannot leave the origin lying about
 * where the gesture started. The origin day is carried over untouched —
 * re-reading it from clientY would let autoscroll drag the anchor.
 *
 * The coarse absolute grid always runs: it owns the day column, the zone, and
 * the whole ghost whenever precision is off. The precision hold is engaged from
 * the pre-snap state first, so the key going down freezes the edge the user is
 * already looking at.
 *
 * A gutter scrub leaves before any of that: it has no absolute slot and no
 * reduced-gain hold to engage or drop.
 */
function stepSession(
  session: Session,
  x: number,
  y: number,
  keys: ModifierKeys,
  hourH: number,
  didDrag: boolean,
  scrub: ScrubStep,
): Session | null {
  if (isScrubKind(session.kind)) {
    const edge = movableEdge(session.kind);
    return edge
      ? stepScrub(session, edge, scrub.samples, scrub.t, y, hourH)
      : null;
  }

  const { precision, snapStep } = dragGrid(hourH, session.pointerType, keys);

  const originMinutes = session.originMinutesRaw;
  const originSlot =
    originMinutes === undefined
      ? session.originSlot
      : {
          day: session.originSlot.day,
          minutes: snapMinutes(originMinutes, snapStep),
        };

  const hit = nextSlotFromPoint(session, x, y, hourH, snapStep);
  // A miss (pointer over the gutter, off-window, or past a sticky edge) still
  // re-snaps the current slot, so the ghost cannot keep a stale coarse minute.
  const currentSlot = hit
    ? hit.slot
    : {
        ...session.currentSlot,
        minutes: snapMinutes(session.currentSlot.minutes, snapStep),
      };
  const currentZone = hit ? hit.zone : session.currentZone;

  // Precision: anchored before the snap above is committed, and dropped the
  // moment the key is released or the gesture leaves the timed grid. The next
  // preview is then the plain absolute snap, which puts the edge back on the
  // pointer's slot — that jump is accepted, not compensated for.
  let precisionHold: PrecisionHold | null = null;
  if (precision && currentZone === 'timed') {
    let hold = session.precisionHold ?? null;
    if (!hold) {
      const anchor = measurePointerAnchor(y);
      if (anchor) {
        // What to freeze: the ghost on screen when the gesture was already on
        // the timed grid, else this step's timed preview — re-entering from the
        // all-day band, where the absolute ghost is an all-day range.
        const ghost =
          session.currentZone === 'timed'
            ? computePreview(session)
            : computePreview({
                ...session,
                originSlot,
                currentSlot,
                currentZone,
              });
        hold = engagePrecisionHold(
          session,
          ghost,
          anchor,
          hit ? hit.slot.day : session.currentSlot.day,
          didDrag,
          hourH,
        );
      }
    }
    if (hold) {
      precisionHold = movePrecisionHold(
        hold,
        session,
        y,
        hourH,
        // A miss has no column of its own: the last column the pointer was on
        // stays the horizontal anchor, so the day shift is unchanged and the
        // ghost cannot snap back to the day precision engaged on. The zone
        // stays the previous one either way, and the travel still comes from
        // clientY, so vertical precision over the gutter keeps working.
        hit ? hit.slot.day : currentSlot.day,
      );
    }
  }

  if (
    snapStep === session.snapStep &&
    precision === session.precision &&
    currentZone === session.currentZone &&
    precisionHold === (session.precisionHold ?? null) &&
    originSlot.minutes === session.originSlot.minutes &&
    currentSlot.day.getTime() === session.currentSlot.day.getTime() &&
    currentSlot.minutes === session.currentSlot.minutes
  ) {
    return null;
  }

  return {
    ...session,
    snapStep,
    precision,
    precisionHold,
    originSlot,
    currentSlot,
    currentZone,
  };
}

export function useCalendarDrag(
  options: UseCalendarDragOptions,
): CalendarDragApi {
  const { hourH } = options;

  // Keep latest callbacks/hourH in refs so window listeners stay stable.
  const optsRef = useRef(options);
  optsRef.current = options;

  const sessionRef = useRef<Session | null>(null);
  const didDragRef = useRef(false);
  const suppressClickRef = useRef(false);
  /** Touch sessions start unclaimed so the scroller can pan. */
  const claimedRef = useRef(false);
  const captureTargetRef = useRef<HTMLElement | null>(null);
  /** Gutter scrub samples, `[grab, ...moves]`; empty outside a scrub. */
  const scrubSamplesRef = useRef<PointerSample[]>([]);
  const lastPtrRef = useRef({ x: 0, y: 0 });
  /** Last modifier state read from a pointer or modifier key event. */
  const lastKeysRef = useRef<ModifierKeys>({ metaKey: false, ctrlKey: false });
  const autoScrollRafRef = useRef(0);
  /** Active touches + multi-touch latch (pinch must never start a session). */
  const touchesRef = useRef(createTouchTracker());

  const [session, setSession] = useState<Session | null>(null);
  const [didDrag, setDidDrag] = useState(false);

  /**
   * Apply a pointer position to the active gesture, re-snapping it at the
   * modifier state carried by that event. Autoscroll and modifier key events
   * come through here too, so the ghost is always on the live grid.
   *
   * `t` is the event's own timestamp; the lag is measured against it, so a
   * delayed frame does not age a sample early.
   */
  const applyPoint = useCallback(
    (clientX: number, clientY: number, keys: ModifierKeys, t?: number) => {
      lastKeysRef.current = keys;
      const s = sessionRef.current;
      if (!s) return;
      const updated = stepSession(
        s,
        clientX,
        clientY,
        keys,
        optsRef.current.hourH,
        didDragRef.current,
        { samples: scrubSamplesRef.current, t: t ?? performance.now() },
      );
      // Nothing a render would show changed (e.g. a held key repeat).
      if (!updated) return;
      if (s.pointerType === 'touch' && claimedRef.current) {
        const prevMs = tickInstantMs(s);
        const nextMs = tickInstantMs(updated);
        if (
          prevMs !== null &&
          nextMs !== null &&
          crossedTickBoundary(prevMs, nextMs, updated.snapStep)
        ) {
          tick();
        }
      }
      sessionRef.current = updated;
      setSession(updated);
    },
    [],
  );

  const stopAutoScroll = useCallback(() => {
    if (autoScrollRafRef.current) {
      cancelAnimationFrame(autoScrollRafRef.current);
      autoScrollRafRef.current = 0;
    }
  }, []);

  const tickAutoScroll = useCallback(() => {
    autoScrollRafRef.current = 0;
    const session = sessionRef.current;
    if (!claimedRef.current || !session) return;
    const scroller = document.querySelector('[data-calendar-scroller]');
    if (!(scroller instanceof HTMLElement)) return;
    const rect = scroller.getBoundingClientRect();
    const { x, y } = lastPtrRef.current;
    let dx = 0;
    let dy = 0;
    if (isScrubKind(session.kind)) {
      // A scrub has no horizontal axis: the visible top of the hours is the
      // sticky band's bottom, so that edge scrolls hours, not the header.
      const band = document.querySelector('[data-allday-row]');
      const top =
        band instanceof HTMLElement
          ? band.getBoundingClientRect().bottom
          : rect.top + COL_HEADER_H;
      dy = autoscrollDelta(y, top, rect.bottom);
    } else {
      dx = autoscrollDelta(x, rect.left, rect.right);
      dy = autoscrollDelta(y, rect.top, rect.bottom);
    }
    if (dx !== 0) scroller.scrollLeft += dx;
    if (dy !== 0) scroller.scrollTop += dy;
    if (dx !== 0 || dy !== 0) applyPoint(x, y, lastKeysRef.current);
    autoScrollRafRef.current = requestAnimationFrame(tickAutoScroll);
  }, [applyPoint]);

  const startAutoScroll = useCallback(() => {
    if (autoScrollRafRef.current) return;
    autoScrollRafRef.current = requestAnimationFrame(tickAutoScroll);
  }, [tickAutoScroll]);

  const claimGesture = useCallback(
    (pointerId: number) => {
      if (claimedRef.current) return;
      claimedRef.current = true;
      tick();
      optsRef.current.lockStrip('drag');
      document.body.style.userSelect = 'none';
      const target = captureTargetRef.current;
      if (target) {
        try {
          target.setPointerCapture(pointerId);
        } catch {
          // Capture can fail if the target is not active; window listeners still work.
        }
      }
      startAutoScroll();
    },
    [startAutoScroll],
  );

  const beginSession = useCallback(
    (
      next: Session,
      target: HTMLElement,
      e: ReactPointerEvent<HTMLElement>,
      claimImmediately = false,
    ) => {
      // Pinch / multi-touch: never start a session (React runs after capture).
      if (touchesRef.current.latched) return;
      sessionRef.current = next;
      didDragRef.current = false;
      setSession(next);
      setDidDrag(false);
      captureTargetRef.current = target;
      lastPtrRef.current = { x: e.clientX, y: e.clientY };
      // Seed the modifier state so an autoscroll tick before the first
      // pointermove does not fall back to the coarse grid.
      lastKeysRef.current = { metaKey: e.metaKey, ctrlKey: e.ctrlKey };

      const isTouch = e.pointerType === 'touch';
      if (isTouch && !claimImmediately) {
        // Leave unclaimed so native scroll on [data-calendar-scroller] works.
        claimedRef.current = false;
      } else {
        claimedRef.current = true;
        if (isTouch) tick();
        optsRef.current.lockStrip('drag');
        try {
          target.setPointerCapture(e.pointerId);
        } catch {
          // Capture can fail if the target is not active; window listeners still work.
        }
        document.body.style.userSelect = 'none';
        startAutoScroll();
      }
    },
    [startAutoScroll],
  );

  const endSession = useCallback(() => {
    stopAutoScroll();
    sessionRef.current = null;
    setSession(null);
    setDidDrag(false);
    didDragRef.current = false;
    claimedRef.current = false;
    captureTargetRef.current = null;
    optsRef.current.unlockStrip('drag');
    document.body.style.userSelect = '';
  }, [stopAutoScroll]);

  // Track touches at window capture (ahead of React's delegated handlers).
  // A 2nd finger latches multi-touch: drop any session and block new ones
  // until every finger lifts, whatever order native/React handlers run in.
  useEffect(() => {
    const touches = touchesRef.current;
    const onDown = (e: PointerEvent) => {
      touches.down(e.pointerId, e.pointerType, e.isPrimary);
      if (touches.latched && sessionRef.current) endSession();
    };
    const onEnd = (e: PointerEvent) => touches.up(e.pointerId);
    const onReset = () => touches.reset();
    const onHidden = () => {
      if (document.visibilityState === 'hidden') touches.reset();
    };
    window.addEventListener('blur', onReset);
    document.addEventListener('visibilitychange', onHidden);
    window.addEventListener('pointerdown', onDown, true);
    window.addEventListener('pointerup', onEnd, true);
    window.addEventListener('pointercancel', onEnd, true);
    return () => {
      window.removeEventListener('blur', onReset);
      document.removeEventListener('visibilitychange', onHidden);
      window.removeEventListener('pointerdown', onDown, true);
      window.removeEventListener('pointerup', onEnd, true);
      window.removeEventListener('pointercancel', onEnd, true);
    };
  }, [endSession]);

  // Window-level move / up while a session is active.
  useEffect(() => {
    if (!session) return;

    const onMoveWin = (e: PointerEvent) => {
      const s = sessionRef.current;
      if (!s) return;
      if (e.pointerId !== s.pointerId) return;

      lastPtrRef.current = { x: e.clientX, y: e.clientY };
      const dx = e.clientX - s.originX;
      const dy = e.clientY - s.originY;

      // Unclaimed touch: classify before stealing the gesture.
      if (!claimedRef.current && s.pointerType === 'touch') {
        const mode = s.tapOnly
          ? 'chip-body'
          : s.kind === 'create' || s.kind === 'allday-create'
            ? 'create'
            : 'chip';
        const held =
          s.kind === 'allday-create' &&
          performance.now() - s.originTime >= ALLDAY_CREATE_HOLD_MS;
        const intent = classifyTouchGesture(dx, dy, mode, held);
        if (intent === 'pending') return;
        if (intent === 'scroll') {
          endSession();
          return;
        }
        claimGesture(e.pointerId);
      }

      if (!didDragRef.current && movedEnough(dx, dy)) {
        didDragRef.current = true;
        setDidDrag(true);
      }

      e.preventDefault();
      applyPoint(
        e.clientX,
        e.clientY,
        {
          metaKey: e.metaKey,
          ctrlKey: e.ctrlKey,
        },
        e.timeStamp,
      );
    };

    const onUpWin = (e: PointerEvent) => {
      const s = sessionRef.current;
      if (!s) return;
      // Ignore other pointers.
      if (e.pointerId !== s.pointerId) return;

      const dragged = didDragRef.current;
      const preview = computePreview(s);
      const {
        onClickCreate: clickCreate,
        onDragCreate: dragCreate,
        onMove: moveCb,
        onResize: resizeCb,
        onAllDayCreate: allDayCreate,
        onAllDayTapCreate: allDayTapCreate,
        onChipTap: chipTap,
        onEmptyClick: emptyClick,
      } = optsRef.current;

      // Commit before clearing so callbacks see a consistent session.
      if (s.kind === 'create') {
        if (!dragged) {
          // Touch tap still creates; mouse/pen click on empty cell is a no-op
          // (except discarding an unpersisted draft via onEmptyClick).
          if (isTapCreatePointer(e.pointerType)) {
            clickCreate(s.originSlot);
          } else {
            emptyClick?.();
          }
        } else if (preview) {
          suppressClickRef.current = true;
          dragCreate(preview);
        }
      } else if (s.kind === 'allday-create') {
        if (!dragged) {
          if (isTapCreatePointer(e.pointerType)) {
            (allDayTapCreate ?? allDayCreate)(
              rangeFromSlots(s.originSlot, s.originSlot, 'allday'),
            );
          } else {
            emptyClick?.();
          }
        } else if (preview) {
          suppressClickRef.current = true;
          allDayCreate(preview);
        }
      } else if (s.kind === 'move') {
        if (!dragged) {
          if (s.eventId) chipTap(s.eventId);
        } else if (preview && s.eventId) {
          suppressClickRef.current = true;
          moveCb(s.eventId, preview);
        }
      } else if (s.kind === 'resize-start' || s.kind === 'resize-end') {
        if (!dragged) {
          if (s.eventId) chipTap(s.eventId);
        } else if (preview && s.eventId) {
          suppressClickRef.current = true;
          resizeCb(s.eventId, preview);
        }
      } else if (isScrubKind(s.kind)) {
        // The pointerup position is not a sample: the commit is the preview
        // already on screen, and a tap that never moved commits nothing.
        const original =
          s.originalStart && s.originalEnd
            ? { start: s.originalStart, end: s.originalEnd }
            : null;
        if (
          dragged &&
          preview &&
          s.eventId &&
          original &&
          !sameRange(preview, original)
        ) {
          suppressClickRef.current = true;
          resizeCb(s.eventId, preview);
        }
        // The pill does not select, so nothing consumes that flag: the synthetic
        // click lands on the pill, which has no click handler, and an unconsumed
        // flag would eat the NEXT chip tap. `click` is dispatched after this
        // `pointerup` and before the next task, so a macrotask clear still lets
        // a synthetic click on a chip see it — a microtask would run first and
        // clear it too early.
        window.setTimeout(() => {
          suppressClickRef.current = false;
        }, 0);
      }

      endSession();
    };

    const onCancelWin = (e: PointerEvent) => {
      const s = sessionRef.current;
      if (!s) return;
      if (e.pointerId !== s.pointerId) return;
      endSession();
    };

    /**
     * Cmd (macOS) / Ctrl (elsewhere) pressed or released mid-drag engages or
     * drops the reduced-gain hold where the pointer already is — no pointermove
     * needed. These keys are never preventDefault-ed. Escape keeps winning: it
     * is checked first.
     */
    const onKeyEvent = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        if (e.type === 'keydown') {
          e.preventDefault();
          e.stopImmediatePropagation();
          endSession();
        }
        return;
      }
      if (!isModifierKey(e.key)) return;
      applyPoint(lastPtrRef.current.x, lastPtrRef.current.y, {
        metaKey: e.metaKey,
        ctrlKey: e.ctrlKey,
      });
    };

    window.addEventListener('pointermove', onMoveWin, { passive: false });
    window.addEventListener('pointerup', onUpWin);
    window.addEventListener('pointercancel', onCancelWin);
    window.addEventListener('keydown', onKeyEvent, true);
    window.addEventListener('keyup', onKeyEvent, true);
    return () => {
      window.removeEventListener('pointermove', onMoveWin);
      window.removeEventListener('pointerup', onUpWin);
      window.removeEventListener('pointercancel', onCancelWin);
      window.removeEventListener('keydown', onKeyEvent, true);
      window.removeEventListener('keyup', onKeyEvent, true);
    };
  }, [session, endSession, claimGesture, applyPoint]);

  // Cleanup body style if unmounted mid-drag.
  useEffect(() => {
    return () => {
      document.body.style.userSelect = '';
    };
  }, []);

  const onColumnPointerDown = useCallback(
    (e: ReactPointerEvent<HTMLElement>, day: Date) => {
      if (e.button !== 0) return;
      // Chips handle their own pointerdown (stopPropagation).
      const target = e.target as HTMLElement | null;
      if (target?.closest('.event-chip')) return;

      const col = e.currentTarget;
      const { precision, snapStep } = dragGrid(hourH, e.pointerType, e);
      // Unsnapped origin: the modifier key can re-snap it mid-drag.
      const originMinutesRaw = rawMinutesAtColumn(e.clientY, col, hourH);
      const originSlot: DragSlot = {
        day: startOfDay(day),
        minutes: snapMinutes(originMinutesRaw, snapStep),
      };
      beginSession(
        {
          kind: 'create',
          snapStep,
          precision,
          originMinutesRaw,
          originSlot,
          currentSlot: originSlot,
          originX: e.clientX,
          originY: e.clientY,
          originZone: 'timed',
          currentZone: 'timed',
          originTime: performance.now(),
          pointerId: e.pointerId,
          pointerType: e.pointerType,
        },
        col,
        e,
      );
    },
    [hourH, beginSession],
  );

  const onChipPointerDown = useCallback(
    (
      e: ReactPointerEvent<HTMLElement>,
      event: CalendarEvent,
      chipEl: HTMLElement,
    ) => {
      if (e.button !== 0) return;
      e.stopPropagation();
      // Delete handle is tap-only: no session, no scroll claim.
      if ((e.target as HTMLElement | null)?.closest?.('[data-delete-handle]')) {
        return;
      }

      const rect = chipEl.getBoundingClientRect();
      const localY = e.clientY - rect.top;
      const handleAttr = (e.target as HTMLElement | null)
        ?.closest?.('[data-resize], [data-move-handle]')
        ?.getAttribute('data-resize');
      const isMoveHandle = Boolean(
        (e.target as HTMLElement | null)?.closest?.('[data-move-handle]'),
      );
      const handle =
        handleAttr === 'start' || handleAttr === 'end'
          ? handleAttr
          : isMoveHandle
            ? 'move'
            : null;
      const gesture = resolveChipGesture(
        e.pointerType,
        handle,
        localY,
        rect.height,
      );
      const kind: DragKind = gesture.kind;

      // Live for the gesture: a Cmd / Ctrl press mid-drag engages precision,
      // and a release drops it back to this coarse grid.
      const { precision, snapStep } = dragGrid(hourH, e.pointerType, e);

      const originalStart = new Date(event.start_time);
      const originalEnd = new Date(event.end_time);

      // Origin: under the pointer on the containing day column. The raw minutes
      // are kept so the modifier key can re-snap the origin, and the day is
      // pinned here (autoscroll must not move it).
      const col = chipEl.closest('[data-day-col]') as HTMLElement | null;
      const dayAttr = col?.getAttribute('data-day-col');
      const day = parseDayAttr(dayAttr) ?? startOfDay(originalStart);
      const originMinutesRaw = col
        ? rawMinutesAtColumn(e.clientY, col, hourH)
        : originalStart.getHours() * 60 + originalStart.getMinutes();
      const originSlot: DragSlot = {
        day,
        minutes: snapMinutes(originMinutesRaw, snapStep),
      };

      // Grab point is measured from the painted (possibly clamped overnight)
      // chip start, not the absolute event start. The offset itself is derived
      // in computePreview, so it follows the re-snapped origin.
      const chipStartMin = Number(chipEl.dataset.startMin);
      const paintedStartMin =
        kind === 'move'
          ? Number.isFinite(chipStartMin)
            ? chipStartMin
            : originMinutesRaw
          : undefined;

      beginSession(
        {
          kind,
          snapStep,
          precision,
          originMinutesRaw,
          originSlot,
          currentSlot: originSlot,
          originX: e.clientX,
          originY: e.clientY,
          eventId: event.id,
          originalStart,
          originalEnd,
          paintedStartMin,
          originZone: 'timed',
          currentZone: 'timed',
          originTime: performance.now(),
          pointerId: e.pointerId,
          pointerType: e.pointerType,
          tapOnly: gesture.tapOnly,
        },
        chipEl,
        e,
        gesture.claimImmediately,
      );
    },
    [hourH, beginSession],
  );

  /**
   * Grab the selected event's start / end pill in the gutter. Touch and mouse
   * alike claim the gesture on the first pixel (a scrub never scrolls), and both
   * run the same 1:10 gain: Cmd/Ctrl precision is a grid gesture, not this one.
   */
  const onGutterEdgePointerDown = useCallback(
    (
      e: ReactPointerEvent<HTMLElement>,
      edge: 'start' | 'end',
      event: CalendarEvent,
    ) => {
      if (e.button !== 0) return;
      e.stopPropagation();
      // A grid drag already owns the gesture (the pills are painting its own
      // preview), and a pinch must never leak into an edge resize.
      if (sessionRef.current || touchesRef.current.latched) return;

      const originalStart = new Date(event.start_time);
      const originalEnd = new Date(event.end_time);
      if (
        !Number.isFinite(originalStart.getTime()) ||
        !Number.isFinite(originalEnd.getTime())
      ) {
        return;
      }
      const anchor = measurePointerAnchor(e.clientY);
      if (!anchor) return;

      // The anchor is the event's own instant, not the pill's painted Y: a stuck
      // or pushed pill must still move the real edge.
      const originSlot: DragSlot = {
        day: startOfDay(originalStart),
        minutes: minutesSinceMidnight(
          edge === 'start' ? originalStart : originalEnd,
        ),
      };

      scrubSamplesRef.current = [{ ...anchor, t: e.timeStamp }];

      beginSession(
        {
          kind: edge === 'start' ? 'scrub-start' : 'scrub-end',
          // No coarse grid to hit-test: the gain's 1-minute steps are the grid.
          snapStep: SNAP_MINUTES,
          precision: false,
          originSlot,
          currentSlot: originSlot,
          originX: e.clientX,
          originY: e.clientY,
          eventId: event.id,
          originalStart,
          originalEnd,
          originZone: 'timed',
          currentZone: 'timed',
          originTime: performance.now(),
          pointerId: e.pointerId,
          pointerType: e.pointerType,
        },
        e.currentTarget,
        e,
        true,
      );
    },
    [beginSession],
  );

  const onAllDayPointerDown = useCallback(
    (e: ReactPointerEvent<HTMLElement>, day: Date) => {
      if (e.button !== 0) return;
      // Chips sit above and stopPropagation when selected.
      const target = e.target as HTMLElement | null;
      if (target?.closest('[data-allday-chip]')) return;

      const originSlot: DragSlot = { day: startOfDay(day), minutes: 0 };
      const { precision, snapStep } = dragGrid(hourH, e.pointerType, e);
      beginSession(
        {
          kind: 'allday-create',
          snapStep,
          precision,
          originSlot,
          currentSlot: originSlot,
          originX: e.clientX,
          originY: e.clientY,
          originZone: 'allday',
          currentZone: 'allday',
          originTime: performance.now(),
          pointerId: e.pointerId,
          pointerType: e.pointerType,
        },
        e.currentTarget,
        e,
      );
    },
    [hourH, beginSession],
  );

  const onAllDayChipPointerDown = useCallback(
    (
      e: ReactPointerEvent<HTMLElement>,
      event: CalendarEvent,
      chipEl: HTMLElement,
    ) => {
      if (e.button !== 0) return;
      e.stopPropagation();

      const originalStart = new Date(event.start_time);
      const originalEnd = new Date(event.end_time);
      const hit = hitTestAllDay(e.clientX, e.clientY);
      const day = hit?.day ?? startOfDay(originalStart);
      const originSlot: DragSlot = { day, minutes: 0 };
      const { precision, snapStep } = dragGrid(hourH, e.pointerType, e);

      beginSession(
        {
          kind: 'move',
          snapStep,
          precision,
          originSlot,
          currentSlot: originSlot,
          originX: e.clientX,
          originY: e.clientY,
          eventId: event.id,
          originalStart,
          originalEnd,
          grabOffsetDays: allDayGrabOffsetDays(originalStart, day),
          originZone: 'allday',
          currentZone: 'allday',
          originTime: performance.now(),
          pointerId: e.pointerId,
          pointerType: e.pointerType,
        },
        chipEl,
        e,
      );
    },
    [hourH, beginSession],
  );

  const suppressNextClick = useCallback(() => {
    if (!suppressClickRef.current) return false;
    suppressClickRef.current = false;
    return true;
  }, []);

  const cancel = useCallback(() => {
    if (!sessionRef.current) return;
    endSession();
  }, [endSession]);

  // Ghost only after the pointer has moved past the threshold — click-without-
  // drag must not flash a preview.
  const preview = session && didDrag ? computePreview(session) : null;

  // The badge is only honest while a timed gesture is running at the reduced
  // gain: an all-day drag (or a timed one dragged into the all-day band) is not.
  const precisionSnap =
    session !== null &&
    session.pointerType !== 'touch' &&
    session.precision &&
    session.currentZone === 'timed';

  return {
    onColumnPointerDown,
    onChipPointerDown,
    onAllDayPointerDown,
    onAllDayChipPointerDown,
    onGutterEdgePointerDown,
    preview,
    previewKind: preview && session ? session.kind : null,
    previewPointerType: preview && session ? session.pointerType : null,
    previewZone: preview && session ? session.currentZone : null,
    activeEventId: session?.eventId ?? null,
    isDragging: session !== null && didDrag,
    precisionSnap,
    suppressNextClick,
    cancel,
  };
}
