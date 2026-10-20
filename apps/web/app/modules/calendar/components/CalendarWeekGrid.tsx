import type { PointerEvent as ReactPointerEvent, RefObject } from 'react';
import { Loader2, RefreshCw } from 'lucide-react';
import { Button } from '@/components/ui/button';
import type { CalendarEvent } from '@/app/types';
import { cn } from '@/lib/utils';
import { AllDayRow, type AllDayChip, type AllDayPreview } from './AllDayRow';
import type { DragPreviewInfo } from '../lib/calendar-drag';
import { dayNameShort } from '../lib/calendar-model';
import { DayColumn, TimedPreviewLayer } from './DayColumn';
import { GutterTimes } from './GutterTimes';
import { type PositionedEvent } from './EventChip';
import { COL_HEADER_H, isSameDay, isWeekend } from '../lib/week-layout';
import type { StripTrackRef } from '../hooks/useCalendarStrip';

/** Stable empty list so DayColumn memo is not busted on empty days. */
const EMPTY_DAY_EVENTS: PositionedEvent[] = [];

export interface CalendarWeekGridProps {
  gridColumnRef: RefObject<HTMLDivElement | null>;
  scrollerRef: RefObject<HTMLDivElement | null>;
  /** Registers a day track so the strip can translate it. */
  stripTrackRef: StripTrackRef;
  colW: number;
  gutterW: number;
  trackWidth: number;
  days: Date[];
  today: Date;
  /** Current time for the now-line (only needed on today's column). */
  now: Date;
  hourH: number;
  totalHoursH: number;
  hourGridBg: string;
  hourLabels: number[];
  /** Timed start/end for the gutter pills (drag preview wins over selection). */
  gutterEdges: { start: Date; end: Date } | null;
  allDayHeight: number;
  allDayChips: AllDayChip[];
  todayIndex: number | null;
  eventsByDay: Map<string, PositionedEvent[]>;
  selectedEventId: string | null;
  draggingEventId: string | null;
  onColumnPointerDown: (e: ReactPointerEvent<HTMLElement>, day: Date) => void;
  onChipPointerDown: (
    e: ReactPointerEvent<HTMLElement>,
    event: CalendarEvent,
    chipEl: HTMLElement,
  ) => void;
  onSelectChip: (eventId: string) => void;
  onRequestDelete?: () => void;
  onAllDayPointerDown: (e: ReactPointerEvent<HTMLElement>, day: Date) => void;
  onAllDayChipPointerDown: (
    e: ReactPointerEvent<HTMLElement>,
    eventId: string,
    chipEl: HTMLElement,
  ) => void;
  /** Grab a gutter edge pill for the selected timed event. */
  onGutterEdgePointerDown: (
    e: ReactPointerEvent<HTMLElement>,
    edge: 'start' | 'end',
  ) => void;
  onChipSelect: (eventId: string) => void;
  isDragging: boolean;
  /** Precision modifier is live: show the 1-minute grid badge. */
  precisionSnap: boolean;
  timedPreviewByDay: Map<string, { top: number; height: number }>;
  previewInfo: DragPreviewInfo | null;
  previewColor: string;
  previewTitle: string;
  allDayPreview: AllDayPreview | null;
  isLoading: boolean;
  eventsEmpty: boolean;
  error: string | null;
  onRetry: () => void;
}

export function CalendarWeekGrid({
  gridColumnRef,
  scrollerRef,
  stripTrackRef,
  colW,
  gutterW,
  trackWidth,
  days,
  today,
  now,
  hourH,
  totalHoursH,
  hourGridBg,
  hourLabels,
  gutterEdges,
  allDayHeight,
  allDayChips,
  todayIndex,
  eventsByDay,
  selectedEventId,
  draggingEventId,
  onColumnPointerDown,
  onChipPointerDown,
  onSelectChip,
  onRequestDelete,
  onAllDayPointerDown,
  onAllDayChipPointerDown,
  onGutterEdgePointerDown,
  onChipSelect,
  isDragging,
  precisionSnap,
  timedPreviewByDay,
  previewInfo,
  previewColor,
  previewTitle,
  allDayPreview,
  isLoading,
  eventsEmpty,
  error,
  onRetry,
}: CalendarWeekGridProps) {
  return (
    /* Grid column — always mounted so measure/scroll-to-now effects attach */
    <div
      ref={gridColumnRef}
      className="flex-1 min-h-0 flex flex-col relative min-w-0"
    >
      <div
        ref={scrollerRef}
        data-calendar-scroller
        className="flex-1 min-h-0 overflow-x-hidden overflow-y-auto [touch-action:pan-y] [scrollbar-width:none] [-ms-overflow-style:none] [&::-webkit-scrollbar]:hidden"
        // One gesture, one axis. The browser may only pan the hours (overflow-y
        // + `touch-action: pan-y`); the day strip is moved by `useCalendarStrip`
        // alone — touch swipe, trackpad burst, drag autoscroll, rebase, park —
        // and lands on a column with its own ease-out tween.
        // The strip is an offset on each day track, never `scrollLeft`, and the
        // scroller has no horizontal scroll range at all: its content is exactly
        // its own width and each band clips a wider track. On iOS a native
        // vertical pan makes WebKit report its own x for a non-user-scrollable
        // axis and overwrite a programmatic `scrollLeft` mid-gesture — the strip
        // then read index 0, rebased a period back, and rewound the calendar on
        // every vertical drag. `useCalendarStrip` writes `transform` straight to
        // the three tracks below, which the browser never touches.
        // No CSS scroll-snap: `snap-x snap-mandatory` re-snapped every
        // programmatic write. The tween is the only snap.
      >
        {/* Content: exactly the scroller's width, so there is no x scroll range */}
        <div className="min-h-full">
          {/* Day headers — sticky top */}
          <div
            className="sticky top-0 z-20 flex bg-cream border-b border-border"
            style={{ height: COL_HEADER_H }}
          >
            <div
              className="relative shrink-0 z-40 flex items-center justify-center border-r border-border/60 bg-cream"
              style={{ width: gutterW }}
              aria-hidden
            >
              {/* The 52px gutter is full of time pills mid-drag, so the
                  1-minute badge lives in this empty day-header corner. */}
              {precisionSnap && (
                <span className="text-[10px] font-medium tabular-nums text-foreground">
                  1m
                </span>
              )}
            </div>
            {/* Track viewport: clips the translated day track */}
            <div className="flex-1 min-w-0 overflow-hidden">
              <div
                ref={stripTrackRef}
                data-strip-track
                className="flex will-change-transform"
                style={{
                  width: trackWidth,
                  height: '100%',
                }}
              >
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
            </div>
          </div>

          {/* All-day band — sticky under headers */}
          <div className="sticky z-20 bg-cream" style={{ top: COL_HEADER_H }}>
            <AllDayRow
              days={days}
              colWidth={colW}
              gutterWidth={gutterW}
              stripTrackRef={stripTrackRef}
              height={allDayHeight}
              chips={allDayChips}
              todayIndex={todayIndex}
              onDayPointerDown={onAllDayPointerDown}
              onChipPointerDown={onAllDayChipPointerDown}
              onChipSelect={onChipSelect}
              selectedEventId={selectedEventId}
              draggingEventId={draggingEventId}
              preview={allDayPreview}
              nowMs={now.getTime()}
            />
          </div>

          {/* Hours: fixed gutter + translated day columns */}
          <div
            className={cn('relative flex', isDragging && 'select-none')}
            style={{ height: totalHoursH }}
          >
            {/* Time gutter — fixed: nothing scrolls sideways under it */}
            <div
              className="relative shrink-0 z-30 border-r border-border/60 bg-cream"
              style={{ width: gutterW, height: totalHoursH }}
            >
              <GutterTimes
                hourH={hourH}
                hourLabels={hourLabels}
                now={todayIndex != null ? now : null}
                edges={gutterEdges}
                headerOffset={COL_HEADER_H + allDayHeight}
                onEdgePointerDown={onGutterEdgePointerDown}
              />
            </div>

            {/* Track viewport: clips the translated hours track */}
            <div className="flex-1 min-w-0 overflow-hidden">
              <div
                ref={stripTrackRef}
                data-strip-track
                className="relative flex will-change-transform"
                style={{
                  width: trackWidth,
                  height: totalHoursH,
                }}
              >
                {/* Day columns — memoized; drag preview is a sibling overlay */}
                {days.map((day) => {
                  const isTodayCol = isSameDay(day, today);
                  const dayKey = day.toDateString();
                  const dayEvents = eventsByDay.get(dayKey) ?? EMPTY_DAY_EVENTS;

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
                      nowMs={now.getTime()}
                      onColumnPointerDown={onColumnPointerDown}
                      onChipPointerDown={onChipPointerDown}
                      onSelectChip={onSelectChip}
                      onRequestDelete={onRequestDelete}
                    />
                  );
                })}

                {/* Inside the track so the ghosts stay on their columns */}
                <TimedPreviewLayer
                  days={days}
                  colW={colW}
                  trackWidth={trackWidth}
                  totalHoursH={totalHoursH}
                  timedPreviewByDay={timedPreviewByDay}
                  previewInfo={previewInfo}
                  previewColor={previewColor}
                  previewTitle={previewTitle}
                />
              </div>
            </div>
          </div>
        </div>
      </div>

      {/* Loading overlay — grid stays mounted and measurable underneath */}
      {isLoading && eventsEmpty && (
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
      {error && eventsEmpty && !isLoading && (
        <div className="absolute inset-0 z-50 flex flex-col items-center justify-center bg-cream/70 text-center px-4">
          <p className="text-foreground font-medium mb-2">
            Failed to load events
          </p>
          <p className="selectable text-sm text-muted-foreground mb-4">
            {error}
          </p>
          <Button variant="outline" onClick={onRetry}>
            <RefreshCw className="h-4 w-4 mr-2" />
            Retry
          </Button>
        </div>
      )}
    </div>
  );
}
