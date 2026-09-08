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
import { useCalendarEventsQuery } from '@/app/queries/calendar';
import type { CalendarEvent } from '@/app/types';
import { cn } from '@/lib/utils';
import {
  CHIP_MARGIN_RIGHT,
  COL_HEADER_H,
  TIME_GUTTER_W,
  WEEK_DAYS,
  addDays,
  clampMinutesToDay,
  colorForCalendar,
  eventHeightPx,
  eventTopPx,
  formatEventTime,
  formatEventTimeRange,
  formatHourLabel,
  formatWeekTitle,
  hourHeight as computeHourHeight,
  isSameDay,
  nowLineY,
  packDayEvents,
  startOfWeek,
  weekDays,
  weekRangeIso,
} from './week-layout';

const NOW_LINE_COLOR = '#F04842'; // Notion --secondary500

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

function hexToRgba(hex: string, alpha: number): string {
  const h = hex.replace('#', '');
  const full =
    h.length === 3
      ? h
          .split('')
          .map((c) => c + c)
          .join('')
      : h;
  const n = parseInt(full, 16);
  const r = (n >> 16) & 255;
  const g = (n >> 8) & 255;
  const b = n & 255;
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
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
  const compact = height < 32;

  const leftPct = (col / cols) * 100;
  const widthPct = (span / cols) * 100;

  return (
    <div
      className="absolute overflow-hidden rounded-md px-1.5 py-0.5 pointer-events-auto"
      style={{
        top,
        height,
        left: `${leftPct}%`,
        width: `calc(${widthPct}% - ${CHIP_MARGIN_RIGHT}px)`,
        backgroundColor: hexToRgba(color, 0.28),
        borderLeft: `3px solid ${color}`,
        color: 'var(--foreground)',
      }}
      title={`${event.title} · ${rangeLabel}`}
      data-start-min={startMin}
      data-end-min={endMin}
    >
      {compact ? (
        <div className="flex items-baseline gap-1 min-w-0 leading-tight">
          <span className="truncate text-[11px] font-medium">{event.title}</span>
          <span className="shrink-0 text-[9px] text-muted-foreground">
            {timeLabel}
          </span>
        </div>
      ) : (
        <div className="min-w-0 leading-tight">
          <div className="truncate text-[11px] font-medium leading-[13px]">
            {event.title}
          </div>
          <div className="truncate text-[9px] text-muted-foreground leading-[11px]">
            {timeLabel}
          </div>
        </div>
      )}
    </div>
  );
}

export function CalendarPage() {
  // Week containing "today" on first mount; prev/next shift by 7 days.
  const [weekStart, setWeekStart] = useState(() => startOfWeek(new Date()));

  const range = useMemo(() => weekRangeIso(weekStart), [weekStart]);

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

  const days = useMemo(() => weekDays(weekStart), [weekStart]);
  const weekTitle = useMemo(() => formatWeekTitle(weekStart), [weekStart]);

  const today = useMemo(() => {
    const now = new Date();
    return new Date(now.getFullYear(), now.getMonth(), now.getDate());
  }, []);

  // Hours-area measurement for hourHeight stretch.
  const scrollerRef = useRef<HTMLDivElement>(null);
  const [availableHoursPx, setAvailableHoursPx] = useState(0);
  // Scroll-to-now only once per mount / when jumping to Today.
  const shouldScrollToNowRef = useRef(true);

  useLayoutEffect(() => {
    const el = scrollerRef.current;
    if (!el) return;

    const measure = () => {
      // Visible hours area ≈ scroller client height minus sticky day headers.
      const h = Math.max(0, el.clientHeight - COL_HEADER_H);
      setAvailableHoursPx(h);
    };
    measure();

    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

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

  // Scroll so the now line sits ~⅓ down the visible hours area — once per
  // mount / Today jump, and only when we're looking at the current week.
  useLayoutEffect(() => {
    if (!shouldScrollToNowRef.current) return;
    if (availableHoursPx <= 0 || hourH <= 0) return;

    const scroller = scrollerRef.current;
    if (!scroller) return;

    const weekContainsToday = days.some((d) => isSameDay(d, today));
    if (!weekContainsToday) {
      shouldScrollToNowRef.current = false;
      return;
    }

    const y = nowLineY(now, hourH);
    const visibleH = Math.max(0, scroller.clientHeight - COL_HEADER_H);
    scroller.scrollTop = Math.max(0, y - visibleH / 3);
    shouldScrollToNowRef.current = false;
  }, [availableHoursPx, hourH, days, today, now]);

  // Position events per day column.
  const eventsByDay = useMemo(() => {
    const map = new Map<string, PositionedEvent[]>();

    for (const day of days) {
      const key = day.toDateString();
      const dayItems: {
        event: CalendarEvent;
        startMin: number;
        endMin: number;
      }[] = [];

      for (const event of events) {
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
  }, [days, events, hourH]);

  const shiftWeek = useCallback((deltaWeeks: number) => {
    shouldScrollToNowRef.current = false;
    setWeekStart((prev) => addDays(prev, deltaWeeks * 7));
  }, []);

  const goToToday = useCallback(() => {
    shouldScrollToNowRef.current = true;
    setWeekStart(startOfWeek(new Date()));
  }, []);

  const hourLabels = useMemo(
    () => Array.from({ length: 24 }, (_, h) => h),
    [],
  );

  return (
    <div className="h-[100dvh] bg-cream flex flex-col pb-20">
      {/* Header */}
      <header className="h-12 shrink-0 flex items-center gap-2 px-3 sm:px-4 border-b border-border/60">
        <h1 className="font-heading text-base sm:text-lg font-semibold text-foreground truncate min-w-0 flex-1">
          {weekTitle}
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
          onClick={() => shiftWeek(-1)}
          aria-label="Previous week"
        >
          <ChevronLeft className="h-4 w-4" />
        </Button>
        <Button
          variant="outline"
          size="icon"
          className="h-8 w-8"
          onClick={() => shiftWeek(1)}
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

      {/* Grid body — always mounted so measure/scroll-to-now effects attach */}
      <div className="flex-1 min-h-0 flex flex-col relative">
        <div
          ref={scrollerRef}
          className={cn(
            'flex-1 min-h-0 overflow-auto',
            isRefreshing && 'opacity-70',
          )}
        >
          {/* Sticky day headers row (gutter spacer + 7 days) */}
          <div
            className="sticky top-0 z-20 flex bg-cream border-b border-border"
            style={{ height: COL_HEADER_H }}
          >
            <div
              className="shrink-0 border-r border-border/60"
              style={{ width: TIME_GUTTER_W }}
              aria-hidden
            />
            {days.map((day, i) => {
              const isToday = isSameDay(day, today);
              return (
                <div
                  key={day.toISOString()}
                  className="flex-1 min-w-0 flex items-center justify-center gap-1 border-r border-border/40 last:border-r-0"
                >
                  <span
                    className={cn(
                      'text-[11px] font-medium uppercase tracking-wide',
                      isToday ? 'text-primary' : 'text-muted-foreground',
                    )}
                  >
                    {WEEK_DAYS[i]}
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

          {/* Hours: gutter + 7 columns, shared scroll (parent scroller) */}
          <div className="relative flex" style={{ height: totalHoursH }}>
            {/* Time gutter */}
            <div
              className="shrink-0 relative border-r border-border/60"
              style={{ width: TIME_GUTTER_W }}
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
              const isToday = isSameDay(day, today);
              const dayEvents = eventsByDay.get(day.toDateString()) ?? [];
              const showNow = isToday;

              return (
                <div
                  key={day.toISOString()}
                  className={cn(
                    'relative flex-1 min-w-0 border-r border-border/40 last:border-r-0',
                    isToday && 'bg-primary/[0.03]',
                  )}
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

        {/* Loading overlay — grid stays mounted and measurable underneath */}
        {isLoading && events.length === 0 && (
          <div
            className="absolute inset-0 z-30 flex flex-col items-center justify-center bg-cream/70 pointer-events-none"
            aria-busy="true"
            aria-label="Loading events"
          >
            <Loader2 className="h-8 w-8 animate-spin mb-3 text-muted-foreground" />
            <p className="text-sm text-muted-foreground">Loading events...</p>
          </div>
        )}

        {/* Hard error overlay — empty grid still mounted underneath */}
        {error && events.length === 0 && !isLoading && (
          <div className="absolute inset-0 z-30 flex flex-col items-center justify-center bg-cream/70 text-center px-4">
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
  );
}
