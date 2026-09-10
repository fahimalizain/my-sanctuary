import type { PointerEvent as ReactPointerEvent } from 'react';
import {
  ALLDAY_CHIP,
  ALLDAY_GAP,
  ALLDAY_PAD,
  contrastingInk,
  isWeekend,
  hexToRgba,
} from '../lib/week-layout';
import { cn } from '@/lib/utils';

const PREVIEW_FILL_ALPHA = 0.18;

/** Mix hex with black ~20% so the left ribbon still reads on a solid body. */
function darkerRibbon(hex: string): string {
  const h = hex.replace('#', '');
  const full =
    h.length === 3
      ? h
          .split('')
          .map((c) => c + c)
          .join('')
      : h;
  const n = parseInt(full, 16);
  if (Number.isNaN(n)) return hex;
  const r = Math.round(((n >> 16) & 255) * 0.8);
  const g = Math.round(((n >> 8) & 255) * 0.8);
  const b = Math.round((n & 255) * 0.8);
  return `rgb(${r}, ${g}, ${b})`;
}

export interface AllDayChip {
  id: string;
  title: string;
  /** Inclusive day index into the rendered strip window. */
  startDay: number;
  /** Inclusive day index into the rendered strip window. */
  endDay: number;
  lane: number;
  color: string;
}

export interface AllDayPreview {
  /** Inclusive day index into `days`. */
  startDay: number;
  /** Inclusive day index into `days`. */
  endDay: number;
  color?: string;
  title?: string;
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
}: AllDayRowProps) {
  const trackWidth = days.length * colWidth;

  return (
    <div
      className="shrink-0 flex border-b border-border bg-cream"
      style={{ height, width: gutterWidth + trackWidth }}
      data-allday-row
    >
      {/* Gutter label — sticky with the time gutter while scrolling X */}
      <div
        className="shrink-0 sticky left-0 z-30 flex items-start justify-end pr-1.5 pt-1 border-r border-border/60 bg-cream"
        style={{ width: gutterWidth }}
      >
        <span className="text-[10px] leading-none text-muted-foreground select-none">
          All-day
        </span>
      </div>

      {/* Day-column chip track (pixel-sized columns) */}
      <div className="relative shrink-0" style={{ width: trackWidth }}>
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

          return (
            <div
              key={chip.id}
              data-allday-chip
              data-event-id={chip.id}
              role="button"
              tabIndex={0}
              className={cn(
                'absolute overflow-hidden rounded-[6px] pointer-events-auto z-[1] cursor-grab active:cursor-grabbing',
                selected && 'ring-1 ring-foreground/25',
                dragging && 'opacity-40',
              )}
              style={{
                top,
                height: ALLDAY_CHIP,
                left: left + 1,
                width: Math.max(0, width),
                backgroundColor: chip.color,
                color: contrastingInk(chip.color),
              }}
              title={chip.title}
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
              {/* 4px left ribbon — darker shade so it reads on solid fill */}
              <div
                className="absolute left-0 top-0 bottom-0 w-1 rounded-l-[6px]"
                style={{ backgroundColor: darkerRibbon(chip.color) }}
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
                className="absolute overflow-hidden rounded-[6px] pointer-events-none z-20 opacity-70"
                style={{
                  top: ALLDAY_PAD,
                  height: ALLDAY_CHIP,
                  left: left + 1,
                  width: Math.max(0, width),
                  color: 'var(--foreground)',
                }}
                aria-hidden
              >
                <div
                  className="absolute left-0 top-0 bottom-0 w-1 rounded-l-[6px]"
                  style={{ backgroundColor: color }}
                />
                <div
                  className="h-full pl-2 pr-1 py-px"
                  style={{
                    backgroundColor: hexToRgba(color, PREVIEW_FILL_ALPHA),
                  }}
                >
                  <div className="truncate text-[11px] font-medium leading-[17px]">
                    {preview.title ?? ''}
                  </div>
                </div>
              </div>
            );
          })()}
      </div>
    </div>
  );
}
