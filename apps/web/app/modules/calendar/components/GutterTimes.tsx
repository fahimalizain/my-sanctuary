import {
  useEffect,
  useState,
  type PointerEvent as ReactPointerEvent,
} from 'react';
import { layoutGutterPills } from '../lib/gutter-time';
import { formatHourLabel } from '../lib/week-layout';

/** Same hex as NOW_LINE_COLOR in DayColumn.tsx (Notion --secondary500). */
const NOW_PILL_COLOR = '#F04842';

const PILL_BASE =
  'absolute right-0.5 -translate-y-1/2 z-10 whitespace-nowrap rounded-full px-1 py-0.5 text-[10px] font-medium leading-none tabular-nums';

/** Decorative pill: the chip and the inspector already announce the time. */
const PILL_CLASS = `${PILL_BASE} pointer-events-none`;

/** Edge pill: grabbable, so the pointer-events opt-out is dropped. */
const EDGE_PILL_CLASS = `${PILL_BASE} touch-none cursor-ns-resize select-none`;

const HOUR_LABEL_CLASS =
  'absolute right-1 -translate-y-1/2 text-[10px] leading-none text-muted-foreground tabular-nums select-none';

export interface GutterTimesProps {
  hourH: number;
  hourLabels: number[];
  /** Null when today is not in the rendered strip. */
  now: Date | null;
  /** Null when nothing timed is selected and no timed drag preview is active. */
  edges: { start: Date; end: Date } | null;
  /** Sticky band above the hours: `COL_HEADER_H + allDayHeight`. */
  headerOffset: number;
  /**
   * Grab an edge pill. Without it the pills stay decorative: no pointer
   * events, no slider role, no focus.
   */
  onEdgePointerDown?: (
    e: ReactPointerEvent<HTMLElement>,
    edge: 'start' | 'end',
  ) => void;
}

/**
 * Hour labels plus the now / start / end pills, inside the sticky gutter.
 * A pill's `top` is its center Y in the hours area, so the painted pill is
 * vertically centered on its instant.
 *
 * The pills follow the scroller: an edge scrolled out of the visible hours
 * band sticks to its edge, which keeps it on screen and grabbable. The scroll
 * position is local state so day columns do not re-render.
 */
export function GutterTimes({
  hourH,
  hourLabels,
  now,
  edges,
  headerOffset,
  onEdgePointerDown,
}: GutterTimesProps) {
  const [viewport, setViewport] = useState<{
    scrollTop: number;
    clientHeight: number;
  } | null>(null);

  useEffect(() => {
    const scroller = document.querySelector('[data-calendar-scroller]');
    if (!(scroller instanceof HTMLElement)) return;

    const measure = () =>
      setViewport((prev) =>
        prev &&
        prev.scrollTop === scroller.scrollTop &&
        prev.clientHeight === scroller.clientHeight
          ? prev
          : {
              scrollTop: scroller.scrollTop,
              clientHeight: scroller.clientHeight,
            },
      );
    measure();
    scroller.addEventListener('scroll', measure, { passive: true });
    const observer =
      typeof ResizeObserver === 'undefined'
        ? null
        : new ResizeObserver(measure);
    observer?.observe(scroller);
    return () => {
      scroller.removeEventListener('scroll', measure);
      observer?.disconnect();
    };
  }, []);

  // Hours-local Y of the visible hours: the sticky band covers the top of the
  // viewport, so `scrollTop` is the first hour row fully in view.
  const visible =
    viewport && viewport.clientHeight > headerOffset
      ? {
          top: viewport.scrollTop,
          bottom: viewport.scrollTop + viewport.clientHeight - headerOffset,
        }
      : null;

  const { pills, hiddenHours } = layoutGutterPills({
    hourH,
    hourLabels,
    now,
    edges,
    visible,
  });
  const hidden = new Set(hiddenHours);

  return (
    <>
      {hourLabels.map((h) => {
        const label = formatHourLabel(h);
        if (!label || hidden.has(h)) return null;
        return (
          <div key={h} className={HOUR_LABEL_CLASS} style={{ top: h * hourH }}>
            {label}
          </div>
        );
      })}

      {pills.map((pill) => {
        if (pill.key === 'now') {
          return (
            <div
              key={pill.key}
              aria-hidden
              className={`${PILL_CLASS} text-white`}
              style={{ top: pill.top, backgroundColor: NOW_PILL_COLOR }}
            >
              {pill.text}
            </div>
          );
        }

        if (!onEdgePointerDown) {
          return (
            <div
              key={pill.key}
              aria-hidden
              className={`${PILL_CLASS} bg-foreground text-background shadow-sm`}
              style={{ top: pill.top }}
            >
              {pill.text}
            </div>
          );
        }

        const edge = pill.key;
        return (
          <div
            key={pill.key}
            role="slider"
            tabIndex={-1}
            aria-orientation="vertical"
            aria-label={edge === 'start' ? 'Start time' : 'End time'}
            aria-valuetext={pill.text}
            className={`${EDGE_PILL_CLASS} bg-foreground text-background shadow-sm`}
            style={{ top: pill.top }}
            onPointerDown={(e) => {
              // A press must not start a text selection.
              e.preventDefault();
              onEdgePointerDown(e, edge);
            }}
          >
            {pill.text}
          </div>
        );
      })}
    </>
  );
}
