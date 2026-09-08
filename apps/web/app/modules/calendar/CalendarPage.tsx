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
} from '@/app/queries/calendar';
import type { CalendarEvent } from '@/app/types';
import { cn } from '@/lib/utils';
import { AllDayRow, type AllDayChip } from './AllDayRow';
import { CalendarSidebar } from './CalendarSidebar';
import {
  CHIP_MARGIN_RIGHT,
  COL_HEADER_H,
  DAYS_PER_PERIOD,
  STRIP_OVERSCAN,
  WEEK_DAYS,
  addDays,
  allDaySectionHeight,
  clampMinutesToDay,
  colWidth as computeColWidth,
  colorForCalendar,
  eventHeightPx,
  eventTopPx,
  formatDayRangeTitle,
  formatEventTime,
  formatEventTimeRange,
  formatHourLabel,
  gutterWithRemainder,
  hexToRgba,
  hourHeight as computeHourHeight,
  isCompactChip,
  isMultiDay,
  isSameDay,
  isWeekend,
  lastOccupiedCivilDate,
  nowLineY,
  packAllDayLanes,
  packDayEvents,
  rangeIso,
  scrollLeftForIndex,
  shiftWindowStart,
  shouldRebase,
  startOfDay,
  startOfWeek,
  stripDayCount,
  visibleStartIndex as computeVisibleStartIndex,
} from './week-layout';

const NOW_LINE_COLOR = '#F04842'; // Notion --secondary500
const CHIP_FILL_ALPHA = 0.22;

/** Mon-based short name for a local date (WEEK_DAYS is Mon→Sun). */
function dayNameShort(date: Date): string {
  const jsDay = date.getDay(); // 0 = Sun … 6 = Sat
  const monIndex = jsDay === 0 ? 6 : jsDay - 1;
  return WEEK_DAYS[monIndex];
}

interface PositionedEvent {
  event: CalendarEvent;
  startMin: number;
  endMin: number;
  top: number;
  height: number;
  col: number;
  cols: number;
  span: number;
  color: string;
}

interface EventChipProps {
  positioned: PositionedEvent;
}

function EventChip({ positioned }: EventChipProps) {
  const { event, top, height, col, cols, span, color, startMin, endMin } =
    positioned;
  const start = new Date(event.start_time);
  const end = new Date(event.end_time);
  const timeLabel = formatEventTime(start);
  const rangeLabel = formatEventTimeRange(start, end);
  const compact = isCompactChip(height);

  const leftPct = (col / cols) * 100;
  const widthPct = (span / cols) * 100;

  return (
    <div
      className="absolute overflow-hidden rounded-[6px] pointer-events-auto"
      style={{
        top,
        height,
        left: `${leftPct}%`,
        width: `calc(${widthPct}% - ${CHIP_MARGIN_RIGHT}px)`,
        color: 'var(--foreground)',
      }}
      title={`${event.title} · ${rangeLabel}`}
      data-start-min={startMin}
      data-end-min={endMin}
    >
      {/* 4px left ribbon (not a border) */}
      <div
        className="absolute left-0 top-0 bottom-0 w-1 rounded-l-[6px]"
        style={{ backgroundColor: color }}
        aria-hidden
      />
      <div
        className={cn(
          'h-full min-w-0 pl-2 pr-1',
          compact ? 'flex items-center gap-1 py-0' : 'py-px',
        )}
        style={{ backgroundColor: hexToRgba(color, CHIP_FILL_ALPHA) }}
      >
        {compact ? (
          <>
            <span className="truncate text-[11px] font-medium leading-[13px]">
              {event.title}
            </span>
            <span className="shrink-0 text-[9px] leading-[11px] text-muted-foreground">
              {timeLabel}
            </span>
          </>
        ) : (
          <>
            <div className="truncate text-[11px] font-medium leading-[13px]">
              {event.title}
            </div>
            <div className="mt-0.5 truncate text-[9px] leading-[11px] text-muted-foreground">
              {timeLabel}
            </div>
          </>
        )}
      </div>
    </div>
  );
}

export function CalendarPage() {
  // Strip window: first *rendered* day. Visible period is windowStart + scroll offset.
  // Initial: overscan before current Mon–Sun so current week is centered in the buffer.
  const [windowStart, setWindowStart] = useState(() =>
    addDays(startOfWeek(new Date()), -STRIP_OVERSCAN),
  );

  // First visible day index into the rendered strip (0 … dayCount-7).
  // Seeded at overscan so the current Mon–Sun shows before measure/scroll attach.
  const [visibleStartIdx, setVisibleStartIdx] = useState(STRIP_OVERSCAN);

  const dayCount = stripDayCount();
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

  // Default: select every calendar once the list first arrives.
  useEffect(() => {
    if (calendars.length === 0 || selectionReady) return;
    setSelectedCalendarIds(new Set(calendars.map((c) => c.id)));
    setSelectionReady(true);
  }, [calendars, selectionReady]);

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
  const visibleEvents = useMemo(() => {
    return events.filter((e) => {
      if (!selectionReady) return true;
      if (selectedCalendarIds.has(e.calendar_id)) return true;
      if (!knownCalendarIds.has(e.calendar_id)) return true;
      return false;
    });
  }, [events, selectedCalendarIds, knownCalendarIds, selectionReady]);

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

  // Full rendered strip (21 days).
  const days = useMemo(() => {
    const origin = startOfDay(windowStart);
    return Array.from({ length: dayCount }, (_, i) => addDays(origin, i));
  }, [windowStart, dayCount]);

  // First day of the 7-day visible period (drives title + sidebar highlight).
  const visibleStart = useMemo(
    () => addDays(startOfDay(windowStart), visibleStartIdx),
    [windowStart, visibleStartIdx],
  );
  const visibleEnd = useMemo(
    () => addDays(visibleStart, DAYS_PER_PERIOD - 1),
    [visibleStart],
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

  const colW = useMemo(() => computeColWidth(mainWidth), [mainWidth]);
  const gutterW = useMemo(
    () => gutterWithRemainder(mainWidth, colW),
    [mainWidth, colW],
  );
  const trackWidth = dayCount * colW;
  const contentWidth = gutterW + trackWidth;

  // All-day chips first so we know band height for hour stretch.
  const { allDayChips, allDayHeight } = useMemo(() => {
    const origin = days[0];
    if (!origin) {
      return {
        allDayChips: [] as AllDayChip[],
        allDayHeight: allDaySectionHeight(null),
      };
    }

    const msPerDay = 24 * 60 * 60 * 1000;
    const lastIdx = dayCount - 1;
    const inputs: {
      id: string;
      startDay: number;
      endDay: number;
      event: CalendarEvent;
    }[] = [];

    for (const event of allDayEvents) {
      const start = new Date(event.start_time);
      const end = new Date(event.end_time);
      const first = startOfDay(start);
      const last = lastOccupiedCivilDate(start, end);

      let startDay = Math.round(
        (first.getTime() - origin.getTime()) / msPerDay,
      );
      let endDay = Math.round((last.getTime() - origin.getTime()) / msPerDay);

      // No overlap with rendered window [0, dayCount).
      if (endDay < 0 || startDay > lastIdx) continue;
      startDay = Math.max(0, Math.min(lastIdx, startDay));
      endDay = Math.max(0, Math.min(lastIdx, endDay));
      if (endDay < startDay) continue;

      inputs.push({ id: event.id, startDay, endDay, event });
    }

    const packed = packAllDayLanes(
      inputs.map((i) => ({
        id: i.id,
        startDay: i.startDay,
        endDay: i.endDay,
      })),
    );
    const laneById = new Map(packed.map((p) => [p.id, p.lane]));

    let maxLane: number | null = null;
    const chips: AllDayChip[] = inputs.map((i) => {
      const lane = laneById.get(i.id) ?? 0;
      if (maxLane === null || lane > maxLane) maxLane = lane;
      return {
        id: i.id,
        title: i.event.title,
        startDay: i.startDay,
        endDay: i.endDay,
        lane,
        color: colorForCalendar(i.event.calendar_id || i.event.id),
      };
    });

    return {
      allDayChips: chips,
      allDayHeight: allDaySectionHeight(maxLane),
    };
  }, [days, allDayEvents, dayCount]);

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

    // Rebase first: shift window ±7 days and compensate scrollLeft so the
    // picture does not jump. Must run before paint.
    const dir = pendingRebaseRef.current;
    if (dir !== 0) {
      pendingRebaseRef.current = 0;
      const compensation = -dir * DAYS_PER_PERIOD * colW;
      suppressRebaseRef.current = true;
      setWindowStart((prev) => shiftWindowStart(startOfDay(prev), dir));
      scroller.scrollLeft = scroller.scrollLeft + compensation;
      setVisibleStartIdx(
        computeVisibleStartIndex(scroller.scrollLeft, colW, dayCount),
      );
      releaseSuppressRebase();
      return;
    }

    // Initial mount: park scroll so current Mon–Sun is the visible period.
    if (!didInitScrollRef.current) {
      didInitScrollRef.current = true;
      suppressRebaseRef.current = true;
      scroller.scrollLeft = scrollLeftForIndex(STRIP_OVERSCAN, colW);
      setVisibleStartIdx(STRIP_OVERSCAN);
      releaseSuppressRebase();
    }

    // Programmatic jump (Today / mini-month): park at overscan index.
    if (pendingScrollLeftRef.current !== null) {
      pendingScrollLeftRef.current = null;
      suppressRebaseRef.current = true;
      scroller.scrollLeft = scrollLeftForIndex(STRIP_OVERSCAN, colW);
      setVisibleStartIdx(STRIP_OVERSCAN);
      releaseSuppressRebase();
    }
  }, [colW, dayCount, windowStart, scrollNonce]);

  // Scroll so the now line sits ~⅓ down the visible hours area — once per
  // mount / Today jump, and only when the visible period contains today.
  useLayoutEffect(() => {
    if (!shouldScrollToNowRef.current) return;
    if (availableHoursPx <= 0 || hourH <= 0) return;

    const scroller = scrollerRef.current;
    if (!scroller) return;

    const visibleDays = Array.from({ length: DAYS_PER_PERIOD }, (_, i) =>
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
    today,
    now,
    allDayHeight,
  ]);

  // Horizontal scroll: update visible start; request rebase near the edges.
  const onScrollerScroll = useCallback(() => {
    const scroller = scrollerRef.current;
    if (!scroller || colW <= 0) return;

    const idx = computeVisibleStartIndex(scroller.scrollLeft, colW, dayCount);
    setVisibleStartIdx((prev) => (prev === idx ? prev : idx));

    if (suppressRebaseRef.current) return;

    const dir = shouldRebase(idx, dayCount);
    if (dir !== 0 && pendingRebaseRef.current === 0) {
      pendingRebaseRef.current = dir;
      setScrollNonce((n) => n + 1);
    }
  }, [colW, dayCount]);

  // Position timed events per day column (multi-day events excluded).
  const eventsByDay = useMemo(() => {
    const map = new Map<string, PositionedEvent[]>();

    for (const day of days) {
      const key = day.toDateString();
      const dayItems: {
        event: CalendarEvent;
        startMin: number;
        endMin: number;
      }[] = [];

      for (const event of timedEvents) {
        const start = new Date(event.start_time);
        const end = new Date(event.end_time);
        const clamped = clampMinutesToDay(start, end, day);
        if (!clamped) continue;
        dayItems.push({
          event,
          startMin: clamped.startMin,
          endMin: clamped.endMin,
        });
      }

      const packed = packDayEvents(
        dayItems.map((d) => ({
          id: d.event.id,
          startMin: d.startMin,
          endMin: d.endMin,
        })),
      );
      const packById = new Map(packed.map((p) => [p.id, p]));

      const positioned: PositionedEvent[] = dayItems.map((d) => {
        const pack = packById.get(d.event.id)!;
        return {
          event: d.event,
          startMin: d.startMin,
          endMin: d.endMin,
          top: eventTopPx(d.startMin, hourH),
          height: eventHeightPx(d.startMin, d.endMin, hourH),
          col: pack.col,
          cols: pack.cols,
          span: pack.span,
          color: colorForCalendar(d.event.calendar_id || d.event.id),
        };
      });

      map.set(key, positioned);
    }

    return map;
  }, [days, timedEvents, hourH]);

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
      scroller.scrollLeft += deltaPeriods * DAYS_PER_PERIOD * colW;
      // onScroll will update visibleStartIdx and rebase if needed.
      onScrollerScroll();
    },
    [colW, onScrollerScroll],
  );

  const goToToday = useCallback(() => {
    jumpToVisibleStart(startOfWeek(new Date()), true);
  }, [jumpToVisibleStart]);

  const goToDate = useCallback(
    (date: Date) => {
      // Monday-snap for mini-month (and Today). Free scroll can land anywhere.
      jumpToVisibleStart(startOfWeek(date), isSameDay(date, today));
    },
    [jumpToVisibleStart, today],
  );

  const hourLabels = useMemo(
    () => Array.from({ length: 24 }, (_, h) => h),
    [],
  );

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

        <span className="hidden sm:inline text-xs font-medium text-muted-foreground px-2">
          Week
        </span>

        <Button variant="outline" size="sm" onClick={goToToday}>
          Today
        </Button>
        <Button
          variant="outline"
          size="icon"
          className="h-8 w-8"
          onClick={() => shiftPeriod(-1)}
          aria-label="Previous week"
        >
          <ChevronLeft className="h-4 w-4" />
        </Button>
        <Button
          variant="outline"
          size="icon"
          className="h-8 w-8"
          onClick={() => shiftPeriod(1)}
          aria-label="Next week"
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

      {/* Body: sidebar + grid */}
      <div className="flex-1 min-h-0 flex">
        <CalendarSidebar
          weekStart={visibleStart}
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
          className="flex-1 min-h-0 flex flex-col relative"
        >
          <div
            ref={scrollerRef}
            className={cn(
              'flex-1 min-h-0 overflow-auto',
              isRefreshing && 'opacity-70',
            )}
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
                />
              </div>

              {/* Hours: sticky left gutter + day columns */}
              <div
                className="relative flex"
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

                {/* Day columns */}
                {days.map((day) => {
                  const isTodayCol = isSameDay(day, today);
                  const weekend = isWeekend(day);
                  const dayEvents = eventsByDay.get(day.toDateString()) ?? [];
                  const showNow = isTodayCol;

                  return (
                    <div
                      key={day.toISOString()}
                      className={cn(
                        'relative shrink-0 border-r last:border-r-0',
                        weekend ? 'border-border/60' : 'border-border/40',
                        isTodayCol
                          ? 'bg-primary/[0.03]'
                          : weekend && 'bg-muted/40',
                      )}
                      style={{ width: colW, height: totalHoursH }}
                    >
                      {/* Hour hairlines */}
                      {hourLabels.map((h) => (
                        <div
                          key={h}
                          className="absolute left-0 right-0 border-t border-border/50"
                          style={{ top: h * hourH }}
                        />
                      ))}

                      {/* Event chips */}
                      {dayEvents.map((p) => (
                        <EventChip key={p.event.id} positioned={p} />
                      ))}

                      {/* Now line */}
                      {showNow && (
                        <div
                          className="absolute left-0 right-0 z-10 pointer-events-none"
                          style={{ top: nowLineY(now, hourH) }}
                          aria-hidden
                        >
                          <div
                            className="absolute -left-[5px] -top-[1px] h-[2px] w-[10px] rounded-full"
                            style={{ backgroundColor: NOW_LINE_COLOR }}
                          />
                          <div
                            className="h-px w-full"
                            style={{ backgroundColor: NOW_LINE_COLOR }}
                          />
                        </div>
                      )}
                    </div>
                  );
                })}
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
      </div>
    </div>
  );
}
