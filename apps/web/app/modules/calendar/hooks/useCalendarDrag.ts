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
  type TimedRange,
  ALLDAY_CREATE_HOLD_MS,
  allDayGrabOffsetDays,
  autoscrollDelta,
  classifyTouchGesture,
  createTouchTracker,
  crossedTickBoundary,
  isTapCreatePointer,
  movedAllDayRange,
  movedEnough,
  movedRange,
  precisionModifierDown,
  rangeFromSlots,
  resolveChipGesture,
  resizedRange,
  toAllDayRange,
  toTimedRange,
} from '../lib/calendar-drag';
import { tick } from '../lib/haptics';
import {
  dateOnDay,
  minutesFromY,
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
  preview: TimedRange | null;
  previewKind: DragKind | null;
  previewPointerType: string | null;
  previewZone: DragZone | null;
  /** Event being moved/resized (null for create gestures). */
  activeEventId: string | null;
  isDragging: boolean;
  /**
   * Precision modifier is live: a non-touch pointer is holding Cmd (macOS) /
   * Ctrl, and the gesture is on the timed grid at 1-minute snap.
   */
  precisionSnap: boolean;
  /** True after a drag commit so a synthetic click can be ignored. */
  suppressNextClick: () => boolean;
  /** Drop an in-progress gesture without committing create/move/resize. */
  cancel: () => void;
}

interface Session {
  kind: DragKind;
  /**
   * Grid step (minutes) for the gesture, hit-tested with it. Re-read from the
   * zoom and the pointer's live modifier state on every pointer and modifier
   * key event — never frozen at pointerdown, or pressing Cmd mid-drag would
   * leave the ghost on the coarse grid.
   */
  snapStep: number;
  /** Precision modifier is down (always false for touch). */
  precision: boolean;
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
   * Timed move only: painted chip start (`data-start-min`). The grab offset is
   * derived from the re-snapped origin against this on every preview, so a
   * modifier change re-snaps the hold point instead of keeping a stale error.
   */
  paintedStartMin?: number;
  /** Civil-day offset from all-day event start to the grab day. */
  grabOffsetDays?: number;
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
 * Modifier state for a gesture: touch never gets precision snap, even on a
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

/** Live grid for a session: the precision modifier and the step it implies. */
function dragGrid(
  hourH: number,
  pointerType: string,
  keys: ModifierKeys,
): { precision: boolean; snapStep: number } {
  const precision = precisionForPointer(pointerType, keys, platformName());
  return { precision, snapStep: snapStepMinutes(hourH, precision) };
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
    originZone,
    currentZone,
    originSlot,
    currentSlot,
    originalStart,
    originalEnd,
    paintedStartMin,
    grabOffsetDays,
  } = session;
  switch (kind) {
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
      // quantized to the live step instead, and the grab offset is recomputed
      // from that snapped origin every preview: pressing Cmd then re-snaps the
      // hold point onto the 1-minute grid instead of keeping the 5-minute
      // error it had a moment ago.
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
 * for resizes (the clamped commit, not the raw slot), else the current slot.
 */
function tickInstantMs(session: Session): number | null {
  if (session.kind === 'resize-start' || session.kind === 'resize-end') {
    const range = computePreview(session);
    if (!range) return null;
    return (
      session.kind === 'resize-start' ? range.start : range.end
    ).getTime();
  }
  const { day, minutes } = session.currentSlot;
  return dateOnDay(day, minutes).getTime();
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
 * Re-snap an active gesture at a point for the pointer's current modifier
 * state. Returns null when nothing moved, so a held modifier key repeat costs
 * no render.
 *
 * The origin is rebuilt from `originMinutesRaw` with the live step rather than
 * reused: a frozen 5-minute origin would leave 1-minute mode lying about where
 * the gesture started. The origin day is carried over untouched — re-reading it
 * from clientY would let autoscroll drag the anchor.
 */
function resnapSession(
  session: Session,
  x: number,
  y: number,
  keys: ModifierKeys,
  hourH: number,
): Session | null {
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

  if (
    snapStep === session.snapStep &&
    precision === session.precision &&
    currentZone === session.currentZone &&
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
   */
  const applyPoint = useCallback(
    (clientX: number, clientY: number, keys: ModifierKeys) => {
      lastKeysRef.current = keys;
      const s = sessionRef.current;
      if (!s) return;
      const updated = resnapSession(
        s,
        clientX,
        clientY,
        keys,
        optsRef.current.hourH,
      );
      // Same step, origin, and slot as before (e.g. a held key repeat).
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
    if (!claimedRef.current || !sessionRef.current) return;
    const scroller = document.querySelector('[data-calendar-scroller]');
    if (!(scroller instanceof HTMLElement)) return;
    const rect = scroller.getBoundingClientRect();
    const { x, y } = lastPtrRef.current;
    const dx = autoscrollDelta(x, rect.left, rect.right);
    const dy = autoscrollDelta(y, rect.top, rect.bottom);
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
      applyPoint(e.clientX, e.clientY, {
        metaKey: e.metaKey,
        ctrlKey: e.ctrlKey,
      });
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
     * Cmd (macOS) / Ctrl (elsewhere) pressed or released mid-drag re-snaps the
     * gesture where the pointer already is — no pointermove needed. These keys
     * are never preventDefault-ed. Escape keeps winning: it is checked first.
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

      // Live for the gesture: a Cmd / Ctrl press mid-drag re-snaps it.
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

  // The badge is only honest while a timed gesture is actually on the 1-minute
  // grid: an all-day drag (or a timed one dragged into the all-day band) is not.
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
