import type { PointerEvent as ReactPointerEvent } from 'react';
import type { CalendarEvent } from '@/app/types';
import { cn } from '@/lib/utils';
import {
  CHIP_MARGIN_RIGHT,
  formatEventTime,
  formatEventTimeRange,
  hexToRgba,
  isCompactChip,
} from './week-layout';

const CHIP_FILL_ALPHA = 0.22;
const CHIP_FILL_ALPHA_SELECTED = 0.4;
const PREVIEW_FILL_ALPHA = 0.18;

export interface PositionedEvent {
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
  selected: boolean;
  onSelect: (eventId: string) => void;
  onPointerDown?: (
    e: ReactPointerEvent<HTMLElement>,
    event: CalendarEvent,
    chipEl: HTMLElement,
  ) => void;
  /** Dim while a drag preview is active for this event. */
  dragging?: boolean;
}

export function EventChip({
  positioned,
  selected,
  onSelect,
  onPointerDown,
  dragging = false,
}: EventChipProps) {
  const { event, top, height, col, cols, span, color, startMin, endMin } =
    positioned;
  const start = new Date(event.start_time);
  const end = new Date(event.end_time);
  const timeLabel = formatEventTime(start);
  const rangeLabel = formatEventTimeRange(start, end);
  const compact = isCompactChip(height);

  const leftPct = (col / cols) * 100;
  const widthPct = (span / cols) * 100;
  const fillAlpha = selected ? CHIP_FILL_ALPHA_SELECTED : CHIP_FILL_ALPHA;

  return (
    <div
      role="button"
      tabIndex={0}
      className={cn(
        'event-chip absolute overflow-hidden rounded-[6px] pointer-events-auto',
        'cursor-grab active:cursor-grabbing',
        selected && 'ring-1 ring-foreground/25',
        dragging && 'opacity-40',
      )}
      style={{
        top,
        height,
        left: `${leftPct}%`,
        width: `calc(${widthPct}% - ${CHIP_MARGIN_RIGHT}px)`,
        color: 'var(--foreground)',
      }}
      title={`${event.title} · ${rangeLabel}`}
      data-event-chip
      data-event-id={event.id}
      data-start-min={startMin}
      data-end-min={endMin}
      onPointerDown={(e) => {
        onPointerDown?.(e, event, e.currentTarget);
      }}
      onClick={(e) => {
        e.stopPropagation();
        // Selection is primarily handled on pointerup via the drag hook when
        // there was no drag; keep click as a fallback.
        onSelect(event.id);
      }}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          e.stopPropagation();
          onSelect(event.id);
        }
      }}
    >
      {/* Resize handles — 6px hit targets at top/bottom edges */}
      <div
        data-resize="start"
        className="absolute inset-x-0 top-0 z-[2] h-1.5 cursor-ns-resize"
        aria-hidden
      />
      <div
        data-resize="end"
        className="absolute inset-x-0 bottom-0 z-[2] h-1.5 cursor-ns-resize"
        aria-hidden
      />

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
        style={{ backgroundColor: hexToRgba(color, fillAlpha) }}
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

/** Ghost chip painted during drag-create / move / resize. */
export function PreviewChip({
  top,
  height,
  color,
  title,
}: {
  top: number;
  height: number;
  color: string;
  title: string;
}) {
  return (
    <div
      className="absolute overflow-hidden rounded-[6px] pointer-events-none z-20 opacity-70"
      style={{
        top,
        height,
        left: 0,
        right: CHIP_MARGIN_RIGHT,
        color: 'var(--foreground)',
      }}
      aria-hidden
    >
      <div
        className="absolute left-0 top-0 bottom-0 w-1 rounded-l-[6px]"
        style={{ backgroundColor: color }}
      />
      <div
        className="h-full min-w-0 pl-2 pr-1 py-px"
        style={{ backgroundColor: hexToRgba(color, PREVIEW_FILL_ALPHA) }}
      >
        <div className="truncate text-[11px] font-medium leading-[13px]">
          {title}
        </div>
      </div>
    </div>
  );
}
