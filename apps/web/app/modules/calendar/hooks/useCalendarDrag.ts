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
  type TimedRange,
  classifyTouchGesture,
  isTapCreatePointer,
  movedEnough,
  movedRange,
  rangeFromSlots,
  resizeEdgeAt,
  resizedRange,
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
  preview: TimedRange | null;
  previewKind: DragKind | null;
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

function hitTestAllDay(clientX: number, clientY: number): DragSlot | null {
  const el = document.elementFromPoint(clientX, clientY);
  if (!el) return null;
  const cell = el.closest('[data-allday-day]') as HTMLElement | null;
  if (!cell) return null;
  const day = parseDayAttr(cell.getAttribute('data-allday-day'));
  if (!day) return null;
  return { day, minutes: 0 };
}

function computePreview(session: Session): TimedRange | null {
  const {
    kind,
    originSlot,
    currentSlot,
    originalStart,
    originalEnd,
    grabOffsetMin,
  } = session;
  switch (kind) {
    case 'create':
      return rangeFromSlots(originSlot, currentSlot, 'timed');
    case 'allday-create':
      return rangeFromSlots(originSlot, currentSlot, 'allday');
    case 'move':
      if (!originalStart || !originalEnd) return null;
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

  const [session, setSession] = useState<Session | null>(null);
  const [didDrag, setDidDrag] = useState(false);

  const claimGesture = useCallback((pointerId: number) => {
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
  }, []);

  const beginSession = useCallback(
    (next: Session, target: HTMLElement, e: ReactPointerEvent<HTMLElement>) => {
      sessionRef.current = next;
      didDragRef.current = false;
      setSession(next);
      setDidDrag(false);
      captureTargetRef.current = target;

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
      }
    },
    [],
  );

  const endSession = useCallback(() => {
    sessionRef.current = null;
    setSession(null);
    setDidDrag(false);
    didDragRef.current = false;
    claimedRef.current = false;
    captureTargetRef.current = null;
    optsRef.current.setStripLocked(false);
    document.body.style.userSelect = '';
  }, []);

  // Window-level move / up while a session is active.
  useEffect(() => {
    if (!session) return;

    const onMoveWin = (e: PointerEvent) => {
      const s = sessionRef.current;
      if (!s) return;
      if (e.pointerId !== s.pointerId) return;

      const dx = e.clientX - s.originX;
      const dy = e.clientY - s.originY;

      // Unclaimed touch: classify before stealing the gesture.
      if (!claimedRef.current && s.pointerType === 'touch') {
        const mode =
          s.kind === 'create' || s.kind === 'allday-create' ? 'create' : 'chip';
        const intent = classifyTouchGesture(dx, dy, mode);
        if (intent === 'pending') return;
        if (intent === 'scroll') {
          endSession();
          return;
        }
        // 'drag' — claim now, then fall through into the move path.
        claimGesture(e.pointerId);
      }

      if (!didDragRef.current && movedEnough(dx, dy)) {
        didDragRef.current = true;
        setDidDrag(true);
      }

      e.preventDefault();

      let nextSlot: DragSlot | null = null;
      if (s.kind === 'allday-create') {
        nextSlot = hitTestAllDay(e.clientX, e.clientY);
      } else {
        nextSlot = hitTestTimed(e.clientX, e.clientY, optsRef.current.hourH);
      }

      if (!nextSlot) return; // keep last slot on miss

      const updated: Session = { ...s, currentSlot: nextSlot };
      sessionRef.current = updated;
      setSession(updated);
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

    window.addEventListener('pointermove', onMoveWin, { passive: false });
    window.addEventListener('pointerup', onUpWin);
    window.addEventListener('pointercancel', onCancelWin);
    return () => {
      window.removeEventListener('pointermove', onMoveWin);
      window.removeEventListener('pointerup', onUpWin);
      window.removeEventListener('pointercancel', onCancelWin);
    };
  }, [session, endSession, claimGesture]);

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
          pointerId: e.pointerId,
          pointerType: e.pointerType,
        },
        e.currentTarget,
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
    preview,
    previewKind: preview && session ? session.kind : null,
    activeEventId: session?.eventId ?? null,
    isDragging: session !== null && didDrag,
    suppressNextClick,
  };
}
