import {
  useCallback,
  useEffect,
  useMemo,
  useState,
} from 'react';
import {
  upsertCalendarEventInCache,
  useCalendarEventsQuery,
  useCalendarsQuery,
  useCreateCalendarEvent,
  useDeleteCalendarEvent,
  useUpdateCalendarEvent,
} from '@/app/queries/calendar';
import type { CalendarEvent } from '@/app/types';
import { useCalendarEventsQueue } from '../events-context';
import {
  allDayPreviewIndices,
  timedPreviewSegments,
  type TimedRange,
} from '../lib/calendar-drag';
import {
  buildAllDayChips,
  buildEventsByDay,
  clickCreateTimesFromSlot,
  eventChipColor,
} from '../lib/calendar-model';
import { useCalendarDrag } from './useCalendarDrag';
import {
  COL_HEADER_H,
  colorForCalendar,
  defaultWritableCalendar,
  hourGridBackground,
  hourHeight as computeHourHeight,
  isMultiDay,
  isSameDay,
} from '../lib/week-layout';

export interface CalendarSessionInput {
  timeMin: string;
  timeMax: string;
  days: Date[];
  dayCount: number;
  scrollerHeight: number;
  setStripLocked: (locked: boolean) => void;
}

/**
 * Events query, calendar filter, inspector, mutations, drag, packing, and
 * now-tick for the calendar page. Owns everything the strip does not.
 */
export function useCalendarSession({
  timeMin,
  timeMax,
  days,
  dayCount,
  scrollerHeight,
  setStripLocked,
}: CalendarSessionInput) {
  const eventsQuery = useCalendarEventsQuery(timeMin, timeMax);
  const events = eventsQuery.data?.events ?? [];
  const isLoading = eventsQuery.isLoading;
  const isRefreshing = eventsQuery.isFetching && !eventsQuery.isLoading;
  const error =
    eventsQuery.error instanceof Error
      ? eventsQuery.error.message
      : eventsQuery.error
        ? 'Failed to load events'
        : null;
  const retry = () => {
    void eventsQuery.refetch();
  };

  // Optimistic move/resize overlay (read-time; not setQueryData).
  const queue = useCalendarEventsQueue();

  // Calendars for sidebar list + visibility filter.
  const calendarsQuery = useCalendarsQuery();
  const calendars = calendarsQuery.data?.calendars ?? [];
  const calendarsError =
    calendarsQuery.error instanceof Error
      ? calendarsQuery.error.message
      : calendarsQuery.error
        ? 'Failed to load calendars'
        : null;

  const [selectedCalendarIds, setSelectedCalendarIds] = useState<Set<string>>(
    () => new Set(),
  );
  // Distinguishes pre-init (empty Set = show all) from user unchecking every
  // calendar (empty Set = show none). Flipped once calendars first arrive.
  const [selectionReady, setSelectionReady] = useState(false);

  // Event inspector selection (chip click or after click-to-create).
  const [selectedEventId, setSelectedEventId] = useState<string | null>(null);
  // Focus title only right after click-to-create, not on every chip select.
  const [focusTitleOnOpen, setFocusTitleOnOpen] = useState(false);
  // Hold the just-created event until the list query includes it (inspector
  // opens immediately; chip appears after invalidate).
  const [pendingEvent, setPendingEvent] = useState<CalendarEvent | null>(null);

  const createEvent = useCreateCalendarEvent();
  const updateEvent = useUpdateCalendarEvent();
  const deleteEvent = useDeleteCalendarEvent();

  const writableCalendar = useMemo(
    () => defaultWritableCalendar(calendars),
    [calendars],
  );

  // Default: select every calendar once the list first arrives.
  useEffect(() => {
    if (calendars.length === 0 || selectionReady) return;
    setSelectedCalendarIds(new Set(calendars.map((c) => c.id)));
    setSelectionReady(true);
  }, [calendars, selectionReady]);

  // Overlay first so inspector shows optimistic move/resize times.
  const overlaidEvents = useMemo(
    () => queue.apply(events),
    [queue, events],
  );

  const selectedEvent = useMemo(() => {
    if (!selectedEventId) return null;
    const fromList = overlaidEvents.find((e) => e.id === selectedEventId);
    if (fromList) return fromList;
    if (pendingEvent?.id === selectedEventId) return pendingEvent;
    return null;
  }, [overlaidEvents, selectedEventId, pendingEvent]);

  // Clear pending once the list catches up.
  useEffect(() => {
    if (
      pendingEvent &&
      events.some((e) => e.id === pendingEvent.id)
    ) {
      setPendingEvent(null);
    }
  }, [events, pendingEvent]);

  // Drop selection if the event disappeared (deleted / filtered out of cache).
  useEffect(() => {
    if (
      selectedEventId &&
      !selectedEvent &&
      !pendingEvent &&
      !eventsQuery.isFetching
    ) {
      setSelectedEventId(null);
      setFocusTitleOnOpen(false);
    }
  }, [
    selectedEventId,
    selectedEvent,
    pendingEvent,
    eventsQuery.isFetching,
  ]);

  const selectedEventCalendar = useMemo(() => {
    if (!selectedEvent) return undefined;
    return calendars.find((c) => c.id === selectedEvent.calendar_id);
  }, [calendars, selectedEvent]);

  const closeInspector = useCallback(() => {
    setSelectedEventId(null);
    setFocusTitleOnOpen(false);
    setPendingEvent(null);
  }, []);

  const selectEvent = useCallback((eventId: string) => {
    setSelectedEventId(eventId);
    setFocusTitleOnOpen(false);
    setPendingEvent(null);
  }, []);

  const handleSaveTitle = useCallback(
    async (summary: string) => {
      if (!selectedEventId) return;
      await updateEvent.mutateAsync({
        id: selectedEventId,
        input: { summary },
      });
    },
    [selectedEventId, updateEvent],
  );

  const handleDeleteEvent = useCallback(async () => {
    if (!selectedEventId) return;
    await deleteEvent.mutateAsync(selectedEventId);
    closeInspector();
  }, [selectedEventId, deleteEvent, closeInspector]);

  const knownCalendarIds = useMemo(
    () => new Set(calendars.map((c) => c.id)),
    [calendars],
  );

  const toggleCalendar = useCallback((calendarId: string) => {
    setSelectedCalendarIds((prev) => {
      const next = new Set(prev);
      if (next.has(calendarId)) {
        next.delete(calendarId);
      } else {
        next.add(calendarId);
      }
      return next;
    });
  }, []);

  // Overlay first, then calendar-visibility filter. Merge a just-created
  // pending event so the chip paints before invalidate (slice 3 owns create).
  const visibleEvents = useMemo(() => {
    const base = overlaidEvents.filter((e) => {
      if (!selectionReady) return true;
      if (selectedCalendarIds.has(e.calendar_id)) return true;
      if (!knownCalendarIds.has(e.calendar_id)) return true;
      return false;
    });
    if (
      pendingEvent &&
      !base.some((e) => e.id === pendingEvent.id)
    ) {
      return [...base, pendingEvent];
    }
    return base;
  }, [
    overlaidEvents,
    selectedCalendarIds,
    knownCalendarIds,
    selectionReady,
    pendingEvent,
  ]);

  const { timedEvents, allDayEvents } = useMemo(() => {
    const timed: CalendarEvent[] = [];
    const allDay: CalendarEvent[] = [];
    for (const e of visibleEvents) {
      const start = new Date(e.start_time);
      const end = new Date(e.end_time);
      if (isMultiDay(start, end)) {
        allDay.push(e);
      } else {
        timed.push(e);
      }
    }
    return { timedEvents: timed, allDayEvents: allDay };
  }, [visibleEvents]);

  // All-day chips first so we know band height for hour stretch.
  const { allDayChips, allDayHeight } = useMemo(
    () => buildAllDayChips(days, allDayEvents, dayCount),
    [days, allDayEvents, dayCount],
  );

  // Headers + all-day are sticky inside the scroller; hours fill the rest.
  const availableHoursPx = useMemo(() => {
    if (scrollerHeight <= 0) return 0;
    return Math.max(0, scrollerHeight - COL_HEADER_H - allDayHeight);
  }, [scrollerHeight, allDayHeight]);

  const hourH = useMemo(
    () => computeHourHeight(availableHoursPx),
    [availableHoursPx],
  );
  const totalHoursH = hourH * 24;

  const commitCreate = useCallback(
    (range: TimedRange) => {
      if (!writableCalendar) {
        closeInspector();
        return;
      }
      createEvent.mutate(
        {
          calendar_id: writableCalendar.id,
          summary: 'New event',
          start: range.start.toISOString(),
          end: range.end.toISOString(),
        },
        {
          onSuccess: (result) => {
            setPendingEvent(result.event);
            setSelectedEventId(result.event.id);
            setFocusTitleOnOpen(true);
          },
        },
      );
    },
    [writableCalendar, createEvent, closeInspector],
  );

  const handleMoveOrResize = useCallback(
    (eventId: string, range: TimedRange) => {
      const current =
        overlaidEvents.find((e) => e.id === eventId) ??
        events.find((e) => e.id === eventId);
      if (!current) return;
      const next: CalendarEvent = {
        ...current,
        start_time: range.start.toISOString(),
        end_time: range.end.toISOString(),
      };
      // Paint immediately; PATCH follows. Overlay wins until reconcile.
      queue.upsert(next);
      void updateEvent
        .mutateAsync({
          id: eventId,
          input: { start: next.start_time, end: next.end_time },
        })
        .then((result) => {
          upsertCalendarEventInCache(result.event);
          // Reconcile THIS event only: drop overlay if it still matches the
          // response (a newer upsert for the same id must stay).
          const latest = queue.getOverlay(eventId);
          if (
            !latest ||
            (latest.op === 'upsert' &&
              latest.event.start_time === result.event.start_time &&
              latest.event.end_time === result.event.end_time &&
              latest.event.title === result.event.title)
          ) {
            queue.clear(eventId);
          }
        })
        .catch(() => {
          // Revert only if overlay was not superseded by a newer drag.
          const latest = queue.getOverlay(eventId);
          if (
            latest?.op === 'upsert' &&
            latest.event.start_time === next.start_time &&
            latest.event.end_time === next.end_time
          ) {
            queue.clear(eventId);
          }
        });
    },
    [overlaidEvents, events, queue, updateEvent],
  );

  const drag = useCalendarDrag({
    hourH,
    setStripLocked,
    onClickCreate: (slot) => commitCreate(clickCreateTimesFromSlot(slot)),
    onDragCreate: commitCreate,
    onMove: handleMoveOrResize,
    onResize: handleMoveOrResize,
    onAllDayCreate: commitCreate,
    onChipTap: selectEvent,
  });

  // Stable across pointermove — only flips at drag start/end (not every move).
  const draggingEventId =
    drag.isDragging && drag.activeEventId ? drag.activeEventId : null;

  const handleSelectChip = useCallback(
    (eventId: string) => {
      if (drag.suppressNextClick()) return;
      selectEvent(eventId);
    },
    [drag.suppressNextClick, selectEvent],
  );

  const timedPreviewByDay = useMemo(
    () =>
      timedPreviewSegments(drag.preview, drag.previewKind, days, hourH),
    [drag.preview, drag.previewKind, days, hourH],
  );

  const allDayPreview = useMemo(() => {
    const idx = allDayPreviewIndices(drag.preview, drag.previewKind, days);
    if (!idx) return null;
    const color = writableCalendar
      ? colorForCalendar(writableCalendar.id)
      : colorForCalendar('preview');
    return { ...idx, color };
  }, [drag.preview, drag.previewKind, days, writableCalendar]);

  const activeDragEvent = drag.activeEventId
    ? overlaidEvents.find((e) => e.id === drag.activeEventId)
    : undefined;
  const previewColor = activeDragEvent
    ? eventChipColor(activeDragEvent)
    : writableCalendar
      ? colorForCalendar(writableCalendar.id)
      : colorForCalendar('preview');
  const previewTitle = activeDragEvent?.title ?? 'New event';

  // Tick "now" so the now-line creeps forward while the page is open.
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const id = window.setInterval(() => setNow(new Date()), 60_000);
    return () => window.clearInterval(id);
  }, []);

  // Position timed events per day column (multi-day events excluded).
  const eventsByDay = useMemo(
    () => buildEventsByDay(days, timedEvents, hourH),
    [days, timedEvents, hourH],
  );

  const todayIndex = useMemo(() => {
    const t = new Date();
    const today = new Date(t.getFullYear(), t.getMonth(), t.getDate());
    const idx = days.findIndex((d) => isSameDay(d, today));
    return idx >= 0 ? idx : null;
  }, [days]);

  const hourLabels = useMemo(
    () => Array.from({ length: 24 }, (_, h) => h),
    [],
  );

  const hourGridBg = useMemo(() => hourGridBackground(hourH), [hourH]);

  return {
    // Header / banner
    isRefreshing,
    isLoading,
    error,
    retry,
    eventsEmpty: events.length === 0,

    // Sidebar
    calendars,
    calendarsLoading: calendarsQuery.isLoading,
    calendarsError,
    retryCalendars: () => {
      void calendarsQuery.refetch();
    },
    selectedCalendarIds,
    toggleCalendar,

    // Grid layout / packing
    now,
    hourH,
    totalHoursH,
    hourGridBg,
    hourLabels,
    availableHoursPx,
    allDayHeight,
    allDayChips,
    todayIndex,
    eventsByDay,

    // Selection / drag
    selectedEventId,
    draggingEventId,
    handleSelectChip,
    selectEvent,
    isDragging: drag.isDragging,
    onColumnPointerDown: drag.onColumnPointerDown,
    onChipPointerDown: drag.onChipPointerDown,
    onAllDayPointerDown: drag.onAllDayPointerDown,
    timedPreviewByDay,
    previewColor,
    previewTitle,
    allDayPreview,

    // Inspector
    selectedEvent,
    selectedEventCalendar,
    focusTitleOnOpen,
    closeInspector,
    handleSaveTitle,
    handleDeleteEvent,
    isSaving: updateEvent.isPending,
    isDeleting: deleteEvent.isPending,
  };
}
