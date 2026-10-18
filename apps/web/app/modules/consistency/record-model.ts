// Pure helpers behind "The Record" (reports slice 5): the range picker, the
// number formatters, the day-stack geometry, and the axis ticks. No React, no
// fetch — the section imports these and the node test runner exercises them
// directly.
//
// Every duration arrives as integer seconds and is divided by 3600 here, once.
// Nothing in this file decides whether a number is good: the delta is neutral
// text, and the only colour that means anything is a category's own colour.

import type { RecordDay, RecordHeatmapRow, RecordRangeKey } from '@/app/types';

/** Seconds in a civil day — the days chart's height scale. */
export const DAY_SECONDS = 86_400;

/** The four ranges, in picker order. */
export const RECORD_RANGES: { key: RecordRangeKey; label: string }[] = [
  { key: '7d', label: '7d' },
  { key: '30d', label: '30d' },
  { key: '90d', label: '90d' },
  { key: 'ytd', label: 'YTD' },
];

/** Monday-first labels (`weekday` is `0 = Monday … 6 = Sunday`). */
export const RECORD_WEEKDAY_LABELS = [
  'Mon',
  'Tue',
  'Wed',
  'Thu',
  'Fri',
  'Sat',
  'Sun',
];

const MONTHS = [
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

// ──────────────────────────────────────────
// Formatting
// ──────────────────────────────────────────

/** Seconds as hours, one decimal (`7200` → `2.0h`). */
export function hoursLabel(seconds: number): string {
  return `${(seconds / 3600).toFixed(1)}h`;
}

/** An hours figure as text, one decimal (`6.2` → `6.2h`). */
export function hoursText(hours: number): string {
  return `${hours.toFixed(1)}h`;
}

/** The sleep median's own precision: two decimals, an em dash when absent. */
export function medianHoursLabel(hours: number | null): string {
  return hours === null ? '—' : `${hours.toFixed(2)}h`;
}

/** The `sleep_nights.source` values in the words the reader sees. */
const SLEEP_SOURCE_LABELS: Record<string, string> = {
  paired: 'Paired',
  est_no_wake: 'No wake marker',
  est_no_asleep: 'No asleep marker',
  est_none: 'No markers',
};

/** A night's source as words (`est_no_wake` → `No wake marker`). An unknown
 *  value is shown as itself rather than hidden. */
export function sleepSourceLabel(source: string): string {
  return SLEEP_SOURCE_LABELS[source] ?? source;
}

/**
 * The mean untracked hours per day over the days that carry a figure.
 *
 * `untracked_seconds` is `null` while the day is still being written (today in
 * its own zone) and for a day nothing has been computed for yet; neither is a
 * tracked zero, so neither joins the mean. `null` when no day has a figure at
 * all — the line is then simply absent.
 */
export function meanUntrackedHours(days: RecordDay[]): number | null {
  let seconds = 0;
  let counted = 0;
  for (const day of days) {
    if (day.untracked_seconds === null) continue;
    seconds += day.untracked_seconds;
    counted += 1;
  }
  return counted === 0 ? null : seconds / counted / 3600;
}

/**
 * A neutral delta label against the previous window: `+1.2h`, `−0.4h`, `0h`
 * (U+2212 minus). No colour, no judgement — the reader decides.
 */
export function deltaLabel(seconds: number, previousSeconds: number): string {
  const delta = Math.round(((seconds - previousSeconds) / 3600) * 10) / 10;
  if (delta === 0) return '0h';
  const sign = delta > 0 ? '+' : '\u2212';
  return `${sign}${Math.abs(delta).toFixed(1)}h`;
}

/** A share as a whole percent (`0.423` → `42%`). */
export function shareLabel(share: number): string {
  return `${Math.round(share * 100)}%`;
}

/** A clock hour as `HH:00` (`9` → `09:00`). */
export function clockLabel(hour: number): string {
  return `${String(hour).padStart(2, '0')}:00`;
}

/** `2026-09-19` → `Sep 19` (the chart labels are the only place dates shrink). */
export function shortDate(date: string): string {
  const [year, month, day] = date.split('-');
  const name = MONTHS[Number(month) - 1];
  if (year === undefined || name === undefined || day === undefined) {
    return date;
  }
  return `${name} ${Number(day)}`;
}

// ──────────────────────────────────────────
// Axis ticks
// ──────────────────────────────────────────

/**
 * Which day columns get a date label: every one of a 7-day range, otherwise
 * the first, middle, and last.
 */
export function dayTickIndexes(count: number): number[] {
  if (count <= 0) return [];
  if (count <= 7) return Array.from({ length: count }, (_, index) => index);
  return [...new Set([0, Math.floor((count - 1) / 2), count - 1])];
}

// ──────────────────────────────────────────
// Days chart
// ──────────────────────────────────────────

export interface StackedSegment {
  categoryId: string;
  title: string;
  color: string;
  seconds: number;
  /** Pixels from the plot's top edge. */
  y: number;
  height: number;
}

/**
 * Positions one day's segments as a bottom-up stack inside a plot of
 * `plotHeight` pixels, scaled against a full civil day: a 4h day is a short
 * bar and a zero day is an empty slot. Segments keep the API's own order.
 */
export function stackDay(day: RecordDay, plotHeight: number): StackedSegment[] {
  const out: StackedSegment[] = [];
  let cursor = plotHeight;
  for (const segment of day.segments) {
    const height = Math.min(
      cursor,
      (segment.seconds / DAY_SECONDS) * plotHeight,
    );
    cursor -= height;
    out.push({
      categoryId: segment.category_id,
      title: segment.title,
      color: segment.color,
      seconds: segment.seconds,
      y: cursor,
      height,
    });
  }
  return out;
}

export interface LegendEntry {
  categoryId: string;
  title: string;
  color: string;
  seconds: number;
}

/**
 * The colours that appear in the range, biggest total first, capped. The cap
 * keeps the legend a legend: whatever falls off the end is still in the bars.
 */
export function legendEntries(days: RecordDay[], limit = 12): LegendEntry[] {
  const totals = new Map<string, LegendEntry>();
  for (const day of days) {
    for (const segment of day.segments) {
      const entry = totals.get(segment.category_id);
      if (entry) {
        entry.seconds += segment.seconds;
      } else {
        totals.set(segment.category_id, {
          categoryId: segment.category_id,
          title: segment.title,
          color: segment.color,
          seconds: segment.seconds,
        });
      }
    }
  }
  return [...totals.values()]
    .sort(
      (left, right) =>
        right.seconds - left.seconds || left.title.localeCompare(right.title),
    )
    .slice(0, limit);
}

// ──────────────────────────────────────────
// Heatmap
// ──────────────────────────────────────────

/** The busiest cell, floored at 1 so a zero grid never divides by zero. */
export function heatmapMax(heatmap: RecordHeatmapRow[]): number {
  let max = 0;
  for (const row of heatmap) {
    for (const seconds of row.seconds) {
      if (seconds > max) max = seconds;
    }
  }
  return Math.max(1, max);
}

/** The cell opacity for one mean, against the grid's busiest cell. */
export function heatmapOpacity(seconds: number, max: number): number {
  if (seconds <= 0) return 0;
  return Math.min(1, seconds / max);
}
