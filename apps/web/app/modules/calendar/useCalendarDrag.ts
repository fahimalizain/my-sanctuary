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
  movedEnough,
  movedRange,
  rangeFromSlots,
  resizeEdgeAt,
  resizedRange,
} from './calendar-drag';
import {
  minutesFromY,
  snapMinutes,
  startOfDay,
} from './week-layout';
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
}

export interface CalendarDragApi {
  onColumnPointerDown: (
    e: ReactPointerEvent<HTMLElement>,
    day: Date,
  ) => void;
  onChipPointerDown: (
    e: ReactPointerEvent<HTMLElement>,
    event: CalendarEvent,
    chipEl: HTMLElement,
  ) => void;
  onAllDayPointerDown: (
    e: ReactPointerEvent<HTMLElement>,
    day: Date,
  ) => void;
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
  pointerId: number;
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

function hitTestTimed(clientX: number, clientY: number, hourH: number): DragSlot | null {
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
  const { kind, originSlot, currentSlot, originalStart, originalEnd } = session;
  switch (kind) {
    case 'create':
      return rangeFromSlots(originSlot, currentSlot, 'timed');
    case 'allday-create':
      return rangeFromSlots(originSlot, currentSlot, 'allday');
    case 'move':
      if (!originalStart || !originalEnd) return null;
      return movedRange(originalStart, originalEnd, currentSlot);
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

  const [session, setSession] = useState<Session | null>(null);
  const [didDrag, setDidDrag] = useState(false);

  const beginSession = useCallback(
    (
      next: Session,
      target: HTMLElement,
      e: ReactPointerEvent<HTMLElement>,
    ) => {
      sessionRef.current = next;
      didDragRef.current = false;
      setSession(next);
      setDidDrag(false);
      optsRef.current.setStripLocked(true);
      try {
        target.setPointerCapture(e.pointerId);
      } catch {
        // Capture can fail if the target is not active; window listeners still work.
      }
      // Avoid text selection while the gesture is live.
      document.body.style.userSelect = 'none';
    },
    [],
  );

  const endSession = useCallback(() => {
    sessionRef.current = null;
    setSession(null);
    setDidDrag(false);
    didDragRef.current = false;
    optsRef.current.setStripLocked(false);
    document.body.style.userSelect = '';
  }, []);

  // Window-level move / up while a session is active.
  useEffect(() => {
    if (!session) return;

    const onMoveWin = (e: PointerEvent) => {
      const s = sessionRef.current;
      if (!s) return;

      const dx = e.clientX - s.originX;
      const dy = e.clientY - s.originY;
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
      } = optsRef.current;

      // Commit before clearing so callbacks see a consistent session.
      if (s.kind === 'create') {
        if (!dragged) {
          clickCreate(s.originSlot);
        } else if (preview) {
          suppressClickRef.current = true;
          dragCreate(preview);
        }
      } else if (s.kind === 'allday-create') {
        if (!dragged) {
          allDayCreate(rangeFromSlots(s.originSlot, s.originSlot, 'allday'));
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

    window.addEventListener('pointermove', onMoveWin, { passive: false });
    window.addEventListener('pointerup', onUpWin);
    window.addEventListener('pointercancel', onUpWin);
    return () => {
      window.removeEventListener('pointermove', onMoveWin);
      window.removeEventListener('pointerup', onUpWin);
      window.removeEventListener('pointercancel', onUpWin);
    };
  }, [session, endSession]);

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
          pointerId: e.pointerId,
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
