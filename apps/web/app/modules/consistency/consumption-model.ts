// Pure helpers behind "Consumption" (reports slice 3): the share and duration
// labels and the per-group summary line. No React, no fetch — the section
// imports these and the node test runner exercises them directly.
//
// The API sends integer seconds and event counts; every derived figure is
// computed here, once. Nothing in this file decides whether a number is good:
// the table is a reading of what was logged, not a score.

import { hoursLabel } from './record-model';

/** The `group` values in the words the reader sees. */
const CONSUMPTION_GROUP_LABELS: Record<string, string> = {
  series: 'Series',
  social: 'Social',
  messaging: 'Messaging',
  gaming: 'Gaming',
};

/**
 * A share of the window's total as a whole percent.
 *
 * Deliberately not `shareLabel`: a share that rounds to zero is `<1%`, not
 * `0%` — a platform with real seconds is never shown as absent.
 */
export function consumptionShareLabel(
  seconds: number,
  totalSeconds: number,
): string {
  if (totalSeconds <= 0 || seconds <= 0) return '0%';
  const pct = Math.round((seconds / totalSeconds) * 100);
  return pct === 0 ? '<1%' : `${pct}%`;
}

/**
 * A duration to the nearest minute: `14m`, then `3h36m` from an hour up.
 * 30–89 seconds rounds to `1m` and 1–29 seconds is `<1m`; nothing positive is
 * ever `0m`. The minute part is zero-padded, so a carry reads `1h00m`.
 */
export function durationLabel(seconds: number): string {
  if (seconds <= 0) return '0m';
  const minutes = Math.round(seconds / 60);
  if (minutes <= 0) return '<1m';
  if (minutes < 60) return `${minutes}m`;
  return `${Math.floor(minutes / 60)}h${String(minutes % 60).padStart(2, '0')}m`;
}

/** A group key as words (`series` → `Series`). An unknown key is shown as
 *  itself rather than hidden. */
export function consumptionGroupLabel(group: string): string {
  return CONSUMPTION_GROUP_LABELS[group] ?? group;
}

/**
 * One muted line of group totals, e.g.
 * `Series 21.6h (46%) · Social 20.5h (44%)`.
 *
 * The groups keep the server's own order (seconds descending) — the line
 * repeats that ordering rather than inventing a second one. An empty list is
 * an empty line, so the caller can omit it.
 */
export function consumptionGroupLine(
  groups: { group: string; seconds: number }[],
  totalSeconds: number,
): string {
  return groups
    .filter((entry) => entry.seconds > 0)
    .map(
      (entry) =>
        `${consumptionGroupLabel(entry.group)} ${hoursLabel(entry.seconds)} (${consumptionShareLabel(entry.seconds, totalSeconds)})`,
    )
    .join(' \u00b7 ');
}
