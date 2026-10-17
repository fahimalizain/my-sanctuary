// The calendar strip's *landing* window: the events range a freshly mounted
// strip opens with. Shared by the strip (its mount initializer) and background
// sync (which keeps that exact query warm off-route), so both must compose the
// week-layout helpers in the same order rather than each hand-rolling the
// formula. After a scroll or rebase the page's window diverges — that is the
// scrolled window, not this one.

import {
  STRIP_OVERSCAN,
  addDays,
  clampPeriodLength,
  fitPeriodStart,
  rangeIso,
  stripDayCount,
} from './week-layout';

/** First rendered day of a freshly mounted strip: fitted period start, then
 *  one overscan block before it. This is the mount initializer in
 *  useCalendarStrip — not the window after a scroll or rebase. */
export function initialStripWindowStart(now: Date, periodLength: number): Date {
  return addDays(
    fitPeriodStart(now, clampPeriodLength(periodLength)),
    -STRIP_OVERSCAN,
  );
}

/** Events query window for that mount. `timeMin`/`timeMax` are what
 *  `queryKeys.calendar.events` is built from, so this and the strip's first
 *  `rangeIso(windowStart, stripDayCount(periodLength))` must be identical. */
export function initialStripRange(
  now: Date,
  periodLength: number,
): { timeMin: string; timeMax: string } {
  const clamped = clampPeriodLength(periodLength);
  return rangeIso(
    initialStripWindowStart(now, clamped),
    stripDayCount(clamped),
  );
}

/** Milliseconds until the next local midnight. Exactly midnight → a full
 *  day (the following midnight), never 0. DST-safe: build the next local
 *  midnight with the Date constructor, do not subtract from 86_400_000. */
export function msUntilLocalMidnight(now: Date = new Date()): number {
  const next = new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1);
  const delta = next.getTime() - now.getTime();
  // Non-positive can happen where a DST rule skips or repeats local midnight
  // itself; a full day beats a 0ms timer that refires forever.
  return delta > 0 ? delta : 86_400_000;
}

/** Fired this long after local midnight so a timer that lands 1ms early
 *  does not recompute yesterday's window and then wait another day. */
export const LOCAL_MIDNIGHT_SKEW_MS = 1_000;
