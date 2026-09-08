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
export const COL_HEADER_H = 28;
export const TIME_GUTTER_W = 52; // Notion token is 26; we widen so "12PM" fits
export const MINUTES_PER_DAY = 1440;

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
  const end = addDays(start, 7);
  return { timeMin: start.toISOString(), timeMax: end.toISOString() };
}

/**
 * Notion-style week title:
 *   same month → "Sep 7–13, 2026"
 *   cross-month → "Sep 28 – Oct 4, 2026"
 *   cross-year  → "Dec 29, 2025 – Jan 4, 2026"
 */
export function formatWeekTitle(weekStart: Date): string {
  const start = startOfWeek(weekStart);
  const end = addDays(start, 6); // inclusive last day of the week

  const sm = start.getMonth();
  const sy = start.getFullYear();
  const em = end.getMonth();
  const ey = end.getFullYear();
  const sd = start.getDate();
  const ed = end.getDate();

  if (sy === ey && sm === em) {
    return `${MONTH_SHORT[sm]} ${sd}–${ed}, ${sy}`;
  }
  if (sy === ey) {
    return `${MONTH_SHORT[sm]} ${sd} – ${MONTH_SHORT[em]} ${ed}, ${sy}`;
  }
  return `${MONTH_SHORT[sm]} ${sd}, ${sy} – ${MONTH_SHORT[em]} ${ed}, ${ey}`;
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

export interface PackInput {
  id: string;
  startMin: number;
  endMin: number;
}

export interface PackResult {
  id: string;
  col: number;
  cols: number;
  span: number;
}

function intervalsOverlap(
  aStart: number,
  aEnd: number,
  bStart: number,
  bEnd: number,
): boolean {
  // Treat zero-duration as a point that still collides if nested in another.
  const aE = aEnd > aStart ? aEnd : aStart + 0.001;
  const bE = bEnd > bStart ? bEnd : bStart + 0.001;
  return aStart < bE && bStart < aE;
}

/**
 * Google/Cron-style column packing for one day (core only — no half-column
 * steal rules).
 *
 * 1. Sort: earlier start first; same start → longer duration first.
 * 2. Cluster: greedy — joins the first cluster that already contains an
 *    overlapping event; else new cluster.
 * 3. Per cluster: leftmost column with no overlap; optional right-span into
 *    empty later columns.
 */
export function packDayEvents(items: PackInput[]): PackResult[] {
  if (items.length === 0) return [];

  const sorted = [...items].sort((a, b) => {
    if (a.startMin !== b.startMin) return a.startMin - b.startMin;
    const durA = a.endMin - a.startMin;
    const durB = b.endMin - b.startMin;
    if (durA !== durB) return durB - durA; // longer first
    return a.id.localeCompare(b.id);
  });

  // Greedy clusters (connected components by overlap, order-preserving).
  const clusters: PackInput[][] = [];
  for (const item of sorted) {
    let placed = false;
    for (const cluster of clusters) {
      if (
        cluster.some((other) =>
          intervalsOverlap(
            item.startMin,
            item.endMin,
            other.startMin,
            other.endMin,
          ),
        )
      ) {
        cluster.push(item);
        placed = true;
        break;
      }
    }
    if (!placed) clusters.push([item]);
  }

  const results: PackResult[] = [];

  for (const cluster of clusters) {
    // Assign leftmost free column.
    const assignments: { item: PackInput; col: number }[] = [];
    // Per column: list of placed intervals.
    const colIntervals: { startMin: number; endMin: number }[][] = [];

    for (const item of cluster) {
      let col = 0;
      for (;;) {
        const intervals = colIntervals[col] ?? [];
        const free = intervals.every(
          (iv) =>
            !intervalsOverlap(
              item.startMin,
              item.endMin,
              iv.startMin,
              iv.endMin,
            ),
        );
        if (free) {
          if (!colIntervals[col]) colIntervals[col] = [];
          colIntervals[col].push({
            startMin: item.startMin,
            endMin: item.endMin,
          });
          assignments.push({ item, col });
          break;
        }
        col += 1;
      }
    }

    const nCols = colIntervals.length;

    for (const { item, col } of assignments) {
      // Right-span: grow while later columns have no overlap in [start, end).
      let span = 1;
      for (let c = col + 1; c < nCols; c++) {
        const intervals = colIntervals[c] ?? [];
        const blocked = intervals.some((iv) =>
          intervalsOverlap(
            item.startMin,
            item.endMin,
            iv.startMin,
            iv.endMin,
          ),
        );
        if (blocked) break;
        // Also blocked if another event in this cluster was assigned to `c`
        // and overlaps — colIntervals already holds those.
        span += 1;
      }

      // Don't span into a column that has a different event assigned that
      // we "skipped" — the check above is sufficient because every placed
      // event is in colIntervals.
      // Cap span so we don't cover a column that has a non-overlapping event
      // that sits beside us visually for the full width — actually spanning
      // empty space is the point. Done.

      results.push({ id: item.id, col, cols: nCols, span });
    }
  }

  return results;
}

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
  const firstOfMonth = new Date(
    viewDate.getFullYear(),
    viewDate.getMonth(),
    1,
  );
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
          !dayRangesOverlap(
            item.startDay,
            item.endDay,
            r.startDay,
            r.endDay,
          ),
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
  const raw =
    ALLDAY_PAD +
    (ALLDAY_CHIP + ALLDAY_GAP) * (maxLaneIndex + 1) +
    1;
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
