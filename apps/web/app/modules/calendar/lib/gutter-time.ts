// Pure layout helpers for the sticky time gutter: the current-time pill and
// the selected / dragged event's start and end pills.
// No React. Deterministic. Native Date only.

import { MINUTES_PER_DAY, minutesSinceMidnight, nowLineY } from './week-layout';

/**
 * Collision box for a gutter pill. The painted pill is `text-[10px]
 * leading-none` plus `py-0.5` (14px); the taller box keeps it from touching
 * an hour label.
 */
export const GUTTER_PILL_H = 16;
/** Painted hour-label height: `text-[10px] leading-none`. */
export const GUTTER_HOUR_LABEL_H = 10;

function clamp(n: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, n));
}

/**
 * Compact 12-hour single instant: "9:05a", "12:30p".
 *
 * `formatEventTime` ("12:30 PM") plus pill padding does not fit the 52px
 * gutter, and the hour labels already carry the spelled meridian ("9AM"), so
 * a colon plus a lowercase letter stays visually distinct from them.
 * Seconds are ignored, like `formatEventTime`.
 */
export function formatGutterTime(date: Date): string {
  const h = date.getHours();
  const m = date.getMinutes();
  const h12 = h % 12 === 0 ? 12 : h % 12;
  return `${h12}:${String(m).padStart(2, '0')}${h < 12 ? 'a' : 'p'}`;
}

/** Exactly local midnight: hours, minutes, seconds and ms all zero. */
function isLocalMidnight(date: Date): boolean {
  return (
    date.getHours() === 0 &&
    date.getMinutes() === 0 &&
    date.getSeconds() === 0 &&
    date.getMilliseconds() === 0
  );
}

/**
 * Minutes since midnight for both ends of a range, on the gutter's one 24h
 * axis.
 *
 * An end that is exactly local midnight *and* strictly after the start reads
 * 1440, so a 23:00–midnight event bottoms out instead of jumping to the top
 * of the axis. A zero-length event at midnight is not "strictly after", so it
 * stays at 0. Inverted ranges are not reordered: each instant keeps its own
 * clock. Non-finite instants → 0 / 0.
 */
export function gutterEdgeMinutes(
  start: Date,
  end: Date,
): { startMin: number; endMin: number } {
  const startMs = start.getTime();
  const endMs = end.getTime();
  if (!Number.isFinite(startMs) || !Number.isFinite(endMs)) {
    return { startMin: 0, endMin: 0 };
  }
  const startMin = clamp(minutesSinceMidnight(start), 0, MINUTES_PER_DAY);
  const endMin =
    endMs > startMs && isLocalMidnight(end)
      ? MINUTES_PER_DAY
      : clamp(minutesSinceMidnight(end), 0, MINUTES_PER_DAY);
  return { startMin, endMin };
}

export interface GutterPillLayout {
  key: 'now' | 'start' | 'end';
  /** Center Y in the hours area, clamped inside the hours box. */
  top: number;
  text: string;
  minutes: number;
}

export interface GutterPillInput {
  hourH: number;
  /** Hours the gutter would paint (already through `hourLabelStep`). */
  hourLabels: readonly number[];
  /** Null when today is not in the rendered strip. */
  now: Date | null;
  /** Null when nothing timed is selected and no timed drag preview is active. */
  edges: { start: Date; end: Date } | null;
}

/**
 * Pills and the hour labels they cover, for the sticky gutter.
 *
 * Content-box clamping only: a pill on 00:00 or on 1440 keeps its center
 * inside the hours box instead of being sliced in half. Viewport stickiness
 * and push-apart are deliberately absent, so short events may overlap.
 */
export function layoutGutterPills(input: GutterPillInput): {
  pills: GutterPillLayout[];
  hiddenHours: number[];
} {
  const { hourH, hourLabels, now, edges } = input;
  if (!Number.isFinite(hourH) || hourH <= 0) {
    return { pills: [], hiddenHours: [] };
  }

  const clampCenter = (y: number): number =>
    clamp(y, GUTTER_PILL_H / 2, hourH * 24 - GUTTER_PILL_H / 2);

  const pills: GutterPillLayout[] = [];
  if (edges) {
    const startMs = edges.start.getTime();
    const endMs = edges.end.getTime();
    if (Number.isFinite(startMs) && Number.isFinite(endMs)) {
      const { startMin, endMin } = gutterEdgeMinutes(edges.start, edges.end);
      // Text comes from the instants, not from the clamped minutes: the
      // midnight end pill reads "12:00a" while sitting at 1440.
      pills.push({
        key: 'start',
        top: clampCenter((startMin / 60) * hourH),
        text: formatGutterTime(edges.start),
        minutes: startMin,
      });
      pills.push({
        key: 'end',
        top: clampCenter((endMin / 60) * hourH),
        text: formatGutterTime(edges.end),
        minutes: endMin,
      });
    }
  }

  if (now && Number.isFinite(now.getTime())) {
    const top = clampCenter(nowLineY(now, hourH));
    const edgeTops = pills.map((p) => p.top);
    // The event's own times win: a now pill drawn over an edge is unreadable.
    // Exact adjacency is not a collision.
    if (!edgeTops.some((edgeTop) => Math.abs(edgeTop - top) < GUTTER_PILL_H)) {
      pills.push({
        key: 'now',
        top,
        text: formatGutterTime(now),
        minutes: minutesSinceMidnight(now),
      });
    }
  }

  // A label under a pill would show the same time twice; drop the label.
  const labelClearance = (GUTTER_PILL_H + GUTTER_HOUR_LABEL_H) / 2;
  const hiddenHours = hourLabels.filter((h) =>
    pills.some((p) => Math.abs(h * hourH - p.top) < labelClearance),
  );

  return { pills, hiddenHours };
}
