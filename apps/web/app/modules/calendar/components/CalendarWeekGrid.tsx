import type { PointerEvent as ReactPointerEvent, RefObject } from 'react';
import { Loader2, RefreshCw } from 'lucide-react';
import { Button } from '@/components/ui/button';
import type { CalendarEvent } from '@/app/types';
import { cn } from '@/lib/utils';
import { AllDayRow, type AllDayChip, type AllDayPreview } from './AllDayRow';
import { dayNameShort } from '../lib/calendar-model';
import { DayColumn, TimedPreviewLayer } from './DayColumn';
import { type PositionedEvent } from './EventChip';
import {
  COL_HEADER_H,
  formatHourLabel,
  isSameDay,
  isWeekend,
} from '../lib/week-layout';

/** Stable empty list so DayColumn memo is not busted on empty days. */
const EMPTY_DAY_EVENTS: PositionedEvent[] = [];

export interface CalendarWeekGridProps {
  gridColumnRef: RefObject<HTMLDivElement | null>;
  scrollerRef: RefObject<HTMLDivElement | null>;
  onScroll: () => void;
  contentWidth: number;
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
  onAllDayPointerDown: (e: ReactPointerEvent<HTMLElement>, day: Date) => void;
  onAllDayChipPointerDown: (
    e: ReactPointerEvent<HTMLElement>,
    eventId: string,
    chipEl: HTMLElement,
  ) => void;
  onChipSelect: (eventId: string) => void;
  isDragging: boolean;
  timedPreviewByDay: Map<string, { top: number; height: number }>;
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
  onScroll,
  contentWidth,
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
  allDayHeight,
  allDayChips,
  todayIndex,
  eventsByDay,
  selectedEventId,
  draggingEventId,
  onColumnPointerDown,
  onChipPointerDown,
  onSelectChip,
  onAllDayPointerDown,
  onAllDayChipPointerDown,
  onChipSelect,
  isDragging,
  timedPreviewByDay,
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
        className="flex-1 min-h-0 overflow-auto touch-pan-x touch-pan-y [scrollbar-width:none] [-ms-overflow-style:none] [&::-webkit-scrollbar]:hidden"
        onScroll={onScroll}
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
          <div className="sticky z-20 bg-cream" style={{ top: COL_HEADER_H }}>
            <AllDayRow
              days={days}
              colWidth={colW}
              gutterWidth={gutterW}
              height={allDayHeight}
              chips={allDayChips}
              todayIndex={todayIndex}
              onDayPointerDown={onAllDayPointerDown}
              onChipPointerDown={onAllDayChipPointerDown}
              onChipSelect={onChipSelect}
              selectedEventId={selectedEventId}
              draggingEventId={draggingEventId}
              preview={allDayPreview}
            />
          </div>

          {/* Hours: sticky left gutter + day columns */}
          <div
            className={cn('relative flex', isDragging && 'select-none')}
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
                  onColumnPointerDown={onColumnPointerDown}
                  onChipPointerDown={onChipPointerDown}
                  onSelectChip={onSelectChip}
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
          <p className="text-sm text-muted-foreground mb-4">{error}</p>
          <Button variant="outline" onClick={onRetry}>
            <RefreshCw className="h-4 w-4 mr-2" />
            Retry
          </Button>
        </div>
      )}
    </div>
  );
}
