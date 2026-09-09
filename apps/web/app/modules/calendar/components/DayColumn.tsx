import { memo, type PointerEvent as ReactPointerEvent } from 'react';
import type { CalendarEvent } from '@/app/types';
import { cn } from '@/lib/utils';
import { EventChip, PreviewChip, type PositionedEvent } from './EventChip';
import { nowLineY } from '../lib/week-layout';

const NOW_LINE_COLOR = '#F04842'; // Notion --secondary500

export interface DayColumnProps {
  day: Date;
  colW: number;
  totalHoursH: number;
  hourH: number;
  hourGridBg: string;
  events: PositionedEvent[];
  selectedEventId: string | null;
  /** Event id being moved/resized, or null. Stable across pointermove. */
  draggingEventId: string | null;
  isToday: boolean;
  isWeekend: boolean;
  showNow: boolean;
  /** Only needed when showNow; omit otherwise so memo stays stable. */
  now?: Date;
  onColumnPointerDown: (e: ReactPointerEvent<HTMLElement>, day: Date) => void;
  onChipPointerDown: (
    e: ReactPointerEvent<HTMLElement>,
    event: CalendarEvent,
    chipEl: HTMLElement,
  ) => void;
  onSelectChip: (eventId: string) => void;
}

/**
 * One day column of the timed grid. Memoized so drag pointermove (preview
 * only) does not rebuild hour lines or chips across all 21 columns.
 */
export const DayColumn = memo(function DayColumn({
  day,
  colW,
  totalHoursH,
  hourH,
  hourGridBg,
  events,
  selectedEventId,
  draggingEventId,
  isToday,
  isWeekend,
  showNow,
  now,
  onColumnPointerDown,
  onChipPointerDown,
  onSelectChip,
}: DayColumnProps) {
  return (
    <div
      data-day-col={day.toISOString()}
      className={cn(
        'relative shrink-0 border-r last:border-r-0 cursor-pointer',
        isWeekend ? 'border-border/60' : 'border-border/40',
        isToday ? 'bg-primary/[0.03]' : isWeekend && 'bg-muted/40',
      )}
      style={{
        width: colW,
        height: totalHoursH,
        backgroundImage: hourGridBg,
      }}
      onPointerDown={(e) => onColumnPointerDown(e, day)}
    >
      {events.map((p) => (
        <EventChip
          key={p.event.id}
          positioned={p}
          selected={p.event.id === selectedEventId}
          onSelect={onSelectChip}
          onPointerDown={onChipPointerDown}
          dragging={draggingEventId === p.event.id}
        />
      ))}

      {showNow && now && (
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
});

export interface TimedPreviewLayerProps {
  days: Date[];
  colW: number;
  gutterW: number;
  trackWidth: number;
  totalHoursH: number;
  timedPreviewByDay: Map<string, { top: number; height: number }>;
  previewColor: string;
  previewTitle: string;
}

/**
 * Single sibling overlay for drag ghosts. Isolated so DayColumn memo is not
 * busted when preview geometry updates on every pointermove.
 */
export const TimedPreviewLayer = memo(function TimedPreviewLayer({
  days,
  colW,
  gutterW,
  trackWidth,
  totalHoursH,
  timedPreviewByDay,
  previewColor,
  previewTitle,
}: TimedPreviewLayerProps) {
  if (timedPreviewByDay.size === 0) return null;

  return (
    <div
      className="absolute pointer-events-none z-20"
      style={{
        left: gutterW,
        top: 0,
        width: trackWidth,
        height: totalHoursH,
      }}
      aria-hidden
    >
      {days.map((day, i) => {
        const ghost = timedPreviewByDay.get(day.toDateString());
        if (!ghost) return null;
        return (
          <div
            key={day.toISOString()}
            className="absolute"
            style={{
              left: i * colW,
              width: colW,
              height: totalHoursH,
            }}
          >
            <PreviewChip
              top={ghost.top}
              height={ghost.height}
              color={previewColor}
              title={previewTitle}
            />
          </div>
        );
      })}
    </div>
  );
});
