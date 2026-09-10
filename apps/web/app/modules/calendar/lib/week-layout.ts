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

  // No overlap with [dayStart, dayEnd).
  if (endMs <= dayStartMs || startMs >= dayEndMs) {
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
