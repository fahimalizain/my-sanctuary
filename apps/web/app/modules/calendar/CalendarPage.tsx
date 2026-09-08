import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from 'react';
import { ChevronLeft, ChevronRight, Loader2, RefreshCw } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  useCalendarEventsQuery,
  useCalendarsQuery,
  useCreateCalendarEvent,
  useDeleteCalendarEvent,
  useUpdateCalendarEvent,
} from '@/app/queries/calendar';
import type { CalendarEvent } from '@/app/types';
import { cn } from '@/lib/utils';
import { AllDayRow } from './AllDayRow';
import {
  allDayPreviewIndices,
  timedPreviewSegments,
  type TimedRange,
} from './calendar-drag';
import {
  buildAllDayChips,
  buildEventsByDay,
  clickCreateTimesFromSlot,
  dayNameShort,
  eventChipColor,
} from './calendar-model';
import { CalendarSidebar } from './CalendarSidebar';
import { DayColumn, TimedPreviewLayer } from './DayColumn';
import { type PositionedEvent } from './EventChip';
import { EventInspector } from './EventInspector';
import { useCalendarDrag } from './useCalendarDrag';
import { ViewSelector } from './ViewSelector';
import {
  COL_HEADER_H,
  DAYS_PER_PERIOD,
  STRIP_OVERSCAN,
  addDays,
  clampPeriodLength,
  colWidth as computeColWidth,
  colorForCalendar,
  defaultWritableCalendar,
  fitPeriodStart,
  formatDayRangeTitle,
  formatHourLabel,
  gutterWithRemainder,
  hourGridBackground,
  hourHeight as computeHourHeight,
  isMultiDay,
  isSameDay,
  isWeekend,
  nowLineY,
  rangeIso,
  scrollLeftForIndex,
  shiftWindowStart,
  shouldRebase,
  startOfDay,
  stripDayCount,
  visibleStartIndex as computeVisibleStartIndex,
} from './week-layout';

/** Stable empty list so DayColumn memo is not busted on empty days. */
const EMPTY_DAY_EVENTS: PositionedEvent[] = [];

export function CalendarPage() {
  // Visible day-column count (1–7). Session-only; not persisted.
  const [periodLength, setPeriodLength] = useState(DAYS_PER_PERIOD);

  // Strip window: first *rendered* day. Visible period is windowStart + scroll offset.
  // Initial: overscan before current Mon–Sun so current week is centered in the buffer.
  const [windowStart, setWindowStart] = useState(() =>
    addDays(fitPeriodStart(new Date(), DAYS_PER_PERIOD), -STRIP_OVERSCAN),
  );

  // First visible day index into the rendered strip (0 … dayCount-periodLength).
  // Seeded at overscan so the current period shows before measure/scroll attach.
  const [visibleStartIdx, setVisibleStartIdx] = useState(STRIP_OVERSCAN);

  const dayCount = stripDayCount(periodLength);
  const range = useMemo(
    () => rangeIso(windowStart, dayCount),
    [windowStart, dayCount],
  );

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

  // Full rendered strip (period + overscan each side).
  const days = useMemo(() => {
    const origin = startOfDay(windowStart);
    return Array.from({ length: dayCount }, (_, i) => addDays(origin, i));
  }, [windowStart, dayCount]);

  // First day of the visible period (drives title + sidebar highlight).
  const visibleStart = useMemo(
    () => addDays(startOfDay(windowStart), visibleStartIdx),
    [windowStart, visibleStartIdx],
  );
  const visibleEnd = useMemo(
    () => addDays(visibleStart, periodLength - 1),
    [visibleStart, periodLength],
  );
  const rangeTitle = useMemo(
    () => formatDayRangeTitle(visibleStart, visibleEnd),
    [visibleStart, visibleEnd],
  );

  const today = useMemo(() => {
    const now = new Date();
    return new Date(now.getFullYear(), now.getMonth(), now.getDate());
  }, []);

  // Measure the grid column (not the scroller) for colWidth / gutter remainder.
  const gridColumnRef = useRef<HTMLDivElement>(null);
  const scrollerRef = useRef<HTMLDivElement>(null);
  const [mainWidth, setMainWidth] = useState(0);
  const [scrollerHeight, setScrollerHeight] = useState(0);

  // Scroll-to-now only once per mount / when jumping to Today.
  const shouldScrollToNowRef = useRef(true);
  // Pending horizontal scroll position after programmatic window moves.
  const pendingScrollLeftRef = useRef<number | null>(null);
  // Seed initial H-scroll once colWidth is known.
  const didInitScrollRef = useRef(false);
  // Rebase direction requested by the scroll handler; applied in layout effect.
  const pendingRebaseRef = useRef<-1 | 0 | 1>(0);
  const [scrollNonce, setScrollNonce] = useState(0);
  // Suppress rebase while applying a programmatic scrollLeft write.
  const suppressRebaseRef = useRef(false);

  useLayoutEffect(() => {
    const el = gridColumnRef.current;
    if (!el) return;

    const measure = () => {
      setMainWidth(el.clientWidth);
      const scroller = scrollerRef.current;
      if (scroller) setScrollerHeight(scroller.clientHeight);
    };
    measure();

    const ro = new ResizeObserver(measure);
    ro.observe(el);
    const scroller = scrollerRef.current;
    if (scroller) ro.observe(scroller);
    return () => ro.disconnect();
  }, []);

  const colW = useMemo(
    () => computeColWidth(mainWidth, periodLength),
    [mainWidth, periodLength],
  );
  const gutterW = useMemo(
    () => gutterWithRemainder(mainWidth, colW, periodLength),
    [mainWidth, colW, periodLength],
  );
  const trackWidth = dayCount * colW;
  const contentWidth = gutterW + trackWidth;

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

  const setStripLocked = useCallback((locked: boolean) => {
    suppressRebaseRef.current = locked;
  }, []);

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

  const releaseSuppressRebase = () => {
    // scroll events from programmatic scrollLeft can be sync or rAF-deferred.
    requestAnimationFrame(() => {
      suppressRebaseRef.current = false;
    });
  };

  // Apply pending horizontal scroll (init / Today / mini-month) and rebase.
  useLayoutEffect(() => {
    const scroller = scrollerRef.current;
    if (!scroller || colW <= 0) return;

    // Rebase first: shift window ±periodLength days and compensate scrollLeft
    // so the picture does not jump. Must run before paint.
    const dir = pendingRebaseRef.current;
    if (dir !== 0) {
      pendingRebaseRef.current = 0;
      const compensation = -dir * periodLength * colW;
      suppressRebaseRef.current = true;
      setWindowStart((prev) =>
        shiftWindowStart(startOfDay(prev), dir, periodLength),
      );
      scroller.scrollLeft = scroller.scrollLeft + compensation;
      setVisibleStartIdx(
        computeVisibleStartIndex(
          scroller.scrollLeft,
          colW,
          dayCount,
          periodLength,
        ),
      );
      releaseSuppressRebase();
      return;
    }

    // Initial mount: park scroll so current period is visible.
    if (!didInitScrollRef.current) {
      didInitScrollRef.current = true;
      suppressRebaseRef.current = true;
      scroller.scrollLeft = scrollLeftForIndex(STRIP_OVERSCAN, colW);
      setVisibleStartIdx(STRIP_OVERSCAN);
      releaseSuppressRebase();
    }

    // Programmatic jump (Today / mini-month / period change): park at overscan.
    if (pendingScrollLeftRef.current !== null) {
      pendingScrollLeftRef.current = null;
      suppressRebaseRef.current = true;
      scroller.scrollLeft = scrollLeftForIndex(STRIP_OVERSCAN, colW);
      setVisibleStartIdx(STRIP_OVERSCAN);
      releaseSuppressRebase();
    }
  }, [colW, dayCount, periodLength, windowStart, scrollNonce]);

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
  ]);

  // Horizontal scroll: update visible start; request rebase near the edges.
  const onScrollerScroll = useCallback(() => {
    const scroller = scrollerRef.current;
    if (!scroller || colW <= 0) return;

    const idx = computeVisibleStartIndex(
      scroller.scrollLeft,
      colW,
      dayCount,
      periodLength,
    );
    setVisibleStartIdx((prev) => (prev === idx ? prev : idx));

    if (suppressRebaseRef.current) return;

    const dir = shouldRebase(idx, dayCount, periodLength);
    if (dir !== 0 && pendingRebaseRef.current === 0) {
      pendingRebaseRef.current = dir;
      setScrollNonce((n) => n + 1);
    }
  }, [colW, dayCount, periodLength]);

  // Position timed events per day column (multi-day events excluded).
  const eventsByDay = useMemo(
    () => buildEventsByDay(days, timedEvents, hourH),
    [days, timedEvents, hourH],
  );

  const todayIndex = useMemo(() => {
    const idx = days.findIndex((d) => isSameDay(d, today));
    return idx >= 0 ? idx : null;
  }, [days, today]);

  /** Park the strip so `firstVisible` is the left edge of the viewport. */
  const jumpToVisibleStart = useCallback(
    (firstVisible: Date, scrollToNow: boolean) => {
      shouldScrollToNowRef.current = scrollToNow;
      pendingRebaseRef.current = 0;
      const origin = addDays(startOfDay(firstVisible), -STRIP_OVERSCAN);
      setWindowStart(origin);
      setVisibleStartIdx(STRIP_OVERSCAN);
      // Layout effect parks scrollLeft at overscan once colW is known.
      pendingScrollLeftRef.current = 0; // non-null sentinel
      setScrollNonce((n) => n + 1);
    },
    [],
  );

  const shiftPeriod = useCallback(
    (deltaPeriods: number) => {
      shouldScrollToNowRef.current = false;
      const scroller = scrollerRef.current;
      if (!scroller || colW <= 0) return;
      scroller.scrollLeft += deltaPeriods * periodLength * colW;
      // onScroll will update visibleStartIdx and rebase if needed.
      onScrollerScroll();
    },
    [colW, periodLength, onScrollerScroll],
  );

  const goToToday = useCallback(() => {
    jumpToVisibleStart(fitPeriodStart(new Date(), periodLength), true);
  }, [jumpToVisibleStart, periodLength]);

  const goToDate = useCallback(
    (date: Date) => {
      // fitPeriodStart Monday-snaps for 5–7 day views; shorter views land on the day.
      jumpToVisibleStart(
        fitPeriodStart(date, periodLength),
        isSameDay(date, today),
      );
    },
    [jumpToVisibleStart, periodLength, today],
  );

  const handlePeriodChange = useCallback(
    (n: number) => {
      const next = clampPeriodLength(n);
      setPeriodLength(next);
      const nextStart = fitPeriodStart(visibleStart, next);
      const nextDays = Array.from({ length: next }, (_, i) =>
        addDays(nextStart, i),
      );
      const containsToday = nextDays.some((d) => isSameDay(d, today));
      jumpToVisibleStart(nextStart, containsToday);
    },
    [visibleStart, today, jumpToVisibleStart],
  );

  // Notion-style period shortcuts (ignore when typing in a field).
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const t = e.target;
      if (t instanceof HTMLElement) {
        const tag = t.tagName;
        if (
          tag === 'INPUT' ||
          tag === 'TEXTAREA' ||
          tag === 'SELECT' ||
          t.isContentEditable
        ) {
          return;
        }
      }
      if (e.metaKey || e.ctrlKey || e.altKey) return;

      const key = e.key;
      let next: number | null = null;
      if (key === '1' || key === 'd' || key === 'D') next = 1;
      else if (key === 'w' || key === 'W' || key === '0') next = 7;
      else if (key >= '2' && key <= '6') next = Number(key);

      if (next === null) return;
      e.preventDefault();
      handlePeriodChange(next);
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [handlePeriodChange]);

  const hourLabels = useMemo(
    () => Array.from({ length: 24 }, (_, h) => h),
    [],
  );

  const hourGridBg = useMemo(() => hourGridBackground(hourH), [hourH]);

  return (
    <div className="h-[100dvh] bg-cream flex flex-col pb-20">
      {/* Header */}
      <header className="h-12 shrink-0 flex items-center gap-2 px-3 sm:px-4 border-b border-border/60">
        <h1 className="font-heading text-base sm:text-lg font-semibold text-foreground truncate min-w-0 flex-1">
          {rangeTitle}
        </h1>

        {isRefreshing && (
          <Loader2
            className="h-4 w-4 animate-spin text-muted-foreground shrink-0"
            aria-label="Refreshing events"
          />
        )}

        <ViewSelector
          periodLength={periodLength}
          onChange={handlePeriodChange}
        />

        <Button variant="outline" size="sm" onClick={goToToday}>
          Today
        </Button>
        <Button
          variant="outline"
          size="icon"
          className="h-8 w-8"
          onClick={() => shiftPeriod(-1)}
          aria-label="Previous period"
        >
          <ChevronLeft className="h-4 w-4" />
        </Button>
        <Button
          variant="outline"
          size="icon"
          className="h-8 w-8"
          onClick={() => shiftPeriod(1)}
          aria-label="Next period"
        >
          <ChevronRight className="h-4 w-4" />
        </Button>
      </header>

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

        {/* Grid column — always mounted so measure/scroll-to-now effects attach */}
        <div
          ref={gridColumnRef}
          className="flex-1 min-h-0 flex flex-col relative min-w-0"
        >
          <div
            ref={scrollerRef}
            className="flex-1 min-h-0 overflow-auto [scrollbar-width:none] [-ms-overflow-style:none] [&::-webkit-scrollbar]:hidden"
            onScroll={onScrollerScroll}
          >
            {/* Content: sticky headers + all-day + hours strip */}
            <div style={{ width: contentWidth, minHeight: '100%' }}>
              {/* Day headers — sticky top */}
              <div
                className="sticky top-0 z-20 flex bg-cream border-b border-border"
                style={{ height: COL_HEADER_H, width: contentWidth }}
              >
                <div
                  className="shrink-0 sticky left-0 top-0 z-40 border-r border-border/60 bg-cream"
                  style={{ width: gutterW }}
                  aria-hidden
                />
                {days.map((day) => {
                  const isToday = isSameDay(day, today);
                  return (
                    <div
                      key={day.toISOString()}
                      className="shrink-0 flex items-center justify-center gap-1 border-r border-border/40 last:border-r-0"
                      style={{ width: colW }}
                    >
                      <span
                        className={cn(
                          'text-[11px] font-medium uppercase tracking-wide',
                          isToday ? 'text-primary' : 'text-muted-foreground',
                        )}
                      >
                        {dayNameShort(day)}
                      </span>
                      <span
                        className={cn(
                          'inline-flex h-6 w-6 items-center justify-center rounded-full text-[13px] font-semibold tabular-nums',
                          isToday
                            ? 'bg-primary text-primary-foreground'
                            : 'text-foreground',
                        )}
                      >
                        {day.getDate()}
                      </span>
                    </div>
                  );
                })}
              </div>

              {/* All-day band — sticky under headers */}
              <div
                className="sticky z-20 bg-cream"
                style={{ top: COL_HEADER_H }}
              >
                <AllDayRow
                  days={days}
                  colWidth={colW}
                  gutterWidth={gutterW}
                  height={allDayHeight}
                  chips={allDayChips}
                  todayIndex={todayIndex}
                  onDayPointerDown={drag.onAllDayPointerDown}
                  onChipSelect={selectEvent}
                  selectedEventId={selectedEventId}
                  preview={allDayPreview}
                />
              </div>

              {/* Hours: sticky left gutter + day columns */}
              <div
                className={cn(
                  'relative flex',
                  drag.isDragging && 'select-none',
                )}
                style={{ height: totalHoursH, width: contentWidth }}
              >
                {/* Time gutter */}
                <div
                  className="shrink-0 sticky left-0 z-30 border-r border-border/60 bg-cream"
                  style={{ width: gutterW, height: totalHoursH }}
                >
                  {hourLabels.map((h) => {
                    const label = formatHourLabel(h);
                    if (!label) return null;
                    return (
                      <div
                        key={h}
                        className="absolute right-1 -translate-y-1/2 text-[10px] leading-none text-muted-foreground tabular-nums select-none"
                        style={{ top: h * hourH }}
                      >
                        {label}
                      </div>
                    );
                  })}
                </div>

                {/* Day columns — memoized; drag preview is a sibling overlay */}
                {days.map((day) => {
                  const isTodayCol = isSameDay(day, today);
                  const dayKey = day.toDateString();
                  const dayEvents =
                    eventsByDay.get(dayKey) ?? EMPTY_DAY_EVENTS;

                  return (
                    <DayColumn
                      key={day.toISOString()}
                      day={day}
                      colW={colW}
                      totalHoursH={totalHoursH}
                      hourH={hourH}
                      hourGridBg={hourGridBg}
                      events={dayEvents}
                      selectedEventId={selectedEventId}
                      draggingEventId={draggingEventId}
                      isToday={isTodayCol}
                      isWeekend={isWeekend(day)}
                      showNow={isTodayCol}
                      now={isTodayCol ? now : undefined}
                      onColumnPointerDown={drag.onColumnPointerDown}
                      onChipPointerDown={drag.onChipPointerDown}
                      onSelectChip={handleSelectChip}
                    />
                  );
                })}

                <TimedPreviewLayer
                  days={days}
                  colW={colW}
                  gutterW={gutterW}
                  trackWidth={trackWidth}
                  totalHoursH={totalHoursH}
                  timedPreviewByDay={timedPreviewByDay}
                  previewColor={previewColor}
                  previewTitle={previewTitle}
                />
              </div>
            </div>
          </div>

          {/* Loading overlay — grid stays mounted and measurable underneath */}
          {isLoading && events.length === 0 && (
            <div
              className="absolute inset-0 z-50 flex flex-col items-center justify-center bg-cream/70 pointer-events-none"
              aria-busy="true"
              aria-label="Loading events"
            >
              <Loader2 className="h-8 w-8 animate-spin mb-3 text-muted-foreground" />
              <p className="text-sm text-muted-foreground">Loading events...</p>
            </div>
          )}

          {/* Hard error overlay — empty grid still mounted underneath */}
          {error && events.length === 0 && !isLoading && (
            <div className="absolute inset-0 z-50 flex flex-col items-center justify-center bg-cream/70 text-center px-4">
              <p className="text-foreground font-medium mb-2">
                Failed to load events
              </p>
              <p className="text-sm text-muted-foreground mb-4">{error}</p>
              <Button variant="outline" onClick={retry}>
                <RefreshCw className="h-4 w-4 mr-2" />
                Retry
              </Button>
            </div>
          )}
        </div>

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
