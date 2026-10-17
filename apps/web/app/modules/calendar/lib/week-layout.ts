// Pure date / geometry / overlap helpers for the week time-grid calendar.
// No React. Deterministic. Native Date only.

export const WEEK_DAYS = [
  'Mon',
  'Tue',
  'Wed',
  'Thu',
  'Fri',
  'Sat',
  'Sun',
] as const;

/** Notion-derived layout tokens (desktop week / gridView: 'default'). */
export const HOUR_H_BASE = 48;
export const HOUR_H_MIN = 16;
export const HOUR_H_MAX = 208;
export const CHIP_MARGIN_RIGHT = 13;
export const CHIP_MARGIN_BOTTOM = 3;
export const CHIP_MIN_H = 18;
/** Base z-index for timed chips; actual = CHIP_Z_BASE + layerIndex. */
export const CHIP_Z_BASE = 2;
/** Selected / manipulating chip sits above every layerIndex. */
export const CHIP_Z_SELECTED = 40;
export const COL_HEADER_H = 28;
export const TIME_GUTTER_W = 52; // Notion token is 26; we widen so "12PM" fits
export const MINUTES_PER_DAY = 1440;
/** Snap click-to-create / drag start times to this many minutes. */
export const SNAP_MINUTES = 15;
/** Snap resize active-edge times to this many minutes. */
export const RESIZE_SNAP_MINUTES = 1;
/** Default duration for a click-created event. */
export const DEFAULT_EVENT_DURATION_MIN = 30;

// Deterministic palette for event chips. Keyed off a hash of calendar_id
// (fallback event id) so the same calendar always paints the same color.
export const EVENT_COLORS = [
  '#2a5c8a', // work-blue
  '#c45a2c', // gym-terracotta
  '#7a4a6a', // family-plum
  '#3a7a5a', // relax-green
  '#8a6a2c', // amber
  '#4a5c8a', // indigo
];

const MONTH_SHORT = [
  'Jan',
  'Feb',
  'Mar',
  'Apr',
  'May',
  'Jun',
  'Jul',
  'Aug',
  'Sep',
  'Oct',
  'Nov',
  'Dec',
];

function clamp(n: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, n));
}

/** Monday local midnight for the week containing `date`. */
export function startOfWeek(date: Date): Date {
  const d = new Date(date);
  d.setHours(0, 0, 0, 0);
  // Treat Monday as the first day of the week (matches WEEK_DAYS).
  const jsDay = d.getDay(); // 0 = Sun ... 6 = Sat
  const offset = jsDay === 0 ? 6 : jsDay - 1;
  d.setDate(d.getDate() - offset);
  return d;
}

export function addDays(date: Date, n: number): Date {
  const d = new Date(date);
  d.setDate(d.getDate() + n);
  return d;
}

export function isSameDay(a: Date, b: Date): boolean {
  return (
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate()
  );
}

/** Saturday or Sunday in the browser's local timezone. */
export function isWeekend(date: Date): boolean {
  const day = date.getDay();
  return day === 0 || day === 6;
}

/** Local minutes since midnight (0–1439+; fractional seconds ignored). */
export function minutesSinceMidnight(date: Date): number {
  return date.getHours() * 60 + date.getMinutes() + date.getSeconds() / 60;
}

/**
 * Clamp an event interval to a single civil day column.
 * Intervals are half-open in spirit: overnight events are split by day.
 * Returns null when the event has no overlap with that day.
 */
export function clampMinutesToDay(
  start: Date,
  end: Date,
  day: Date,
): { startMin: number; endMin: number } | null {
  const dayStart = new Date(day);
  dayStart.setHours(0, 0, 0, 0);
  const dayEnd = addDays(dayStart, 1);

  const startMs = start.getTime();
  const endMs = end.getTime();
  const dayStartMs = dayStart.getTime();
  const dayEndMs = dayEnd.getTime();

  // No overlap with [dayStart, dayEnd). A zero-length event is a point: it
  // belongs to the day containing it (so 00:00–00:00 stays on that day).
  if (startMs === endMs) {
    if (startMs < dayStartMs || startMs >= dayEndMs) return null;
  } else if (endMs <= dayStartMs || startMs >= dayEndMs) {
    return null;
  }

  const clampedStart = Math.max(startMs, dayStartMs);
  const clampedEnd = Math.min(endMs, dayEndMs);

  let startMin = (clampedStart - dayStartMs) / 60_000;
  let endMin = (clampedEnd - dayStartMs) / 60_000;

  // Zero-duration (or inverted after clamp) → still paint a min chip at start.
  if (endMin <= startMin) {
    endMin = startMin;
  }

  startMin = clamp(startMin, 0, MINUTES_PER_DAY);
  endMin = clamp(endMin, 0, MINUTES_PER_DAY);

  return { startMin, endMin };
}

/** Seven local midnights Mon→Sun for the week starting at `weekStart`. */
export function weekDays(weekStart: Date): Date[] {
  const start = startOfWeek(weekStart);
  return Array.from({ length: 7 }, (_, i) => addDays(start, i));
}

/**
 * Query window: local midnight of week start → local midnight of week start + 7,
 * serialized as absolute UTC instants via toISOString().
 */
export function weekRangeIso(weekStart: Date): {
  timeMin: string;
  timeMax: string;
} {
  const start = startOfWeek(weekStart);
  return rangeIso(start, 7);
}

/**
 * Query window of `dayCount` local midnights starting at `start`:
 * [start midnight, start + dayCount midnight), as absolute UTC ISO strings.
 */
export function rangeIso(
  start: Date,
  dayCount: number,
): { timeMin: string; timeMax: string } {
  const origin = startOfDay(start);
  const end = addDays(origin, dayCount);
  return { timeMin: origin.toISOString(), timeMax: end.toISOString() };
}

// ── Infinite horizontal day strip ───────────────────────────────────────

/** Visible day columns that fill the viewport (Notion period length default). */
export const DAYS_PER_PERIOD = 7;
/** Minimum selectable period length (Day view). */
export const MIN_PERIOD_LENGTH = 1;
/**
 * Maximum selectable period length (Week view).
 * Numbered Notion items 1–7; Month/Other are separate surfaces.
 */
export const MAX_PERIOD_LENGTH = 7;
/** Extra days rendered on each side of the visible period. */
export const STRIP_OVERSCAN = 7;
/** Rebase when the first visible day is this close to a rendered edge. */
export const STRIP_REBASE_THRESHOLD = 3;

/**
 * Clamp a period length to `[MIN_PERIOD_LENGTH, MAX_PERIOD_LENGTH]`.
 * Non-finite values fall back to `DAYS_PER_PERIOD` (7).
 */
export function clampPeriodLength(n: number): number {
  if (!Number.isFinite(n)) return DAYS_PER_PERIOD;
  return clamp(Math.trunc(n), MIN_PERIOD_LENGTH, MAX_PERIOD_LENGTH);
}

/** localStorage key for the visible period length (1–7). */
export const PERIOD_LENGTH_STORAGE_KEY = 'sanctuary.calendar.periodLength';

/**
 * Parse localStorage value. null / '' / non-finite → null.
 * Finite → clampPeriodLength(n).
 */
export function parseStoredPeriodLength(raw: string | null): number | null {
  if (raw == null || raw === '') return null;
  const n = Number(raw);
  if (!Number.isFinite(n)) return null;
  return clampPeriodLength(n);
}

/** Notion button label: 1 → "Day", 7 → "Week", else `${n} days`. */
export function periodLabel(periodLength: number): string {
  const n = clampPeriodLength(periodLength);
  if (n === 1) return 'Day';
  if (n === DAYS_PER_PERIOD) return 'Week';
  return `${n} days`;
}

/**
 * Notion `fitPeriodStartDate`: if periodLength ≥ 5 (weekLength 7 − 2),
 * snap to Monday; else the civil day itself is the left edge.
 */
export function fitPeriodStart(date: Date, periodLength: number): Date {
  const n = clampPeriodLength(periodLength);
  if (n >= DAYS_PER_PERIOD - 2) {
    return startOfWeek(date);
  }
  return startOfDay(date);
}

/**
 * True once the grid column has a real layout width.
 * Do not use `colWidth(mainWidth) > 0` — `colWidth(0)` falls back to 16px, which
 * is enough to falsely complete the strip's initial overscan park.
 */
export function isStripWidthMeasured(mainWidth: number): boolean {
  return Number.isFinite(mainWidth) && mainWidth > 0;
}

/** Floor column width so exactly `periodLength` columns fit; leftover px go to the gutter. */
export function colWidth(
  availablePx: number,
  periodLength: number = DAYS_PER_PERIOD,
): number {
  const n = clampPeriodLength(periodLength);
  if (!Number.isFinite(availablePx) || availablePx <= 0) {
    return 16;
  }
  return Math.max(16, Math.floor((availablePx - TIME_GUTTER_W) / n));
}

/**
 * Gutter width including the fractional leftover after flooring colWidth.
 * `gutter + colW * periodLength` equals `availablePx`
 * (when availablePx ≥ TIME_GUTTER_W + 16*periodLength).
 */
export function gutterWithRemainder(
  availablePx: number,
  colW: number,
  periodLength: number = DAYS_PER_PERIOD,
): number {
  const n = clampPeriodLength(periodLength);
  if (!Number.isFinite(availablePx) || availablePx <= 0) {
    return TIME_GUTTER_W;
  }
  return TIME_GUTTER_W + (availablePx - TIME_GUTTER_W - colW * n);
}

/** Total days in the rendered strip window (period + overscan each side). */
export function stripDayCount(periodLength: number = DAYS_PER_PERIOD): number {
  return clampPeriodLength(periodLength) + STRIP_OVERSCAN * 2;
}

/**
 * Day index of the left edge of the viewport (first partially/fully visible
 * column), using floor. Not clamped.
 */
export function dayIndexFromScroll(scrollLeft: number, colW: number): number {
  if (!Number.isFinite(scrollLeft) || !Number.isFinite(colW) || colW <= 0) {
    return 0;
  }
  return Math.floor(scrollLeft / colW);
}

/**
 * Index of the first day of the visible period (snap via round).
 * Clamped so a full period always fits in `[0, dayCount - periodLength]`.
 */
export function visibleStartIndex(
  scrollLeft: number,
  colW: number,
  dayCount: number,
  periodLength: number = DAYS_PER_PERIOD,
): number {
  const n = clampPeriodLength(periodLength);
  if (!Number.isFinite(scrollLeft) || !Number.isFinite(colW) || colW <= 0) {
    return 0;
  }
  const maxStart = Math.max(0, dayCount - n);
  return clamp(Math.round(scrollLeft / colW), 0, maxStart);
}

/** scrollLeft that aligns column `index` with the left edge of the track. */
export function scrollLeftForIndex(index: number, colW: number): number {
  if (!Number.isFinite(index) || !Number.isFinite(colW) || colW <= 0) {
    return 0;
  }
  return index * colW;
}

/**
 * Whether the strip window should slide left (−1) or right (+1) so the
 * visible period stays away from the rendered edges. 0 = no rebase.
 */
export function shouldRebase(
  visibleStart: number,
  dayCount: number,
  periodLength: number = DAYS_PER_PERIOD,
): -1 | 0 | 1 {
  const n = clampPeriodLength(periodLength);
  if (visibleStart < STRIP_REBASE_THRESHOLD) return -1;
  if (visibleStart > dayCount - n - STRIP_REBASE_THRESHOLD) {
    return 1;
  }
  return 0;
}

/** Shift the strip window by one period in `direction`. */
export function shiftWindowStart(
  windowStart: Date,
  direction: -1 | 1,
  periodLength: number = DAYS_PER_PERIOD,
): Date {
  return addDays(windowStart, direction * clampPeriodLength(periodLength));
}

// ── Strip lock ownership ────────────────────────────────────────────────

/** Gesture owners that can hold the horizontal strip's scroll-snap off. */
export type StripLockReason = 'drag' | 'pinch' | 'swipe';

/**
 * Scroll-snap stays `none` while any owner holds the lock.
 * Membership is a Set, not a counter: holding twice and releasing once
 * releases the lock.
 */
export function stripSnapDisabled(held: ReadonlySet<StripLockReason>): boolean {
  return held.size > 0;
}

/**
 * Rebase is suppressed for every owner: each one writes `scrollLeft` itself
 * (drag auto-scroll, pinch zoom, touch swipe), and a rebase write would fight
 * those. A future owner that only holds snap off must be added explicitly.
 */
export function stripRebaseSuppressed(
  held: ReadonlySet<StripLockReason>,
): boolean {
  return held.has('drag') || held.has('pinch') || held.has('swipe');
}

// ── Touch scroll axis ───────────────────────────────────────────────────

/**
 * Sideways travel (px) a touch must make before the strip follows the finger.
 * Deliberately high: a horizontal day swipe is always intentional, and the
 * strip only moves once the gesture is unambiguously sideways. Tune this to
 * make day swipes easier (lower) or safer (higher).
 */
export const SCROLL_X_THRESHOLD_PX = 48;

/**
 * Vertical travel (px) that commits a gesture to `y`, locking it out of
 * horizontal for the rest of its life. Small, so a time scroll stops touching
 * the day as soon as it is clearly vertical.
 */
export const SCROLL_Y_SLOP_PX = 10;

/** Committed gesture axis; `pending` while the gesture is in the dead zone. */
export type ScrollAxis = 'pending' | 'x' | 'y';

/**
 * Axis of a touch gesture from its displacement:
 *
 *   x        |dx| ≥ `xThreshold` and clearly horizontal-dominant (> 2:1)
 *   y        |dy| > `ySlop` and not horizontal-dominant
 *   pending  the dead zone — the strip must not move sideways
 *
 * The scroller pans vertically natively (`touch-action: pan-y`), so a vertical
 * gesture needs no JS. A horizontal one is driven by hand: the browser would
 * otherwise move the strip from the first pixel, which makes a dead zone
 * impossible and lets a vertical swipe drift onto the next day.
 */
export function classifyScrollAxis(
  dx: number,
  dy: number,
  xThreshold: number = SCROLL_X_THRESHOLD_PX,
  ySlop: number = SCROLL_Y_SLOP_PX,
): ScrollAxis {
  const adx = Math.abs(dx);
  const ady = Math.abs(dy);
  // A swipe needs both distance and direction: the 2:1 ratio keeps a diagonal
  // time scroll from paging the day.
  if (adx >= xThreshold && adx > ady * 2) return 'x';
  // Vertical or diagonal: commit to y and never look sideways again.
  if (ady > ySlop && adx <= ady * 2) return 'y';
  return 'pending';
}

// ── Touch swipe release ─────────────────────────────────────────────────

/** How far ahead (ms) release velocity is projected to pick a column. */
export const SWIPE_PROJECTION_MS = 250;

/** Finger speed (px/ms) at or above which a release counts as a flick. */
export const SWIPE_FLICK_VELOCITY = 0.3;

/** Only samples this recent (ms) contribute to the release velocity. */
export const SWIPE_VELOCITY_WINDOW_MS = 100;

/** Finger sample: event timestamp (ms) and `clientX` (px). */
export interface SwipeSample {
  t: number;
  x: number;
}

/**
 * Finger velocity (px/ms, positive = moving right) at release.
 *
 * Only the last `windowMs` of movement counts: the release speed is what the
 * user felt at the end of the gesture, not the average over a slow drag that
 * happened to finish with a flick. A finger that stopped before lifting keeps
 * its newest sample at the same `x`, so it reads as 0 speed rather than as its
 * last fast burst.
 */
export function swipeVelocity(
  samples: readonly SwipeSample[],
  windowMs: number = SWIPE_VELOCITY_WINDOW_MS,
): number {
  if (samples.length < 2) return 0;
  const last = samples[samples.length - 1];
  let firstIdx = samples.length - 1;
  while (firstIdx > 0 && last.t - samples[firstIdx - 1].t <= windowMs) {
    firstIdx -= 1;
  }
  const first = samples[firstIdx];
  const dt = last.t - first.t;
  if (!(dt > 0)) return 0;
  const velocity = (last.x - first.x) / dt;
  return Number.isFinite(velocity) ? velocity : 0;
}

export interface SwipeRelease {
  /** Index of the column the finger landed on. */
  startIdx: number;
  /** `scrollLeft` when the finger lifted. */
  scrollLeft: number;
  /** Finger velocity in px/ms; positive = moving right (towards earlier days). */
  velocity: number;
  colW: number;
  periodLength: number;
  /** Highest index at which a full period still fits. */
  maxIdx: number;
}

/**
 * Column the strip should settle on after a horizontal swipe.
 *
 * A slow drag lands on the nearest column to where the finger left the strip.
 * A flick also projects the release velocity forward, but a projected column
 * equal to the starting one would make Day view (a full-width column) feel
 * dead — so a flick steps one column the way the finger moved.
 */
export function swipeTargetIndex({
  startIdx,
  scrollLeft,
  velocity,
  colW,
  periodLength,
  maxIdx,
}: SwipeRelease): number {
  const width = Number.isFinite(colW) && colW > 0 ? colW : 16;
  const left = Number.isFinite(scrollLeft) ? scrollLeft : 0;
  const speed = Number.isFinite(velocity) ? velocity : 0;
  const projected = Math.round((left - speed * SWIPE_PROJECTION_MS) / width);

  // Finger right drags the content right, i.e. towards earlier days.
  const settled =
    Math.abs(speed) >= SWIPE_FLICK_VELOCITY && projected === startIdx
      ? projected + (speed > 0 ? -1 : 1)
      : projected;

  // One gesture may cross at most one period: a fling must not skip a week.
  const span = Number.isFinite(periodLength)
    ? Math.max(1, Math.trunc(periodLength))
    : 1;
  const limit = Number.isFinite(maxIdx) ? Math.max(0, Math.trunc(maxIdx)) : 0;
  return clamp(clamp(settled, startIdx - span, startIdx + span), 0, limit);
}

// ── Wheel scroll axis ───────────────────────────────────────────────────

/**
 * Movement (px) a wheel burst must exceed before it commits to an axis.
 * Separate from the touch dead zone: a trackpad burst arrives as many small
 * events and needs a much smaller slop to classify in time.
 */
export const WHEEL_AXIS_SLOP = 8;

/**
 * Dominant axis of a wheel burst from its accumulated delta: `pending` under
 * the slop, `x` past 2:1, `y` otherwise. This is the pre-swipe classifier,
 * kept for the wheel path only — the touch path has its own dead-zone rules.
 */
export function classifyWheelAxis(
  dx: number,
  dy: number,
  slop: number = WHEEL_AXIS_SLOP,
): ScrollAxis {
  const adx = Math.abs(dx);
  const ady = Math.abs(dy);
  if (adx <= slop && ady <= slop) return 'pending';
  return adx > ady * 2 ? 'x' : 'y';
}

/**
 * Line box (px) assumed for `deltaMode === 1` (line) wheel deltas.
 * Mirrors `zoomFactorFromWheel` in calendar-zoom so both wheel paths read the
 * same device delta the same way.
 */
export const WHEEL_LINE_PX = 16;

/**
 * Quiet gap (ms) between wheel events that starts a new burst. A trackpad
 * emits a burst of events per gesture ~8–16ms apart; a pause longer than this
 * is a new gesture, so it decides its own axis.
 */
export const WHEEL_BURST_IDLE_MS = 120;

/**
 * Convert a raw wheel `delta` to px. Mode 0 is already px, mode 1 is lines,
 * mode 2 is pages and needs the viewport size the caller measured.
 * Non-finite deltas are 0, so a malformed event can never move the scroller.
 * A degenerate page falls back to the line box rather than to 0: the handler
 * has already preventDefault-ed the event by the time it scrolls.
 */
export function wheelDeltaPx(
  delta: number,
  deltaMode: number,
  pagePx: number,
): number {
  if (!Number.isFinite(delta)) return 0;
  if (deltaMode === 1) return delta * WHEEL_LINE_PX;
  if (deltaMode === 2) {
    const page = Number.isFinite(pagePx) && pagePx > 0 ? pagePx : WHEEL_LINE_PX;
    return delta * page;
  }
  return delta;
}

/** Latched wheel burst: committed axis, accumulated px, last event time. */
export interface WheelBurst {
  /** `pending` until the burst's accumulated delta crosses the slop. */
  axis: ScrollAxis;
  sumX: number;
  sumY: number;
  /** `timeStamp` of the last event folded in. */
  lastT: number;
}

/** No burst in flight: `lastT` −∞ makes the next event start one. */
export const EMPTY_WHEEL_BURST: WheelBurst = {
  axis: 'pending',
  sumX: 0,
  sumY: 0,
  lastT: Number.NEGATIVE_INFINITY,
};

/**
 * Fold one wheel event (px deltas + `timeStamp`) into the burst latch.
 *
 * A Mac trackpad puts a little horizontal delta on a nominally vertical
 * two-finger scroll. The axis is decided from the *accumulated* burst — one
 * event alone is noisy — and then latched: momentum keeps arriving after the
 * fingers lift, so a vertical burst that later turns horizontal-heavy must not
 * hand the strip back to native snap mid-gesture.
 *
 * A gap over `WHEEL_BURST_IDLE_MS` starts a fresh burst (sums and axis reset).
 * The comparison is NaN-safe: an unusable timestamp counts as a new burst,
 * never as a continuation of stale state.
 */
export function nextWheelBurst(
  prev: WheelBurst,
  event: { dx: number; dy: number; t: number },
): WheelBurst {
  const dx = Number.isFinite(event.dx) ? event.dx : 0;
  const dy = Number.isFinite(event.dy) ? event.dy : 0;
  const fresh = !(event.t - prev.lastT <= WHEEL_BURST_IDLE_MS);
  const sumX = (fresh ? 0 : prev.sumX) + dx;
  const sumY = (fresh ? 0 : prev.sumY) + dy;
  let axis = fresh ? 'pending' : prev.axis;
  if (axis === 'pending') axis = classifyWheelAxis(sumX, sumY);
  return { axis, sumX, sumY, lastT: event.t };
}

/**
 * Notion-style day-range title (inclusive first…last):
 *   same day   → "Tue, Sep 8, 2026"
 *   same month → "Sep 7–13, 2026"
 *   cross-month → "Sep 28 – Oct 4, 2026"
 *   cross-year  → "Dec 29, 2025 – Jan 4, 2026"
 */
export function formatDayRangeTitle(first: Date, last: Date): string {
  const sm = first.getMonth();
  const sy = first.getFullYear();
  const em = last.getMonth();
  const ey = last.getFullYear();
  const sd = first.getDate();
  const ed = last.getDate();

  if (sy === ey && sm === em && sd === ed) {
    const jsDay = first.getDay(); // 0 = Sun … 6 = Sat
    const monIndex = jsDay === 0 ? 6 : jsDay - 1;
    return `${WEEK_DAYS[monIndex]}, ${MONTH_SHORT[sm]} ${sd}, ${sy}`;
  }
  if (sy === ey && sm === em) {
    return `${MONTH_SHORT[sm]} ${sd}–${ed}, ${sy}`;
  }
  if (sy === ey) {
    return `${MONTH_SHORT[sm]} ${sd} – ${MONTH_SHORT[em]} ${ed}, ${sy}`;
  }
  return `${MONTH_SHORT[sm]} ${sd}, ${sy} – ${MONTH_SHORT[em]} ${ed}, ${ey}`;
}

/**
 * Notion-style week title for the Mon–Sun week containing `weekStart`.
 * Wrapper around formatDayRangeTitle.
 */
export function formatWeekTitle(weekStart: Date): string {
  const start = startOfWeek(weekStart);
  return formatDayRangeTitle(start, addDays(start, 6));
}

/** Hour gutter label: 1AM…11PM. Hour 0 returns empty (Notion has no midnight label). */
export function formatHourLabel(hour0to23: number): string {
  if (hour0to23 <= 0 || hour0to23 > 23) return '';
  const h12 = hour0to23 % 12 === 0 ? 12 : hour0to23 % 12;
  const suffix = hour0to23 < 12 ? 'AM' : 'PM';
  return `${h12}${suffix}`;
}

/**
 * Compact single-instant time like Notion: "5 AM", "5:15 AM".
 * Zero minutes omit the :00.
 */
export function formatEventTime(date: Date): string {
  const h = date.getHours();
  const m = date.getMinutes();
  const h12 = h % 12 === 0 ? 12 : h % 12;
  const suffix = h < 12 ? 'AM' : 'PM';
  if (m === 0) return `${h12} ${suffix}`;
  return `${h12}:${String(m).padStart(2, '0')} ${suffix}`;
}

/** Range label: "5 AM – 6:15 AM" (en-dash). */
export function formatEventTimeRange(start: Date, end: Date): string {
  return `${formatEventTime(start)} – ${formatEventTime(end)}`;
}

/**
 * Compact duration for the inspector when-block: "15min", "1h", "1h 30min".
 * Whole minutes from end − start, rounded to nearest, clamped ≥ 0.
 */
export function formatEventDuration(start: Date, end: Date): string {
  const totalMin = Math.max(
    0,
    Math.round((end.getTime() - start.getTime()) / 60_000),
  );
  const hours = Math.floor(totalMin / 60);
  const minutes = totalMin % 60;
  if (hours === 0) return `${minutes}min`;
  if (minutes === 0) return `${hours}h`;
  return `${hours}h ${minutes}min`;
}

/** Sun-first weekday labels for civil date lines (not WEEK_DAYS, which is Mon-first). */
const WEEKDAY_SUN_FIRST = [
  'Sun',
  'Mon',
  'Tue',
  'Wed',
  'Thu',
  'Fri',
  'Sat',
] as const;

function formatCivilDateLabel(date: Date): string {
  const weekday = WEEKDAY_SUN_FIRST[date.getDay()];
  return `${weekday} ${MONTH_SHORT[date.getMonth()]} ${date.getDate()}`;
}

/**
 * Civil date line for the inspector when-block, browser local zone.
 * Same day → "Sun Sep 13"; cross-day → "Sun Sep 13 → Mon Sep 14".
 */
export function formatEventDateLine(start: Date, end: Date): string {
  const startLabel = formatCivilDateLabel(start);
  if (
    start.getFullYear() === end.getFullYear() &&
    start.getMonth() === end.getMonth() &&
    start.getDate() === end.getDate()
  ) {
    return startLabel;
  }
  return `${startLabel} → ${formatCivilDateLabel(end)}`;
}

/** `type="time"` value: local 24h zero-padded "HH:mm". */
export function toTimeInputValue(date: Date): string {
  return `${String(date.getHours()).padStart(2, '0')}:${String(date.getMinutes()).padStart(2, '0')}`;
}

/** `type="date"` value: local civil "YYYY-MM-DD". */
export function toDateInputValue(date: Date): string {
  const y = date.getFullYear();
  const m = String(date.getMonth() + 1).padStart(2, '0');
  const d = String(date.getDate()).padStart(2, '0');
  return `${y}-${m}-${d}`;
}

function parseTimeInput(
  hhmm: string,
): { hours: number; minutes: number } | null {
  const match = /^(\d{1,2}):(\d{2})$/.exec(hhmm.trim());
  if (!match) return null;
  const hours = Number(match[1]);
  const minutes = Number(match[2]);
  if (
    !Number.isInteger(hours) ||
    !Number.isInteger(minutes) ||
    hours < 0 ||
    hours > 23 ||
    minutes < 0 ||
    minutes > 59
  ) {
    return null;
  }
  return { hours, minutes };
}

function parseDateInput(
  ymd: string,
): { year: number; monthIndex: number; day: number } | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(ymd.trim());
  if (!match) return null;
  const year = Number(match[1]);
  const month = Number(match[2]);
  const day = Number(match[3]);
  if (
    !Number.isInteger(year) ||
    !Number.isInteger(month) ||
    !Number.isInteger(day)
  ) {
    return null;
  }
  const monthIndex = month - 1;
  // Reject impossible civil dates (e.g. 2026-02-30) via round-trip.
  const probe = new Date(year, monthIndex, day);
  if (
    probe.getFullYear() !== year ||
    probe.getMonth() !== monthIndex ||
    probe.getDate() !== day
  ) {
    return null;
  }
  return { year, monthIndex, day };
}

/**
 * Apply a start-time edit: set hours:minutes on start's civil date, keep duration.
 * Invalid `hhmm` → null.
 */
export function applyStartTime(
  start: Date,
  end: Date,
  hhmm: string,
): { start: Date; end: Date } | null {
  const parsed = parseTimeInput(hhmm);
  if (!parsed) return null;
  const newStart = new Date(start.getTime());
  newStart.setHours(parsed.hours, parsed.minutes, 0, 0);
  const durationMs = end.getTime() - start.getTime();
  const newEnd = new Date(newStart.getTime() + durationMs);
  return { start: newStart, end: newEnd };
}

/**
 * Apply an end-time edit on end's civil date (overnight-safe). Start unchanged.
 * newEnd === start is allowed (0-min); newEnd < start clamps to start.
 * Invalid `hhmm` → null.
 */
export function applyEndTime(
  start: Date,
  end: Date,
  hhmm: string,
): { start: Date; end: Date } | null {
  const parsed = parseTimeInput(hhmm);
  if (!parsed) return null;
  const newEnd = new Date(end.getTime());
  newEnd.setHours(parsed.hours, parsed.minutes, 0, 0);
  if (newEnd.getTime() < start.getTime()) {
    return { start: new Date(start.getTime()), end: new Date(start.getTime()) };
  }
  return { start: new Date(start.getTime()), end: newEnd };
}

/**
 * Shift both instants by the whole-day delta from start's civil date to `ymd`.
 * Clock times and duration are preserved. Invalid → null.
 */
export function applyStartDate(
  start: Date,
  end: Date,
  ymd: string,
): { start: Date; end: Date } | null {
  const parsed = parseDateInput(ymd);
  if (!parsed) return null;
  const oldMidnight = new Date(
    start.getFullYear(),
    start.getMonth(),
    start.getDate(),
  ).getTime();
  const newMidnight = new Date(
    parsed.year,
    parsed.monthIndex,
    parsed.day,
  ).getTime();
  const deltaMs = newMidnight - oldMidnight;
  return {
    start: new Date(start.getTime() + deltaMs),
    end: new Date(end.getTime() + deltaMs),
  };
}

/**
 * Hour row height for the current viewport hours area.
 * Short viewports stay at ≥48 and scroll; tall ones stretch up to 208.
 */
export function hourHeight(availablePx: number): number {
  if (!Number.isFinite(availablePx) || availablePx <= 0) {
    return HOUR_H_BASE;
  }
  // Prefer filling 24 hours when there's room; never go below base unless
  // the clamp floor forces it (HOUR_H_MIN) — base is the practical floor.
  const stretched = availablePx / 24;
  return clamp(Math.max(HOUR_H_BASE, stretched), HOUR_H_MIN, HOUR_H_MAX);
}

/**
 * CSS `background-image` for 24 hour hairlines on a day column.
 * One layer replaces 24 absolutely-positioned border divs.
 */
export function hourGridBackground(hourH: number): string {
  const h = Number.isFinite(hourH) && hourH > 0 ? hourH : HOUR_H_BASE;
  const line = h - 1;
  return `repeating-linear-gradient(to bottom, transparent 0, transparent ${line}px, hsl(var(--border) / 0.5) ${line}px, hsl(var(--border) / 0.5) ${h}px)`;
}

export function eventTopPx(startMin: number, hourH: number): number {
  return (startMin / 60) * hourH;
}

/**
 * Chip height: duration in px minus CHIP_MARGIN_BOTTOM, floored at CHIP_MIN_H.
 * Zero / tiny durations still get a visible min chip.
 */
export function eventHeightPx(
  startMin: number,
  endMin: number,
  hourH: number,
): number {
  const raw = ((endMin - startMin) / 60) * hourH - CHIP_MARGIN_BOTTOM;
  return Math.max(raw, CHIP_MIN_H);
}

// Timed packing lives in notion-pack.ts (Notion cascade). Re-export for
// existing call sites (calendar-model, tests).
export {
  packDayEvents,
  intervalsOverlap,
  PACK_STEAL_MINUTES,
  PACK_PEEK_GUTTER,
  type PackInput,
  type PackResult,
} from './notion-pack';

/** Y position of the now line within the hours area. */
export function nowLineY(now: Date, hourH: number): number {
  return minutesSinceMidnight(now) * (hourH / 60);
}

function hashString(s: string): number {
  let hash = 0;
  for (let i = 0; i < s.length; i++) {
    hash = (hash << 5) - hash + s.charCodeAt(i);
    hash |= 0; // force int32
  }
  return hash;
}

/** Palette color for a calendar (or any stable id). */
export function colorForCalendar(calendarId: string): string {
  const hash = hashString(calendarId || 'default');
  return EVENT_COLORS[Math.abs(hash) % EVENT_COLORS.length];
}

/**
 * Convert `#rgb` / `#rrggbb` to `rgba(r, g, b, alpha)`.
 * Shared by timed chips, all-day chips, and any muted calendar fill.
 */
export function hexToRgba(hex: string, alpha: number): string {
  const h = hex.replace('#', '');
  const full =
    h.length === 3
      ? h
          .split('')
          .map((c) => c + c)
          .join('')
      : h;
  const n = parseInt(full, 16);
  const r = (n >> 16) & 255;
  const g = (n >> 8) & 255;
  const b = n & 255;
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

/** Parse `#rgb` / `#rrggbb` into 0..255 channels. Invalid → null. */
function parseHexRgb(hex: string): { r: number; g: number; b: number } | null {
  const h = hex.replace('#', '');
  const full =
    h.length === 3
      ? h
          .split('')
          .map((c) => c + c)
          .join('')
      : h;
  if (!/^[0-9a-fA-F]{6}$/.test(full)) return null;
  const n = parseInt(full, 16);
  return {
    r: (n >> 16) & 255,
    g: (n >> 8) & 255,
    b: n & 255,
  };
}

/** sRGB channel 0..1 → linear contribution for relative luminance. */
function srgbToLinear(c: number): number {
  return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
}

/** Relative luminance 0..1 (sRGB). Invalid hex → 0. */
export function hexLuminance(hex: string): number {
  const rgb = parseHexRgb(hex);
  if (!rgb) return 0;
  const R = srgbToLinear(rgb.r / 255);
  const G = srgbToLinear(rgb.g / 255);
  const B = srgbToLinear(rgb.b / 255);
  return 0.2126 * R + 0.7152 * G + 0.0722 * B;
}

/** Ink that contrasts with a solid fill: '#fafafa' if luminance < 0.55, else '#1a1a1a'. */
export function contrastingInk(hex: string): string {
  return hexLuminance(hex) < 0.55 ? '#fafafa' : '#1a1a1a';
}

// Notion chip chrome tokens used by isCompactChip.
const CHIP_PAD_TOP = 1; // py-px
const CHIP_PAD_BOTTOM = 1;
const CHIP_TIME_LINE_H = 11; // text-[9px] leading-[11px]
const CHIP_TIME_MARGIN_TOP = 2;

/**
 * Notion compact-vs-stacked rule for timed event chips.
 * When the remaining vertical space for the title is tighter than one time
 * line (+ its top margin), collapse title+time onto a single row.
 *
 *   inner = height - padTop - padBottom - timeLineH - marginBottom
 *   compact = inner < timeLineH + timeMarginTop
 */
export function isCompactChip(heightPx: number): boolean {
  const inner =
    heightPx -
    CHIP_PAD_TOP -
    CHIP_PAD_BOTTOM -
    CHIP_TIME_LINE_H -
    CHIP_MARGIN_BOTTOM;
  return inner < CHIP_TIME_LINE_H + CHIP_TIME_MARGIN_TOP;
}

// ── All-day band (Notion Cron Xme tokens) ───────────────────────────────

export const ALLDAY_CHIP = 19;
export const ALLDAY_GAP = 2;
export const ALLDAY_PAD = 3;
export const ALLDAY_MIN = 25;
export const ALLDAY_MAX = 137.5;

/**
 * Monday-aligned first cell of the 6×7 mini-month grid for the month
 * containing `viewDate`.
 */
export function monthGridStart(viewDate: Date): Date {
  const firstOfMonth = new Date(viewDate.getFullYear(), viewDate.getMonth(), 1);
  return startOfWeek(firstOfMonth);
}

/** 42 local midnights covering the mini-month grid for `viewDate`'s month. */
export function monthGridDays(viewDate: Date): Date[] {
  const start = monthGridStart(viewDate);
  return Array.from({ length: 42 }, (_, i) => addDays(start, i));
}

/**
 * Multi-day (all-day band) events: occupies ≥2 distinct local civil dates
 * AND duration ≥ 12 hours. Short overnights (e.g. 11pm–1am) stay on the
 * time grid.
 */
export function isMultiDay(start: Date, end: Date): boolean {
  if (isSameDay(start, end)) return false;
  const durationMs = end.getTime() - start.getTime();
  return durationMs >= 12 * 60 * 60 * 1000;
}

export interface AllDayPackInput {
  id: string;
  /** Inclusive day index in the visible week (0 = Mon … 6 = Sun). */
  startDay: number;
  /** Inclusive day index in the visible week (0 = Mon … 6 = Sun). */
  endDay: number;
}

export interface AllDayPackResult {
  id: string;
  lane: number;
}

function dayRangesOverlap(
  aStart: number,
  aEnd: number,
  bStart: number,
  bEnd: number,
): boolean {
  // Inclusive ranges on the day axis.
  return aStart <= bEnd && bStart <= aEnd;
}

/**
 * First-fit lane packing for all-day chips.
 * `topIndex` = smallest non-negative integer not used by an overlapping chip.
 * Caller must clamp each event to the visible week before calling.
 */
export function packAllDayLanes(items: AllDayPackInput[]): AllDayPackResult[] {
  if (items.length === 0) return [];

  const sorted = [...items].sort((a, b) => {
    if (a.startDay !== b.startDay) return a.startDay - b.startDay;
    const spanA = a.endDay - a.startDay;
    const spanB = b.endDay - b.startDay;
    if (spanA !== spanB) return spanB - spanA; // longer first
    return a.id.localeCompare(b.id);
  });

  const results: AllDayPackResult[] = [];
  const laneRanges: { startDay: number; endDay: number }[][] = [];

  for (const item of sorted) {
    let lane = 0;
    for (;;) {
      const occupied = laneRanges[lane] ?? [];
      const free = occupied.every(
        (r) =>
          !dayRangesOverlap(item.startDay, item.endDay, r.startDay, r.endDay),
      );
      if (free) {
        if (!laneRanges[lane]) laneRanges[lane] = [];
        laneRanges[lane].push({
          startDay: item.startDay,
          endDay: item.endDay,
        });
        results.push({ id: item.id, lane });
        break;
      }
      lane += 1;
    }
  }

  return results;
}

/**
 * Height of the all-day section.
 * `null` / no items → ALLDAY_MIN.
 * Else clamp(max(MIN, PAD + (CHIP+GAP)*(maxLane+1) + 1), MIN, MAX).
 */
export function allDaySectionHeight(maxLaneIndex: number | null): number {
  if (maxLaneIndex === null || maxLaneIndex < 0) return ALLDAY_MIN;
  const raw = ALLDAY_PAD + (ALLDAY_CHIP + ALLDAY_GAP) * (maxLaneIndex + 1) + 1;
  return clamp(Math.max(ALLDAY_MIN, raw), ALLDAY_MIN, ALLDAY_MAX);
}

/**
 * Inclusive last civil date occupied by [start, end).
 * When `end` is exactly local midnight (exclusive end-of-day boundary),
 * the previous calendar day is the last occupied day.
 */
export function lastOccupiedCivilDate(start: Date, end: Date): Date {
  const endCivil = new Date(end.getFullYear(), end.getMonth(), end.getDate());
  if (end.getTime() === endCivil.getTime() && end.getTime() > start.getTime()) {
    return addDays(endCivil, -1);
  }
  return endCivil;
}

/** Local midnight of the civil date containing `date`. */
export function startOfDay(date: Date): Date {
  return new Date(date.getFullYear(), date.getMonth(), date.getDate());
}

/**
 * Parse the leading `YYYY-MM-DD` of a stored all-day ISO (typically
 * `YYYY-MM-DDT00:00:00Z`) as **local civil midnight**. Do not use
 * `new Date(iso)` — UTC midnight shifts to the previous evening in US zones.
 */
export function civilDateFromAllDayIso(iso: string): Date | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})/.exec(iso.trim());
  if (!match) return null;
  const year = Number(match[1]);
  const month = Number(match[2]);
  const day = Number(match[3]);
  if (
    !Number.isInteger(year) ||
    !Number.isInteger(month) ||
    !Number.isInteger(day)
  ) {
    return null;
  }
  const monthIndex = month - 1;
  const probe = new Date(year, monthIndex, day);
  if (
    probe.getFullYear() !== year ||
    probe.getMonth() !== monthIndex ||
    probe.getDate() !== day
  ) {
    return null;
  }
  return probe;
}

/** Replica / overlay shape for an all-day civil date: `YYYY-MM-DDT00:00:00Z`. */
export function civilYmdToUtcMidnightIso(ymd: string): string {
  return `${ymd.trim()}T00:00:00Z`;
}

/**
 * Timed → all-day conversion for the inspector toggle.
 * Start = local civil date of start; end = exclusive day after last occupied.
 * Overlay ISO values match the replica (`…T00:00:00Z`).
 */
export function timedRangeToAllDay(
  start: Date,
  end: Date,
): {
  startDate: string;
  endDate: string;
  startIso: string;
  endIso: string;
} {
  const startDate = toDateInputValue(start);
  const lastOccupied = lastOccupiedCivilDate(start, end);
  const endExclusive = addDays(lastOccupied, 1);
  const endDate = toDateInputValue(endExclusive);
  return {
    startDate,
    endDate,
    startIso: civilYmdToUtcMidnightIso(startDate),
    endIso: civilYmdToUtcMidnightIso(endDate),
  };
}

/**
 * All-day → timed conversion for the inspector toggle.
 * Start civil date at 09:00 local → 10:00 local (1 hour).
 * Uses the ISO date prefix (not `new Date(iso)` local conversion).
 */
export function allDayRangeToTimed(
  startIso: string,
  _endIso: string,
): { start: Date; end: Date } | null {
  const civil = civilDateFromAllDayIso(startIso);
  if (!civil) return null;
  const start = new Date(
    civil.getFullYear(),
    civil.getMonth(),
    civil.getDate(),
    9,
    0,
    0,
    0,
  );
  const end = new Date(
    civil.getFullYear(),
    civil.getMonth(),
    civil.getDate(),
    10,
    0,
    0,
    0,
  );
  return { start, end };
}

/**
 * Shift an all-day exclusive range so start becomes `ymd`, preserving span
 * in whole days. Inputs/outputs are replica ISO (`…T00:00:00Z`) or bare dates.
 */
export function applyAllDayStartDate(
  startIso: string,
  endIso: string,
  ymd: string,
): { startIso: string; endIso: string } | null {
  const startCivil = civilDateFromAllDayIso(startIso);
  const endCivil = civilDateFromAllDayIso(endIso);
  const parsed = parseDateInput(ymd);
  if (!startCivil || !endCivil || !parsed) return null;
  const newStart = new Date(parsed.year, parsed.monthIndex, parsed.day);
  const spanMs = endCivil.getTime() - startCivil.getTime();
  // Preserve exclusive span; if inverted/zero, default to one day exclusive.
  const safeSpan = spanMs > 0 ? spanMs : 24 * 60 * 60 * 1000;
  const newEnd = new Date(newStart.getTime() + safeSpan);
  const startDate = toDateInputValue(newStart);
  const endDate = toDateInputValue(newEnd);
  return {
    startIso: civilYmdToUtcMidnightIso(startDate),
    endIso: civilYmdToUtcMidnightIso(endDate),
  };
}

// ── Click-to-create geometry ────────────────────────────────────────────

/**
 * Snap minutes-since-midnight to the nearest `step` boundary
 * (default `SNAP_MINUTES`). Clamped to `[0, 1440]`; 1440 stays 1440.
 */
export function snapMinutes(min: number, step = SNAP_MINUTES): number {
  if (!Number.isFinite(min) || min <= 0) return 0;
  if (min >= MINUTES_PER_DAY) return MINUTES_PER_DAY;
  const grid = Number.isFinite(step) && step > 0 ? step : SNAP_MINUTES;
  return Math.round(min / grid) * grid;
}

/** Convert a Y offset (px) within the hours area to minutes since midnight. */
export function minutesFromY(yPx: number, hourH: number): number {
  if (!Number.isFinite(yPx) || !Number.isFinite(hourH) || hourH <= 0) {
    return 0;
  }
  return (yPx / hourH) * 60;
}

/**
 * Local Date for a civil `day` at `minutes` since midnight.
 * Minutes may be fractional; seconds/ms are zeroed via the Date constructor.
 */
export function dateOnDay(day: Date, minutes: number): Date {
  const origin = startOfDay(day);
  const total = Number.isFinite(minutes) ? minutes : 0;
  const h = Math.floor(total / 60);
  const m = Math.floor(total - h * 60);
  return new Date(
    origin.getFullYear(),
    origin.getMonth(),
    origin.getDate(),
    h,
    m,
    0,
    0,
  );
}

/**
 * Prefer the primary calendar when the user can write to it; otherwise the
 * first owner/writer. Readers and freeBusyReader are never returned.
 */
export function defaultWritableCalendar<
  T extends { is_primary: boolean; access_role: string },
>(calendars: T[]): T | undefined {
  const writable = calendars.filter(
    (c) => c.access_role === 'owner' || c.access_role === 'writer',
  );
  if (writable.length === 0) return undefined;
  return writable.find((c) => c.is_primary) ?? writable[0];
}
