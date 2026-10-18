import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { RecordDay } from '../../types';
import {
  clockLabel,
  dayTickIndexes,
  deltaLabel,
  heatmapMax,
  heatmapOpacity,
  hoursLabel,
  hoursText,
  legendEntries,
  meanUntrackedHours,
  medianHoursLabel,
  RECORD_RANGES,
  shareLabel,
  shortDate,
  sleepSourceLabel,
  stackDay,
} from './record-model';

// ── number labels ────────────────────────────────────────────────────────

test('hoursLabel: seconds divided by 3600, one decimal', () => {
  assert.equal(hoursLabel(0), '0.0h');
  assert.equal(hoursLabel(7_200), '2.0h');
  assert.equal(hoursLabel(23_400), '6.5h');
  assert.equal(hoursLabel(1_800), '0.5h');
});

test('hoursText: an hours figure stays one decimal', () => {
  assert.equal(hoursText(6.2), '6.2h');
  assert.equal(hoursText(0), '0.0h');
});

test('medianHoursLabel: two decimals, em dash when there is no median', () => {
  assert.equal(medianHoursLabel(6.5), '6.50h');
  assert.equal(medianHoursLabel(null), '—');
});

test('sleepSourceLabel: the sources in words, an unknown one as itself', () => {
  assert.equal(sleepSourceLabel('paired'), 'Paired');
  assert.equal(sleepSourceLabel('est_no_wake'), 'No wake marker');
  assert.equal(sleepSourceLabel('est_no_asleep'), 'No asleep marker');
  assert.equal(sleepSourceLabel('est_none'), 'No markers');
  assert.equal(sleepSourceLabel('est_from_mars'), 'est_from_mars');
});

test('shareLabel: a whole percent', () => {
  assert.equal(shareLabel(0.423), '42%');
  assert.equal(shareLabel(1), '100%');
  assert.equal(shareLabel(0), '0%');
});

test('deltaLabel: a neutral signed delta, never a colour', () => {
  assert.equal(deltaLabel(7_200, 3_600), '+1.0h');
  assert.equal(deltaLabel(3_600, 7_200), '\u22121.0h');
  assert.equal(deltaLabel(7_200, 7_200), '0h');
  assert.equal(deltaLabel(3_600, 0), '+1.0h');
  assert.equal(deltaLabel(0, 3_600), '\u22121.0h');
  // Rounds to a tenth before deciding the sign.
  assert.equal(deltaLabel(3_600 + 60, 3_600), '0h');
  assert.equal(deltaLabel(3_600 - 60, 3_600), '0h');
  // The minus is U+2212, not a hyphen.
  assert.ok(!deltaLabel(0, 3_600).includes('-'));
});

test('clockLabel and shortDate', () => {
  assert.equal(clockLabel(9), '09:00');
  assert.equal(clockLabel(0), '00:00');
  assert.equal(clockLabel(23), '23:00');
  assert.equal(shortDate('2026-09-19'), 'Sep 19');
  assert.equal(shortDate('2026-01-01'), 'Jan 1');
  assert.equal(shortDate('nonsense'), 'nonsense');
});

// ── ticks ────────────────────────────────────────────────────────────────

test('dayTickIndexes: every column of a week, first/mid/last otherwise', () => {
  assert.deepEqual(dayTickIndexes(0), []);
  assert.deepEqual(dayTickIndexes(7), [0, 1, 2, 3, 4, 5, 6]);
  assert.deepEqual(dayTickIndexes(30), [0, 14, 29]);
  assert.deepEqual(dayTickIndexes(90), [0, 44, 89]);
  assert.deepEqual(dayTickIndexes(1), [0]);
});

// ── days chart ───────────────────────────────────────────────────────────

function day(date: string, segments: RecordDay['segments']): RecordDay {
  return { date, aggregated: true, segments, untracked_seconds: null };
}

test('stackDay: a bottom-up stack scaled against a full day', () => {
  const stacked = stackDay(
    day('2026-10-17', [
      { category_id: 'a', title: 'A', color: '#111111', seconds: 43_200 },
      { category_id: 'b', title: 'B', color: '#222222', seconds: 21_600 },
    ]),
    100,
  );
  // 12h is half the day, 6h a quarter: 50px and 25px, stacked from the bottom.
  assert.equal(stacked.length, 2);
  assert.deepEqual(stacked[0], {
    categoryId: 'a',
    title: 'A',
    color: '#111111',
    seconds: 43_200,
    y: 50,
    height: 50,
  });
  assert.deepEqual(stacked[1], {
    categoryId: 'b',
    title: 'B',
    color: '#222222',
    seconds: 21_600,
    y: 25,
    height: 25,
  });
});

test('stackDay: a zero day is an empty slot and never overflows the plot', () => {
  assert.deepEqual(stackDay(day('2026-10-17', []), 100), []);
  // More than 86400 seconds (overlapping shares) still fits the plot.
  const stacked = stackDay(
    day('2026-10-17', [
      { category_id: 'a', title: 'A', color: '#111111', seconds: 172_800 },
    ]),
    100,
  );
  assert.equal(stacked[0].height, 100);
  assert.equal(stacked[0].y, 0);
});

// ── data quality ─────────────────────────────────────────────────────────

test('meanUntrackedHours: skips nulls and has no answer without a figure', () => {
  const days = [
    // A tracked day, an unbuilt day, and today: only the first has a figure.
    { ...day('2026-10-16', []), untracked_seconds: 3_600 },
    { ...day('2026-10-17', []), aggregated: false },
    day('2026-10-18', []),
  ];
  assert.equal(meanUntrackedHours(days), 1);
  // Every value null (today, unbuilt days) → the line is omitted.
  assert.equal(meanUntrackedHours([day('2026-10-18', [])]), null);
  assert.equal(meanUntrackedHours([]), null);
});

test('meanUntrackedHours: the mean of the days that do carry a figure', () => {
  const days = [
    { ...day('2026-10-16', []), untracked_seconds: 0 },
    { ...day('2026-10-17', []), untracked_seconds: 7_200 },
    day('2026-10-18', []),
  ];
  assert.equal(meanUntrackedHours(days), 1, '0h and 2h average to 1h');
});

test('legendEntries: biggest total first, capped', () => {
  const days = [
    day('2026-10-17', [
      { category_id: 'a', title: 'A', color: '#111111', seconds: 3_600 },
      { category_id: 'b', title: 'B', color: '#222222', seconds: 7_200 },
    ]),
    day('2026-10-18', [
      { category_id: 'a', title: 'A', color: '#111111', seconds: 7_200 },
    ]),
  ];
  const entries = legendEntries(days);
  assert.deepEqual(
    entries.map((entry) => [entry.categoryId, entry.seconds]),
    [
      ['a', 10_800],
      ['b', 7_200],
    ],
  );
  assert.equal(legendEntries(days, 1).length, 1);
});

// ── heatmap ──────────────────────────────────────────────────────────────

test('heatmapMax floors at 1 and heatmapOpacity is the share of it', () => {
  assert.equal(heatmapMax([{ weekday: 0, seconds: [0, 3_600, 1_800] }]), 3_600);
  assert.equal(heatmapMax([{ weekday: 0, seconds: [0] }]), 1);
  assert.equal(heatmapOpacity(1_800, 3_600), 0.5);
  assert.equal(heatmapOpacity(0, 3_600), 0);
  assert.equal(heatmapOpacity(7_200, 3_600), 1);
});

// ── picker ───────────────────────────────────────────────────────────────

test('RECORD_RANGES: the four values the server accepts', () => {
  assert.deepEqual(
    RECORD_RANGES.map((range) => range.key),
    ['7d', '30d', '90d', 'ytd'],
  );
  assert.deepEqual(
    RECORD_RANGES.map((range) => range.label),
    ['7d', '30d', '90d', 'YTD'],
  );
});
