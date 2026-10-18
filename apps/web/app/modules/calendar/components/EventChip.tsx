import {
  memo,
  useLayoutEffect,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from 'react';
import type { CalendarEvent } from '@/app/types';
import { GripVertical, Trash2 } from 'lucide-react';
import { cn } from '@/lib/utils';
import {
  type HandleRect,
  TOUCH_HANDLE_KNOB_PX,
  touchHandleGeometry,
} from '../lib/calendar-drag';
import {
  CHIP_MARGIN_RIGHT,
  CHIP_Z_BASE,
  CHIP_Z_SELECTED,
  contrastingInk,
  formatEventTime,
  formatEventTimeDetail,
  hexToRgba,
  isCompactChip,
} from '../lib/week-layout';

const PREVIEW_FILL_ALPHA = 0.18;

// Primary input is a finger: fat handles replace the hover pills.
const COARSE_HIDE = '[@media(hover:none)_and_(pointer:coarse)]:hidden';
const COARSE_BLOCK = '[@media(hover:none)_and_(pointer:coarse)]:block';

const knobClassName =
  'pointer-events-none absolute flex items-center justify-center rounded-full ring-2 ring-white shadow-[0_1px_4px_rgba(0,0,0,0.45)]';

function TouchHandles({
  height,
  color,
  ink,
  onRequestDelete,
}: {
  height: number;
  color: string;
  ink: string;
  /** Omitted for read-only events: no delete handle. */
  onRequestDelete?: () => void;
}) {
  const rootRef = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);
  useLayoutEffect(() => {
    const el = rootRef.current;
    if (!el) return;
    const measure = () => setWidth(el.offsetWidth);
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const g = touchHandleGeometry(width, height);
  const knob = TOUCH_HANDLE_KNOB_PX;
  const knobStyle = { backgroundColor: color, color: ink };
  const handle = (
    r: HandleRect,
    [cx, cy]: [number, number],
    attrs: Record<string, string | boolean>,
    icon: ReactNode,
  ) => (
    <div
      {...attrs}
      className="pointer-events-auto absolute z-[3] touch-none"
      style={{ left: r.x, top: r.y, width: r.w, height: r.h }}
      aria-hidden
    >
      <span
        className={knobClassName}
        style={{
          ...knobStyle,
          width: knob,
          height: knob,
          left: cx - knob / 2 - r.x,
          top: cy - knob / 2 - r.y,
        }}
      >
        {icon}
      </span>
    </div>
  );
  const bar = <span className="h-0.5 w-2.5 rounded-full bg-current" />;
  // Overlay matching the chip box; only the handles take pointers.
  return (
    <div
      ref={rootRef}
      className={cn(
        'pointer-events-none absolute inset-0 hidden',
        COARSE_BLOCK,
      )}
    >
      {width > 0 && (
        <>
          {handle(g.start, g.knobs.start, { 'data-resize': 'start' }, bar)}
          {handle(g.end, g.knobs.end, { 'data-resize': 'end' }, bar)}
          {handle(
            g.move,
            g.knobs.move,
            { 'data-move-handle': true },
            <GripVertical className="h-3.5 w-3.5" />,
          )}
          {onRequestDelete && g.remove && g.knobs.remove && (
            // Tap-only: no touch-none (a pan may still scroll) and pointerdown
            // never reaches the chip/column, so it can't drag, create or deselect.
            <button
              type="button"
              data-delete-handle
              aria-label="Delete event"
              className="pointer-events-auto absolute z-[3]"
              style={{
                left: g.remove.x,
                top: g.remove.y,
                width: g.remove.w,
                height: g.remove.h,
              }}
              onPointerDown={(e) => e.stopPropagation()}
              onClick={(e) => {
                e.stopPropagation();
                onRequestDelete();
              }}
            >
              <span
                className={cn(knobClassName, 'bg-destructive text-white')}
                style={{
                  width: knob,
                  height: knob,
                  left: g.knobs.remove[0] - knob / 2 - g.remove.x,
                  top: g.knobs.remove[1] - knob / 2 - g.remove.y,
                }}
              >
                <Trash2 className="h-3 w-3" />
              </span>
            </button>
          )}
        </>
      )}
    </div>
  );
}

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
  /** Touch delete handle; leave undefined for read-only events. */
  onRequestDelete?: () => void;
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
  onRequestDelete,
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
  // Full "range · duration" detail: the tall chip's second line, and the
  // tooltip for both layouts (a compact or truncated line hides it otherwise).
  const timeDetail = formatEventTimeDetail(start, end);
  const compact = isCompactChip(height);
  const ink = contrastingInk(color);

  return (
    <div
      role="button"
      tabIndex={0}
      className={cn(
        'event-chip group absolute rounded-[6px] pointer-events-auto',
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
        color: ink,
      }}
      title={`${event.title} · ${timeDetail}`}
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
      {/* Clipped body; the root stays unclipped so touch handles can overflow. */}
      <div
        className="absolute inset-0 overflow-hidden rounded-[6px]"
        style={{ backgroundColor: color }}
      >
        {/* Resize handles — 6px hit targets at top/bottom edges */}
        <div
          data-resize="start"
          className={cn(
            'absolute inset-x-0 top-0 z-[2] flex h-1.5 cursor-ns-resize items-center justify-center',
            COARSE_HIDE,
          )}
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
          className={cn(
            'absolute inset-x-0 bottom-0 z-[2] flex h-1.5 cursor-ns-resize items-center justify-center',
            COARSE_HIDE,
          )}
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
                {timeDetail}
              </div>
            </>
          )}
        </div>
      </div>
      {selected && (
        <TouchHandles
          height={height}
          color={color}
          ink={ink}
          onRequestDelete={onRequestDelete}
        />
      )}
    </div>
  );
});

/** Below this the ghost can't fit a second line; the gutter carries the time. */
const PREVIEW_LABEL_FIT_PX = 30;

/** Ghost chip painted during drag-create / move / resize. */
export const PreviewChip = memo(function PreviewChip({
  top,
  height,
  color,
  title,
  label = null,
}: {
  top: number;
  height: number;
  color: string;
  title: string;
  /** Live time range; null on day segments that don't carry the label. */
  label?: string | null;
}) {
  const fits = height >= PREVIEW_LABEL_FIT_PX;
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
        {label && fits && (
          <div className="truncate text-[10px] leading-[13px] tabular-nums opacity-80">
            {label}
          </div>
        )}
      </div>
    </div>
  );
});
