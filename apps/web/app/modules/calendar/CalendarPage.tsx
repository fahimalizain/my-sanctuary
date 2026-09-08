import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useState,
} from 'react';
import { RefreshCw } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  useCalendarEventsQuery,
  useCalendarsQuery,
  useCreateCalendarEvent,
  useDeleteCalendarEvent,
  useUpdateCalendarEvent,
} from '@/app/queries/calendar';
import type { CalendarEvent } from '@/app/types';
import {
  allDayPreviewIndices,
  timedPreviewSegments,
  type TimedRange,
} from './calendar-drag';
import {
  buildAllDayChips,
  buildEventsByDay,
  clickCreateTimesFromSlot,
  eventChipColor,
} from './calendar-model';
import { CalendarHeader } from './CalendarHeader';
import { CalendarSidebar } from './CalendarSidebar';
import { CalendarWeekGrid } from './CalendarWeekGrid';
import { EventInspector } from './EventInspector';
import { useCalendarDrag } from './useCalendarDrag';
import { useCalendarStrip } from './useCalendarStrip';
import {
  COL_HEADER_H,
  addDays,
  colorForCalendar,
  defaultWritableCalendar,
  hourGridBackground,
  hourHeight as computeHourHeight,
  isMultiDay,
  isSameDay,
  nowLineY,
} from './week-layout';

export function CalendarPage() {
  const strip = useCalendarStrip();
  const {
    periodLength,
    handlePeriodChange,
    visibleStart,
    rangeTitle,
    today,
    days,
    dayCount,
    range,
    gridColumnRef,
    scrollerRef,
    scrollerHeight,
    colW,
    gutterW,
    trackWidth,
    contentWidth,
    setStripLocked,
    onScrollerScroll,
    shiftPeriod,
    goToToday,
    goToDate,
    shouldScrollToNowRef,
  } = strip;

  const eventsQuery = useCalendarEventsQuery(range.timeMin, range.timeMax);
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

  const selectedEvent = useMemo(() => {
    if (!selectedEventId) return null;
    const fromList = events.find((e) => e.id === selectedEventId);
    if (fromList) return fromList;
    if (pendingEvent?.id === selectedEventId) return pendingEvent;
    return null;
  }, [events, selectedEventId, pendingEvent]);

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

  // Events whose calendar is selected, plus stale calendar_ids not in the list.
  // Pre-init (!selectionReady) shows everything; after ready, empty Set = none.
  // Merge a just-created pending event so the chip paints before invalidate.
  const visibleEvents = useMemo(() => {
    const base = events.filter((e) => {
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
    events,
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
      void updateEvent.mutateAsync({
        id: eventId,
        input: {
          start: range.start.toISOString(),
          end: range.end.toISOString(),
        },
      });
    },
    [updateEvent],
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
    ? events.find((e) => e.id === drag.activeEventId)
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

  // Scroll so the now line sits ~⅓ down the visible hours area — once per
  // mount / Today jump, and only when the visible period contains today.
  useLayoutEffect(() => {
    if (!shouldScrollToNowRef.current) return;
    if (availableHoursPx <= 0 || hourH <= 0) return;

    const scroller = scrollerRef.current;
    if (!scroller) return;

    const visibleDays = Array.from({ length: periodLength }, (_, i) =>
      addDays(visibleStart, i),
    );
    const periodContainsToday = visibleDays.some((d) => isSameDay(d, today));
    if (!periodContainsToday) {
      shouldScrollToNowRef.current = false;
      return;
    }

    const y = nowLineY(now, hourH);
    // Hours sit below sticky headers + all-day inside the content box.
    const contentY = COL_HEADER_H + allDayHeight + y;
    const visibleH = Math.max(0, scroller.clientHeight);
    scroller.scrollTop = Math.max(0, contentY - visibleH / 3);
    shouldScrollToNowRef.current = false;
  }, [
    availableHoursPx,
    hourH,
    visibleStart,
    periodLength,
    today,
    now,
    allDayHeight,
    shouldScrollToNowRef,
    scrollerRef,
  ]);

  // Position timed events per day column (multi-day events excluded).
  const eventsByDay = useMemo(
    () => buildEventsByDay(days, timedEvents, hourH),
    [days, timedEvents, hourH],
  );

  const todayIndex = useMemo(() => {
    const idx = days.findIndex((d) => isSameDay(d, today));
    return idx >= 0 ? idx : null;
  }, [days, today]);

  const hourLabels = useMemo(
    () => Array.from({ length: 24 }, (_, h) => h),
    [],
  );

  const hourGridBg = useMemo(() => hourGridBackground(hourH), [hourH]);

  return (
    <div className="h-[100dvh] bg-cream flex flex-col pb-20">
      <CalendarHeader
        rangeTitle={rangeTitle}
        isRefreshing={isRefreshing}
        periodLength={periodLength}
        onPeriodChange={handlePeriodChange}
        onToday={goToToday}
        onPrevPeriod={() => shiftPeriod(-1)}
        onNextPeriod={() => shiftPeriod(1)}
      />

      {error && events.length > 0 && (
        <div className="shrink-0 flex items-center justify-between gap-3 border-b border-border bg-muted/50 px-4 py-2 text-sm">
          <p className="text-muted-foreground truncate">
            Couldn&apos;t refresh events: {error}
          </p>
          <Button variant="outline" size="sm" onClick={retry}>
            <RefreshCw className="h-3.5 w-3.5 mr-1.5" />
            Retry
          </Button>
        </div>
      )}

      {/* Body: sidebar + grid + inspector */}
      <div className="flex-1 min-h-0 flex relative">
        <CalendarSidebar
          weekStart={visibleStart}
          periodLength={periodLength}
          onGoToDate={goToDate}
          calendars={calendars}
          calendarsLoading={calendarsQuery.isLoading}
          calendarsError={calendarsError}
          onRetryCalendars={() => {
            void calendarsQuery.refetch();
          }}
          selectedCalendarIds={selectedCalendarIds}
          onToggleCalendar={toggleCalendar}
        />

        <CalendarWeekGrid
          gridColumnRef={gridColumnRef}
          scrollerRef={scrollerRef}
          onScroll={onScrollerScroll}
          contentWidth={contentWidth}
          colW={colW}
          gutterW={gutterW}
          trackWidth={trackWidth}
          days={days}
          today={today}
          now={now}
          hourH={hourH}
          totalHoursH={totalHoursH}
          hourGridBg={hourGridBg}
          hourLabels={hourLabels}
          allDayHeight={allDayHeight}
          allDayChips={allDayChips}
          todayIndex={todayIndex}
          eventsByDay={eventsByDay}
          selectedEventId={selectedEventId}
          draggingEventId={draggingEventId}
          onColumnPointerDown={drag.onColumnPointerDown}
          onChipPointerDown={drag.onChipPointerDown}
          onSelectChip={handleSelectChip}
          onAllDayPointerDown={drag.onAllDayPointerDown}
          onChipSelect={selectEvent}
          isDragging={drag.isDragging}
          timedPreviewByDay={timedPreviewByDay}
          previewColor={previewColor}
          previewTitle={previewTitle}
          allDayPreview={allDayPreview}
          isLoading={isLoading}
          eventsEmpty={events.length === 0}
          error={error}
          onRetry={retry}
        />

        {selectedEvent && (
          <EventInspector
            event={selectedEvent}
            calendar={selectedEventCalendar}
            focusTitle={focusTitleOnOpen}
            onClose={closeInspector}
            onSaveTitle={handleSaveTitle}
            onDelete={handleDeleteEvent}
            isSaving={updateEvent.isPending}
            isDeleting={deleteEvent.isPending}
          />
        )}
      </div>
    </div>
  );
}
