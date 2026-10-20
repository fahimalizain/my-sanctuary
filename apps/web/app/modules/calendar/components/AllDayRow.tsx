import type { PointerEvent as ReactPointerEvent } from 'react';
import {
  ALLDAY_CHIP,
  ALLDAY_GAP,
  ALLDAY_PAD,
  STRIP_TRACK_TRANSFORM,
  isWeekend,
  formatEventDateLine,
  lastOccupiedCivilDate,
} from '../lib/week-layout';
import { isEventPast, notionEventCssVars } from '../lib/event-colors';
import { cn } from '@/lib/utils';

export interface AllDayChip {
  id: string;
  title: string;
  /** Inclusive day index into the rendered strip window. */
  startDay: number;
  /** Inclusive day index into the rendered strip window. */
  endDay: number;
  lane: number;
  color: string;
  /** End instant (ms), for the past tick. Layout data only, not stored color. */
  endMs: number;
}

export interface AllDayPreview {
  /** Inclusive day index into `days`. */
  startDay: number;
  /** Inclusive day index into `days`. */
  endDay: number;
  color?: string;
  title?: string;
  /** Previewed range, for the date label. */
  range?: { start: Date; end: Date } | null;
}

export interface AllDayRowProps {
  /** Rendered strip window days (typically 21). */
  days: Date[];
  colWidth: number;
  gutterWidth: number;
  height: number;
  chips: AllDayChip[];
  /** Highlight today's column index into `days`, or null. */
  todayIndex?: number | null;
  /** Empty-cell pointerdown for all-day create / drag-create. */
  onDayPointerDown?: (e: ReactPointerEvent<HTMLElement>, day: Date) => void;
  onChipPointerDown?: (
    e: ReactPointerEvent<HTMLElement>,
    eventId: string,
    chipEl: HTMLElement,
  ) => void;
  /** Chip click/tap selects the event. */
  onChipSelect?: (eventId: string) => void;
  selectedEventId?: string | null;
  draggingEventId?: string | null;
  /** Ghost range while drag-creating across all-day cells. */
  preview?: AllDayPreview | null;
  /** Current clock (ms) for the past tick; threaded from the session tick. */
  nowMs: number;
}

export function AllDayRow({
  days,
  colWidth,
  gutterWidth,
  height,
  chips,
  todayIndex = null,
  onDayPointerDown,
  onChipPointerDown,
  onChipSelect,
  selectedEventId = null,
  draggingEventId = null,
  preview = null,
  nowMs,
}: AllDayRowProps) {
  const trackWidth = days.length * colWidth;

  return (
    <div
      className="flex border-b border-border bg-cream"
      style={{ height }}
      data-allday-row
    >
      {/* Gutter label — fixed: the day track scrolls under its right edge */}
      <div
        className="shrink-0 flex items-start justify-end pr-1.5 pt-1 border-r border-border/60 bg-cream"
        style={{ width: gutterWidth }}
      >
        <span className="text-[10px] leading-none text-muted-foreground select-none">
          All-day
        </span>
      </div>

      {/* Track viewport: clips the translated chip track */}
      <div className="flex-1 min-w-0 overflow-hidden">
        <div
          className="relative"
          style={{
            width: trackWidth,
            height: '100%',
            transform: STRIP_TRACK_TRANSFORM,
          }}
        >
          {/* Column hairlines + weekend / today wash (decorative) */}
          <div className="absolute inset-0 flex pointer-events-none">
            {days.map((day, i) => {
              const weekend = isWeekend(day);
              const isToday = todayIndex === i;
              return (
                <div
                  key={day.toISOString()}
                  className={cn(
                    'shrink-0 border-r last:border-r-0',
                    weekend ? 'border-border/60' : 'border-border/40',
                    isToday ? 'bg-primary/[0.03]' : weekend && 'bg-muted/40',
                  )}
                  style={{ width: colWidth }}
                />
              );
            })}
          </div>

          {/* Hit layer for empty-cell create (under chips). */}
          <div className="absolute inset-0 flex z-0">
            {days.map((day) => (
              <div
                key={`hit-${day.toISOString()}`}
                data-allday-day={day.toISOString()}
                className="shrink-0 h-full cursor-pointer"
                style={{ width: colWidth }}
                onPointerDown={(e) => onDayPointerDown?.(e, day)}
              />
            ))}
          </div>

          {chips.map((chip) => {
            const daySpan = chip.endDay - chip.startDay + 1;
            const left = chip.startDay * colWidth;
            const width = daySpan * colWidth - 2;
            const top = (ALLDAY_CHIP + ALLDAY_GAP) * chip.lane + ALLDAY_PAD;
            const selected = chip.id === selectedEventId;
            const dragging = chip.id === draggingEventId;
            const past = isEventPast(chip.endMs, nowMs);

            return (
              <div
                key={chip.id}
                data-allday-chip
                data-event-id={chip.id}
                role="button"
                tabIndex={0}
                className={cn(
                  'group absolute overflow-hidden rounded-[6px] pointer-events-auto z-[1] cursor-grab active:cursor-grabbing',
                  'bg-[var(--ev-fill)] dark:bg-[var(--ev-fill-dark)]',
                  'text-[var(--ev-title)] dark:text-[var(--ev-title-dark)]',
                  'data-[past]:text-[var(--ev-title-dim)] dark:data-[past]:text-[var(--ev-title-dim-dark)]',
                  '[outline-style:solid] outline-1 outline-cream',
                  selected && 'ring-1 ring-foreground/25',
                  dragging && 'opacity-40',
                )}
                style={{
                  ...notionEventCssVars(chip.color),
                  top,
                  height: ALLDAY_CHIP,
                  left: left + 1,
                  width: Math.max(0, width),
                }}
                title={chip.title}
                data-past={past ? '' : undefined}
                onClick={(e) => {
                  e.stopPropagation();
                  onChipSelect?.(chip.id);
                }}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' || e.key === ' ') {
                    e.preventDefault();
                    e.stopPropagation();
                    onChipSelect?.(chip.id);
                  }
                }}
                onPointerDown={(e) => {
                  onChipPointerDown?.(e, chip.id, e.currentTarget);
                }}
              >
                {/* 4px left ribbon — accent tone so it reads on the washed fill */}
                <div
                  className={cn(
                    'absolute left-0 top-0 bottom-0 w-1 rounded-l-[6px]',
                    'bg-[var(--ev-accent)] dark:bg-[var(--ev-accent-dark)]',
                    'group-data-[past]:bg-[var(--ev-accent-dim)] dark:group-data-[past]:bg-[var(--ev-accent-dim-dark)]',
                  )}
                  aria-hidden
                />
                <div className="h-full pl-2 pr-1 py-px">
                  <div className="truncate text-[11px] font-medium leading-[17px]">
                    {chip.title}
                  </div>
                </div>
              </div>
            );
          })}

          {/* Drag-create ghost */}
          {preview &&
            preview.endDay >= preview.startDay &&
            (() => {
              const daySpan = preview.endDay - preview.startDay + 1;
              const left = preview.startDay * colWidth;
              const width = daySpan * colWidth - 2;
              const color = preview.color ?? '#2a5c8a';
              return (
                <div
                  className={cn(
                    'absolute overflow-hidden rounded-[6px] pointer-events-none z-20 opacity-70',
                    'bg-[var(--ev-fill)] dark:bg-[var(--ev-fill-dark)]',
                    'text-[var(--ev-title)] dark:text-[var(--ev-title-dark)]',
                    '[outline-style:solid] outline-1 outline-cream',
                  )}
                  style={{
                    ...notionEventCssVars(color),
                    top: ALLDAY_PAD,
                    height: ALLDAY_CHIP,
                    left: left + 1,
                    width: Math.max(0, width),
                  }}
                  aria-hidden
                >
                  <div className="absolute left-0 top-0 bottom-0 w-1 rounded-l-[6px] bg-[var(--ev-accent)] dark:bg-[var(--ev-accent-dark)]" />
                  <div className="h-full pl-2 pr-1 py-px">
                    <div className="truncate text-[11px] font-medium leading-[17px]">
                      {preview.title ?? ''}
                      {preview.range && (
                        <span
                          className={cn(
                            'ml-1.5 text-[10px] font-normal tabular-nums',
                            'text-[var(--ev-time)] dark:text-[var(--ev-time-dark)]',
                          )}
                        >
                          {formatEventDateLine(
                            preview.range.start,
                            lastOccupiedCivilDate(
                              preview.range.start,
                              preview.range.end,
                            ),
                          )}
                        </span>
                      )}
                    </div>
                  </div>
                </div>
              );
            })()}
        </div>
      </div>
    </div>
  );
}
