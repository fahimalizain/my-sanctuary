import { memo, type PointerEvent as ReactPointerEvent } from 'react';
import type { CalendarEvent } from '@/app/types';
import { cn } from '@/lib/utils';
import {
  CHIP_MARGIN_RIGHT,
  CHIP_Z_BASE,
  CHIP_Z_SELECTED,
  contrastingInk,
  formatEventTime,
  formatEventTimeRange,
  hexToRgba,
  isCompactChip,
} from '../lib/week-layout';

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

export interface PositionedEvent {
  event: CalendarEvent;
  startMin: number;
  endMin: number;
  top: number;
  height: number;
  leftPercent: number;
  widthPercent: number;
  layerIndex: number;
  leftPixels: number;
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

export const EventChip = memo(function EventChip({
  positioned,
  selected,
  onSelect,
  onPointerDown,
  dragging = false,
}: EventChipProps) {
  const {
    event,
    top,
    height,
    leftPercent,
    widthPercent,
    layerIndex,
    leftPixels,
    color,
    startMin,
    endMin,
  } = positioned;
  const start = new Date(event.start_time);
  const end = new Date(event.end_time);
  const timeLabel = formatEventTime(start);
  const rangeLabel = formatEventTimeRange(start, end);
  const compact = isCompactChip(height);

  return (
    <div
      role="button"
      tabIndex={0}
      className={cn(
        'event-chip group absolute overflow-hidden rounded-[6px] pointer-events-auto',
        'cursor-grab active:cursor-grabbing',
        selected && 'ring-1 ring-foreground/25',
        dragging && 'opacity-40',
      )}
      style={{
        top,
        height,
        left: `${leftPixels}px`,
        marginLeft: `${leftPercent * 100}%`,
        width: `calc(${widthPercent * 100}% - ${leftPixels}px - ${CHIP_MARGIN_RIGHT}px)`,
        zIndex: selected ? CHIP_Z_SELECTED : CHIP_Z_BASE + layerIndex,
        backgroundColor: color,
        color: contrastingInk(color),
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
        className="absolute inset-x-0 top-0 z-[2] flex h-1.5 cursor-ns-resize items-center justify-center"
        aria-hidden
      >
        <span
          className={cn(
            'pointer-events-none h-[3px] w-[32%] max-w-[40px] rounded-full bg-current opacity-0 transition-opacity',
            'group-hover:opacity-45',
            selected && 'opacity-45',
          )}
        />
      </div>
      <div
        data-resize="end"
        className="absolute inset-x-0 bottom-0 z-[2] flex h-1.5 cursor-ns-resize items-center justify-center"
        aria-hidden
      >
        <span
          className={cn(
            'pointer-events-none h-[3px] w-[32%] max-w-[40px] rounded-full bg-current opacity-0 transition-opacity',
            'group-hover:opacity-45',
            selected && 'opacity-45',
          )}
        />
      </div>

      {/* 4px left ribbon — darker shade so it reads on solid fill */}
      <div
        className="absolute left-0 top-0 bottom-0 w-1 rounded-l-[6px]"
        style={{ backgroundColor: darkerRibbon(color) }}
        aria-hidden
      />
      <div
        className={cn(
          'h-full min-w-0 pl-2 pr-1',
          compact ? 'flex items-center gap-1 py-0' : 'py-px',
        )}
      >
        {compact ? (
          <>
            <span className="truncate text-[11px] font-medium leading-[13px]">
              {event.title}
            </span>
            <span className="shrink-0 text-[9px] leading-[11px] opacity-80">
              {timeLabel}
            </span>
          </>
        ) : (
          <>
            <div className="truncate text-[11px] font-medium leading-[13px]">
              {event.title}
            </div>
            <div className="mt-0.5 truncate text-[9px] leading-[11px] opacity-80">
              {timeLabel}
            </div>
          </>
        )}
      </div>
    </div>
  );
});

/** Ghost chip painted during drag-create / move / resize. */
export const PreviewChip = memo(function PreviewChip({
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
      className="absolute overflow-hidden rounded-[6px] pointer-events-none z-[50] opacity-70"
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
});
