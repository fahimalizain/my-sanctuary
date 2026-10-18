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
/** Clear space between the two edge pills once they are pushed apart. */
export const GUTTER_PILL_GAP = 2;

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

/**
 * Place the start / end pill centers so both stay grabbable.
 *
 * Each center is clamped into `[minCenter, maxCenter]` first, which is what
 * sticks an edge that scrolled out of the hours viewport to the visible end.
 * A pair closer than `minSep` is then pushed apart about its midpoint and slid
 * back inside the range, so a short event hosts two pills instead of one.
 *
 * The pair's own order is kept: an overnight event (end above start) is pushed
 * apart in its own direction rather than swapped. A range shorter than `minSep`
 * is spread as far as it fits and may overlap — never an error. Non-finite
 * centers pin to `minCenter`, and neither finite gives `{minCenter, minCenter}`.
 */
export function separateEdgeCenters(
  startTop: number,
  endTop: number,
  minCenter: number,
  maxCenter: number,
  minSep: number,
): { startTop: number; endTop: number } {
  const lo = Number.isFinite(minCenter) ? minCenter : 0;
  const hi = Number.isFinite(maxCenter) ? Math.max(lo, maxCenter) : lo;
  const pin = (y: number): number =>
    Number.isFinite(y) ? clamp(y, lo, hi) : lo;

  const start = pin(startTop);
  const end = pin(endTop);
  if (!Number.isFinite(startTop) && !Number.isFinite(endTop)) {
    return { startTop: start, endTop: start };
  }

  const sep = Number.isFinite(minSep) && minSep > 0 ? minSep : 0;
  const gap = Math.abs(end - start);
  const want = Math.min(sep, hi - lo);
  if (gap >= want) return { startTop: start, endTop: end };

  const half = (want - gap) / 2;
  const dir = end >= start ? 1 : -1;
  const pushedStart = start - dir * half;
  const pushedEnd = end + dir * half;

  // Slide the pair back inside; the clamp covers a range too short to hold it.
  const shiftUp = Math.max(0, lo - Math.min(pushedStart, pushedEnd));
  const shiftDown = Math.min(0, hi - Math.max(pushedStart, pushedEnd));
  const shift = shiftUp > 0 ? shiftUp : shiftDown;
  return {
    startTop: clamp(pushedStart + shift, lo, hi),
    endTop: clamp(pushedEnd + shift, lo, hi),
  };
}

export interface GutterPillInput {
  hourH: number;
  /** Hours the gutter would paint (already through `hourLabelStep`). */
  hourLabels: readonly number[];
  /** Null when today is not in the rendered strip. */
  now: Date | null;
  /** Null when nothing timed is selected and no timed drag preview is active. */
  edges: { start: Date; end: Date } | null;
  /**
   * Visible hours band in hours-local Y (0 at midnight): `scrollTop` to
   * `scrollTop + clientHeight - headerOffset`. Null / empty when unknown.
   */
  visible?: { top: number; bottom: number } | null;
}

/**
 * Pills and the hour labels they cover, for the sticky gutter.
 *
 * The two edge pills are clamped into the hours box — narrowed to the visible
 * band when one is known, so a pill whose edge scrolled away stays on screen
 * and grabbable — and keep `GUTTER_PILL_H + GUTTER_PILL_GAP` apart, so a short
 * event hosts both. Sticking is the edges' alone, because only they are
 * grabbable: the now pill is clamped to the hours box and never to the band, so
 * it stays on the now line instead of pinning to a viewport end the scroller
 * has moved away from. It yields only when its own center lands within
 * `GUTTER_PILL_H` of a placed edge pill. The now pill and the hidden labels are
 * tested against the centers as painted, not against the raw edge minutes.
 */
export function layoutGutterPills(input: GutterPillInput): {
  pills: GutterPillLayout[];
  hiddenHours: number[];
} {
  const { hourH, hourLabels, now, edges, visible } = input;
  if (!Number.isFinite(hourH) || hourH <= 0) {
    return { pills: [], hiddenHours: [] };
  }

  const boxLo = GUTTER_PILL_H / 2;
  const boxHi = hourH * 24 - GUTTER_PILL_H / 2;
  let minCenter = boxLo;
  let maxCenter = boxHi;
  if (visible && visible.bottom > visible.top) {
    const visLo = Math.max(boxLo, visible.top + GUTTER_PILL_H / 2);
    const visHi = Math.min(boxHi, visible.bottom - GUTTER_PILL_H / 2);
    // A band outside the hours box is no band at all.
    if (visLo <= visHi) {
      minCenter = visLo;
      maxCenter = visHi;
    }
  }

  // Edge pills only: the band-narrowed range that does the sticking.
  const clampCenter = (y: number): number => clamp(y, minCenter, maxCenter);

  const pills: GutterPillLayout[] = [];
  if (edges) {
    const startMs = edges.start.getTime();
    const endMs = edges.end.getTime();
    if (Number.isFinite(startMs) && Number.isFinite(endMs)) {
      const { startMin, endMin } = gutterEdgeMinutes(edges.start, edges.end);
      const placed = separateEdgeCenters(
        clampCenter((startMin / 60) * hourH),
        clampCenter((endMin / 60) * hourH),
        minCenter,
        maxCenter,
        GUTTER_PILL_H + GUTTER_PILL_GAP,
      );
      // Text comes from the instants, not from the clamped minutes: the
      // midnight end pill reads "12:00a" while sitting at 1440. The painted top
      // is a layout position only — the anchor time is the instant.
      pills.push({
        key: 'start',
        top: placed.startTop,
        text: formatGutterTime(edges.start),
        minutes: startMin,
      });
      pills.push({
        key: 'end',
        top: placed.endTop,
        text: formatGutterTime(edges.end),
        minutes: endMin,
      });
    }
  }

  if (now && Number.isFinite(now.getTime())) {
    // The hours box only, never the visible band: the now pill is decorative,
    // and pinning it to a viewport end would draw the now line at a time it is
    // not. Only a placed edge pill — which IS grabbable — can push it off.
    const top = clamp(nowLineY(now, hourH), boxLo, boxHi);
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
