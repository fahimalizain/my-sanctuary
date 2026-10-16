import {
  memo,
  useLayoutEffect,
  useRef,
  type PointerEvent as ReactPointerEvent,
} from 'react';
import type { CalendarEvent } from '@/app/types';
import { cn } from '@/lib/utils';
import { EventChip, PreviewChip, type PositionedEvent } from './EventChip';
import type { DragPreviewInfo } from '../lib/calendar-drag';
import {
  formatEventDuration,
  formatEventTimeRange,
  nowLineY,
} from '../lib/week-layout';

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
        'relative shrink-0 snap-start border-r last:border-r-0 cursor-pointer',
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
  previewInfo: DragPreviewInfo | null;
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
  previewInfo,
  previewColor,
  previewTitle,
}: TimedPreviewLayerProps) {
  if (timedPreviewByDay.size === 0) return null;

  const label = previewInfo
    ? `${formatEventTimeRange(previewInfo.range.start, previewInfo.range.end)} · ${formatEventDuration(previewInfo.range.start, previewInfo.range.end)}`
    : null;
  // Label once: on the segment holding the dragged edge, else the first.
  const segmentDays = days.filter((d) =>
    timedPreviewByDay.has(d.toDateString()),
  );
  const labelDay =
    previewInfo?.kind === 'resize-end'
      ? segmentDays[segmentDays.length - 1]
      : segmentDays[0];
  const isTouch = previewInfo?.pointerType === 'touch';

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
              label={day === labelDay ? label : null}
              floatingLabel={isTouch}
            />
            {day === labelDay && label && isTouch && previewInfo && (
              <FloatingTimePill
                label={label}
                top={ghost.top}
                bottom={ghost.top + ghost.height}
                below={previewInfo.kind === 'resize-end'}
                gutterW={gutterW}
              />
            )}
          </div>
        );
      })}
    </div>
  );
});

const FLOAT_GAP_PX = 28;
const FLOAT_MARGIN_PX = 4;

/**
 * Touch-only pill held clear of the finger. Positioned and clamped to the
 * visible scroller area in a layout effect (every render, i.e. every move),
 * so it never reaches React state.
 */
function FloatingTimePill({
  label,
  top,
  bottom,
  below,
  gutterW,
}: {
  label: string;
  top: number;
  bottom: number;
  below: boolean;
  gutterW: number;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const el = ref.current;
    const layer = el?.parentElement; // the day wrapper (top = grid top)
    const scroller = document.querySelector('[data-calendar-scroller]');
    if (!el || !layer || !scroller) return;
    const s = scroller.getBoundingClientRect();
    const l = layer.getBoundingClientRect();
    // Sticky day header + all-day band cover the top of the scroller.
    const allDay = document.querySelector('[data-allday-row]');
    const visTop =
      Math.max(s.top, allDay ? allDay.getBoundingClientRect().bottom : s.top) -
      l.top;
    const visBottom = s.bottom - l.top;
    const visLeft = s.left + gutterW - l.left;
    const visRight = s.right - l.left;
    const h = el.offsetHeight;
    const w = el.offsetWidth;
    const want = below ? bottom + FLOAT_GAP_PX : top - FLOAT_GAP_PX - h;
    const y = Math.max(
      visTop + FLOAT_MARGIN_PX,
      Math.min(visBottom - h - FLOAT_MARGIN_PX, want),
    );
    el.style.top = `${y}px`;
    el.style.left = `${Math.max(
      visLeft + FLOAT_MARGIN_PX,
      Math.min(visRight - w - FLOAT_MARGIN_PX, 0),
    )}px`;
  });
  return (
    <div
      ref={ref}
      className="absolute z-[60] whitespace-nowrap rounded-full bg-foreground px-2 py-0.5 text-[11px] font-medium leading-4 tabular-nums text-background shadow-md pointer-events-none"
      style={{ top, left: 0 }}
    >
      {label}
    </div>
  );
}
