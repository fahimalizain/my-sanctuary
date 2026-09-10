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
  isTapCreatePointer,
  movedAllDayRange,
  movedEnough,
  movedRange,
  rangeFromSlots,
  resizeEdgeAt,
  resizedRange,
  toAllDayRange,
  toTimedRange,
} from '../lib/calendar-drag';
import { minutesFromY, snapMinutes, startOfDay } from '../lib/week-layout';
import type { CalendarEvent } from '@/app/types';

export interface UseCalendarDragOptions {
  hourH: number;
  /** Lock the infinite strip so it does not rebase mid-drag. */
  setStripLocked: (locked: boolean) => void;
  onClickCreate: (slot: DragSlot) => void;
  onDragCreate: (range: TimedRange) => void;
  onMove: (eventId: string, range: TimedRange) => void;
  onResize: (eventId: string, range: TimedRange) => void;
  onAllDayCreate: (range: TimedRange) => void;
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
  previewZone: DragZone | null;
  /** Event being moved/resized (null for create gestures). */
  activeEventId: string | null;
  isDragging: boolean;
  /** True after a drag commit so a synthetic click can be ignored. */
  suppressNextClick: () => boolean;
}

interface Session {
  kind: DragKind;
  originSlot: DragSlot;
  currentSlot: DragSlot;
  originX: number;
  originY: number;
  eventId?: string;
  originalStart?: Date;
  originalEnd?: Date;
  /** Minutes from painted chip start to pointer at pointerdown (move only). */
  grabOffsetMin?: number;
  /** Civil-day offset from all-day event start to the grab day. */
  grabOffsetDays?: number;
  originZone: DragZone;
  currentZone: DragZone;
  originTime: number;
  pointerId: number;
  /** touch waits for classify; mouse/pen is claimed immediately. */
  pointerType: string;
}

function parseDayAttr(value: string | null | undefined): Date | null {
  if (!value) return null;
  const d = new Date(value);
  if (Number.isNaN(d.getTime())) return null;
  return startOfDay(d);
}

function slotFromColumn(
  clientY: number,
  colEl: Element,
  day: Date,
  hourH: number,
): DragSlot {
  const top = colEl.getBoundingClientRect().top;
  const y = clientY - top;
  const minutes = snapMinutes(minutesFromY(y, hourH));
  return { day: startOfDay(day), minutes };
}

function hitTestTimed(
  clientX: number,
  clientY: number,
  hourH: number,
): DragSlot | null {
  const el = document.elementFromPoint(clientX, clientY);
  if (!el) return null;
  const col = el.closest('[data-day-col]') as HTMLElement | null;
  if (!col) return null;
  const day = parseDayAttr(col.getAttribute('data-day-col'));
  if (!day) return null;
  return slotFromColumn(clientY, col, day, hourH);
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
): { slot: DragSlot; zone: DragZone } | null {
  const allDay = hitTestAllDay(clientX, clientY);
  if (allDay) return { slot: allDay, zone: 'allday' };
  const timed = hitTestTimed(clientX, clientY, hourH);
  if (timed) return { slot: timed, zone: 'timed' };
  return null;
}

function computePreview(session: Session): TimedRange | null {
  const {
    kind,
    originZone,
    currentZone,
    originSlot,
    currentSlot,
    originalStart,
    originalEnd,
    grabOffsetMin,
    grabOffsetDays,
  } = session;
  switch (kind) {
    case 'create':
      if (currentZone === 'allday') return toAllDayRange(currentSlot);
      return rangeFromSlots(originSlot, currentSlot, 'timed');
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
      return movedRange(
        originalStart,
        originalEnd,
        currentSlot,
        grabOffsetMin ?? 0,
      );
    case 'resize-start':
      if (!originalStart || !originalEnd) return null;
      return resizedRange(originalStart, originalEnd, 'start', currentSlot);
    case 'resize-end':
      if (!originalStart || !originalEnd) return null;
      return resizedRange(originalStart, originalEnd, 'end', currentSlot);
    default:
      return null;
  }
}

function nextSlotFromPoint(
  session: Session,
  clientX: number,
  clientY: number,
  hourH: number,
): { slot: DragSlot; zone: DragZone } | null {
  if (session.kind === 'allday-create') {
    const slot = hitTestAllDay(clientX, clientY);
    return slot ? { slot, zone: 'allday' } : null;
  }
  if (session.kind === 'resize-start' || session.kind === 'resize-end') {
    const slot = hitTestTimed(clientX, clientY, hourH);
    return slot ? { slot, zone: 'timed' } : null;
  }
  return hitTestAny(clientX, clientY, hourH);
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
  const autoScrollRafRef = useRef(0);

  const [session, setSession] = useState<Session | null>(null);
  const [didDrag, setDidDrag] = useState(false);

  const applyPoint = useCallback((clientX: number, clientY: number) => {
    const s = sessionRef.current;
    if (!s) return;
    const hit = nextSlotFromPoint(s, clientX, clientY, optsRef.current.hourH);
    if (!hit) return;
    const updated: Session = {
      ...s,
      currentSlot: hit.slot,
      currentZone: hit.zone,
    };
    sessionRef.current = updated;
    setSession(updated);
  }, []);

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
    if (dx !== 0 || dy !== 0) applyPoint(x, y);
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
      optsRef.current.setStripLocked(true);
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
    (next: Session, target: HTMLElement, e: ReactPointerEvent<HTMLElement>) => {
      sessionRef.current = next;
      didDragRef.current = false;
      setSession(next);
      setDidDrag(false);
      captureTargetRef.current = target;
      lastPtrRef.current = { x: e.clientX, y: e.clientY };

      const isTouch = e.pointerType === 'touch';
      if (isTouch) {
        // Leave unclaimed so native scroll on [data-calendar-scroller] works.
        claimedRef.current = false;
      } else {
        claimedRef.current = true;
        optsRef.current.setStripLocked(true);
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
    optsRef.current.setStripLocked(false);
    document.body.style.userSelect = '';
  }, [stopAutoScroll]);

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
        const mode =
          s.kind === 'create' || s.kind === 'allday-create' ? 'create' : 'chip';
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
      applyPoint(e.clientX, e.clientY);
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
            allDayCreate(rangeFromSlots(s.originSlot, s.originSlot, 'allday'));
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

    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      e.preventDefault();
      e.stopImmediatePropagation();
      endSession();
    };

    window.addEventListener('pointermove', onMoveWin, { passive: false });
    window.addEventListener('pointerup', onUpWin);
    window.addEventListener('pointercancel', onCancelWin);
    window.addEventListener('keydown', onKeyDown, true);
    return () => {
      window.removeEventListener('pointermove', onMoveWin);
      window.removeEventListener('pointerup', onUpWin);
      window.removeEventListener('pointercancel', onCancelWin);
      window.removeEventListener('keydown', onKeyDown, true);
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
      const originSlot = slotFromColumn(e.clientY, col, day, hourH);
      beginSession(
        {
          kind: 'create',
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

      const rect = chipEl.getBoundingClientRect();
      const localY = e.clientY - rect.top;
      const resizeAttr = (e.target as HTMLElement | null)
        ?.closest?.('[data-resize]')
        ?.getAttribute('data-resize');
      const edge: 'start' | 'end' | null =
        resizeAttr === 'start' || resizeAttr === 'end'
          ? resizeAttr
          : resizeEdgeAt(localY, rect.height);

      const originalStart = new Date(event.start_time);
      const originalEnd = new Date(event.end_time);

      // Origin slot: under the pointer on the containing day column.
      const col = chipEl.closest('[data-day-col]') as HTMLElement | null;
      const dayAttr = col?.getAttribute('data-day-col');
      const day = parseDayAttr(dayAttr) ?? startOfDay(originalStart);
      const originSlot = col
        ? slotFromColumn(e.clientY, col, day, hourH)
        : {
            day: startOfDay(originalStart),
            minutes: snapMinutes(
              originalStart.getHours() * 60 + originalStart.getMinutes(),
            ),
          };

      let kind: DragKind = 'move';
      if (edge === 'start') kind = 'resize-start';
      else if (edge === 'end') kind = 'resize-end';

      // Grab offset from the painted (possibly clamped overnight) chip start,
      // not the absolute event start — keeps the hold point under the pointer.
      let grabOffsetMin = 0;
      if (kind === 'move') {
        const startMinOnCol = Number(chipEl.dataset.startMin);
        if (Number.isFinite(startMinOnCol)) {
          grabOffsetMin = originSlot.minutes - startMinOnCol;
        }
      }

      beginSession(
        {
          kind,
          originSlot,
          currentSlot: originSlot,
          originX: e.clientX,
          originY: e.clientY,
          eventId: event.id,
          originalStart,
          originalEnd,
          grabOffsetMin: kind === 'move' ? grabOffsetMin : undefined,
          originZone: 'timed',
          currentZone: 'timed',
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

  const onAllDayPointerDown = useCallback(
    (e: ReactPointerEvent<HTMLElement>, day: Date) => {
      if (e.button !== 0) return;
      // Chips sit above and stopPropagation when selected.
      const target = e.target as HTMLElement | null;
      if (target?.closest('[data-allday-chip]')) return;

      const originSlot: DragSlot = { day: startOfDay(day), minutes: 0 };
      beginSession(
        {
          kind: 'allday-create',
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
    [beginSession],
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

      beginSession(
        {
          kind: 'move',
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
    [beginSession],
  );

  const suppressNextClick = useCallback(() => {
    if (!suppressClickRef.current) return false;
    suppressClickRef.current = false;
    return true;
  }, []);

  // Ghost only after the pointer has moved past the threshold — click-without-
  // drag must not flash a preview.
  const preview = session && didDrag ? computePreview(session) : null;

  return {
    onColumnPointerDown,
    onChipPointerDown,
    onAllDayPointerDown,
    onAllDayChipPointerDown,
    preview,
    previewKind: preview && session ? session.kind : null,
    previewZone: preview && session ? session.currentZone : null,
    activeEventId: session?.eventId ?? null,
    isDragging: session !== null && didDrag,
    suppressNextClick,
  };
}
