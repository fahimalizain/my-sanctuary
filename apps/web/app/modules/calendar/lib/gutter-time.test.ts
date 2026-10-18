// Unit tests for the gutter time helpers: the compact pill format, the
// midnight-end rule, and the pill / hour-label collision layout.
// No React / DOM.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  GUTTER_HOUR_LABEL_H,
  GUTTER_PILL_H,
  formatGutterTime,
  gutterEdgeMinutes,
  layoutGutterPills,
  type GutterPillLayout,
} from './gutter-time';
import { formatHourLabel } from './week-layout';

/** Every hour, as `hourLabelStep(48)` (step 1) would paint them. */
const ALL_HOURS = Array.from({ length: 24 }, (_, h) => h);

function byKey(
  pills: GutterPillLayout[],
  key: GutterPillLayout['key'],
): GutterPillLayout | undefined {
  return pills.find((p) => p.key === key);
}

// ── formatGutterTime ────────────────────────────────────────────────────

test('formatGutterTime: compact 12-hour, minutes always, lowercase meridian', () => {
  assert.equal(formatGutterTime(new Date(2026, 8, 8, 9, 0)), '9:00a');
  assert.equal(formatGutterTime(new Date(2026, 8, 8, 9, 5)), '9:05a');
  assert.equal(formatGutterTime(new Date(2026, 8, 8, 0, 0)), '12:00a');
  assert.equal(formatGutterTime(new Date(2026, 8, 8, 12, 0)), '12:00p');
  assert.equal(formatGutterTime(new Date(2026, 8, 8, 12, 30)), '12:30p');
  assert.equal(formatGutterTime(new Date(2026, 8, 8, 23, 7)), '11:07p');
});

test('formatGutterTime: seconds are ignored, not rounded up', () => {
  assert.equal(formatGutterTime(new Date(2026, 8, 8, 9, 5, 40)), '9:05a');
  assert.equal(formatGutterTime(new Date(2026, 8, 8, 9, 5, 59, 999)), '9:05a');
});

test('formatGutterTime: stays distinct from the hour label of the same hour', () => {
  // "9AM" (hour label) vs "9:00a" (pill): the colon + lowercase meridian is
  // what keeps a pill from reading as another row label.
  assert.equal(formatHourLabel(9), '9AM');
  assert.equal(formatGutterTime(new Date(2026, 8, 8, 9, 0)), '9:00a');
  assert.notEqual(
    formatGutterTime(new Date(2026, 8, 8, 9, 0)),
    formatHourLabel(9),
  );
});

// ── gutterEdgeMinutes ───────────────────────────────────────────────────

test('gutterEdgeMinutes: 9:00–10:30', () => {
  const r = gutterEdgeMinutes(
    new Date(2026, 8, 8, 9, 0),
    new Date(2026, 8, 8, 10, 30),
  );
  assert.deepEqual(r, { startMin: 540, endMin: 630 });
});

test('gutterEdgeMinutes: 23:00 – next local midnight bottoms out at 1440', () => {
  const r = gutterEdgeMinutes(
    new Date(2026, 8, 8, 23, 0),
    new Date(2026, 8, 9, 0, 0, 0, 0),
  );
  assert.deepEqual(r, { startMin: 23 * 60, endMin: 1440 });
});

test('gutterEdgeMinutes: overnight 22:00–01:30 keeps both clocks', () => {
  const r = gutterEdgeMinutes(
    new Date(2026, 8, 8, 22, 0),
    new Date(2026, 8, 9, 1, 30),
  );
  assert.deepEqual(r, { startMin: 1320, endMin: 90 });
});

test('gutterEdgeMinutes: zero-length at midnight stays 0 for both', () => {
  const midnight = new Date(2026, 8, 8, 0, 0, 0, 0);
  assert.deepEqual(gutterEdgeMinutes(midnight, midnight), {
    startMin: 0,
    endMin: 0,
  });
});

test('gutterEdgeMinutes: an inverted range is not reordered', () => {
  const r = gutterEdgeMinutes(
    new Date(2026, 8, 8, 18, 0),
    new Date(2026, 8, 8, 9, 0),
  );
  assert.deepEqual(r, { startMin: 18 * 60, endMin: 9 * 60 });
});

test('gutterEdgeMinutes: non-finite instants → 0 / 0', () => {
  const bad = new Date(Number.NaN);
  const good = new Date(2026, 8, 8, 9, 0);
  assert.deepEqual(gutterEdgeMinutes(bad, good), { startMin: 0, endMin: 0 });
  assert.deepEqual(gutterEdgeMinutes(good, bad), { startMin: 0, endMin: 0 });
  assert.deepEqual(gutterEdgeMinutes(bad, bad), { startMin: 0, endMin: 0 });
});

// ── layoutGutterPills ───────────────────────────────────────────────────

test('layoutGutterPills: noon now pill and 9:00–10:00 edges at hourH 48', () => {
  const { pills, hiddenHours } = layoutGutterPills({
    hourH: 48,
    hourLabels: ALL_HOURS,
    now: new Date(2026, 8, 8, 12, 0, 0, 0),
    edges: {
      start: new Date(2026, 8, 8, 9, 0),
      end: new Date(2026, 8, 8, 10, 0),
    },
  });

  assert.equal(pills.length, 3);
  const nowPill = byKey(pills, 'now');
  assert.ok(nowPill);
  assert.equal(nowPill.top, 576);
  assert.equal(nowPill.text, '12:00p');
  assert.equal(nowPill.minutes, 720);

  const startPill = byKey(pills, 'start');
  assert.ok(startPill);
  assert.equal(startPill.top, 9 * 48);
  assert.equal(startPill.text, '9:00a');
  assert.equal(startPill.minutes, 540);

  const endPill = byKey(pills, 'end');
  assert.ok(endPill);
  assert.equal(endPill.top, 10 * 48);
  assert.equal(endPill.text, '10:00a');
  assert.equal(endPill.minutes, 600);

  // Every pill hides the label it sits on (9, 10 and the now pill's 12), and
  // only those: hour 8 is a full 48px clear of the 9:00 edge.
  assert.deepEqual(hiddenHours, [9, 10, 12]);
  assert.ok(!hiddenHours.includes(8));
});

test('layoutGutterPills: the now pill yields to an edge it would cover', () => {
  const { pills, hiddenHours } = layoutGutterPills({
    hourH: 48,
    hourLabels: ALL_HOURS,
    now: new Date(2026, 8, 8, 9, 0, 0, 0),
    edges: {
      start: new Date(2026, 8, 8, 9, 0),
      end: new Date(2026, 8, 8, 10, 0),
    },
  });

  assert.equal(byKey(pills, 'now'), undefined);
  assert.equal(byKey(pills, 'start')?.top, 9 * 48);
  assert.deepEqual(hiddenHours, [9, 10]);
});

test('layoutGutterPills: a now pill inside a pill of an edge is dropped', () => {
  // 9:15 now sits 12px below the 9:00 start edge — under GUTTER_PILL_H, so the
  // edge wins even though the now pill is 36px clear of the 10:00 end.
  const { pills } = layoutGutterPills({
    hourH: 48,
    hourLabels: ALL_HOURS,
    now: new Date(2026, 8, 8, 9, 15, 0, 0),
    edges: {
      start: new Date(2026, 8, 8, 9, 0),
      end: new Date(2026, 8, 8, 10, 0),
    },
  });
  assert.equal(byKey(pills, 'now'), undefined);
  assert.equal(pills.length, 2);
});

test('layoutGutterPills: exact adjacency keeps the now pill', () => {
  // Exactly one pill away: 20 min at hourH 48 = 16px.
  const { pills } = layoutGutterPills({
    hourH: 48,
    hourLabels: ALL_HOURS,
    now: new Date(2026, 8, 8, 9, 20, 0, 0),
    edges: {
      start: new Date(2026, 8, 8, 9, 0),
      end: new Date(2026, 8, 8, 10, 0),
    },
  });
  const nowPill = byKey(pills, 'now');
  assert.ok(nowPill);
  assert.equal(nowPill.top, 9 * 48 + GUTTER_PILL_H);
  assert.equal(nowPill.text, '9:20a');
});

test('layoutGutterPills: the now pill is kept hours away from the edges', () => {
  const { pills } = layoutGutterPills({
    hourH: 48,
    hourLabels: ALL_HOURS,
    now: new Date(2026, 8, 8, 14, 0, 0, 0),
    edges: {
      start: new Date(2026, 8, 8, 9, 0),
      end: new Date(2026, 8, 8, 10, 0),
    },
  });
  assert.equal(pills.length, 3);
  assert.equal(byKey(pills, 'now')?.top, 14 * 48);
  assert.equal(byKey(pills, 'now')?.text, '2:00p');
});

test('layoutGutterPills: a 23:00–midnight event sits at both ends of the axis', () => {
  const { pills, hiddenHours } = layoutGutterPills({
    hourH: 48,
    hourLabels: ALL_HOURS,
    now: null,
    edges: {
      start: new Date(2026, 8, 8, 23, 0),
      end: new Date(2026, 8, 9, 0, 0, 0, 0),
    },
  });

  const startPill = byKey(pills, 'start');
  assert.ok(startPill);
  assert.equal(startPill.top, 23 * 48);
  assert.equal(startPill.text, '11:00p');

  const endPill = byKey(pills, 'end');
  assert.ok(endPill);
  assert.equal(endPill.minutes, 1440);
  assert.equal(endPill.top, 24 * 48 - GUTTER_PILL_H / 2);
  assert.equal(endPill.text, '12:00a');

  assert.deepEqual(hiddenHours, [23]);
});

test('layoutGutterPills: a 00:00 start pill is not sliced by the hours box', () => {
  const { pills } = layoutGutterPills({
    hourH: 48,
    hourLabels: ALL_HOURS,
    now: null,
    edges: {
      start: new Date(2026, 8, 8, 0, 0),
      end: new Date(2026, 8, 8, 0, 30),
    },
  });
  assert.equal(byKey(pills, 'start')?.top, GUTTER_PILL_H / 2);
  assert.equal(byKey(pills, 'start')?.top, 8);
  assert.equal(byKey(pills, 'end')?.top, 24);
});

test('layoutGutterPills: clamping feeds the now-pill collision test', () => {
  // 00:00–01:00 selected, now 00:05: both centers clamp toward the top edge,
  // so the now pill lands on the start pill and is dropped.
  const { pills } = layoutGutterPills({
    hourH: 48,
    hourLabels: ALL_HOURS,
    now: new Date(2026, 8, 8, 0, 5, 0, 0),
    edges: {
      start: new Date(2026, 8, 8, 0, 0),
      end: new Date(2026, 8, 8, 1, 0),
    },
  });
  assert.equal(byKey(pills, 'start')?.top, GUTTER_PILL_H / 2);
  assert.equal(byKey(pills, 'now'), undefined);
  assert.equal(pills.length, 2);
});

test('layoutGutterPills: a zero-length event keeps both pills', () => {
  const at = new Date(2026, 8, 8, 9, 0, 0, 0);
  const { pills, hiddenHours } = layoutGutterPills({
    hourH: 48,
    hourLabels: ALL_HOURS,
    now: null,
    edges: { start: at, end: at },
  });

  assert.equal(pills.length, 2);
  assert.equal(byKey(pills, 'start')?.top, 9 * 48);
  assert.equal(byKey(pills, 'end')?.top, 9 * 48);
  assert.equal(byKey(pills, 'start')?.text, '9:00a');
  assert.equal(byKey(pills, 'end')?.text, '9:00a');
  // Overlapping pills are left overlapping; push-apart is a later slice.
  assert.deepEqual(hiddenHours, [9]);
});

test('layoutGutterPills: an unusable hourH returns nothing', () => {
  for (const hourH of [Number.NaN, 0, -48, Number.POSITIVE_INFINITY]) {
    const { pills, hiddenHours } = layoutGutterPills({
      hourH,
      hourLabels: ALL_HOURS,
      now: new Date(2026, 8, 8, 12, 0),
      edges: {
        start: new Date(2026, 8, 8, 9, 0),
        end: new Date(2026, 8, 8, 10, 0),
      },
    });
    assert.deepEqual(pills, [], `hourH ${hourH}`);
    assert.deepEqual(hiddenHours, [], `hourH ${hourH}`);
  }
});

test('layoutGutterPills: no now and no edges → no pills, no hidden labels', () => {
  const { pills, hiddenHours } = layoutGutterPills({
    hourH: 48,
    hourLabels: ALL_HOURS,
    now: null,
    edges: null,
  });
  assert.deepEqual(pills, []);
  assert.deepEqual(hiddenHours, []);
});

test('layoutGutterPills: invalid instants are skipped', () => {
  const bad = new Date(Number.NaN);
  const badNow = layoutGutterPills({
    hourH: 48,
    hourLabels: ALL_HOURS,
    now: bad,
    edges: null,
  });
  assert.deepEqual(badNow.pills, []);

  const badEdges = layoutGutterPills({
    hourH: 48,
    hourLabels: ALL_HOURS,
    now: null,
    edges: { start: bad, end: new Date(2026, 8, 8, 10, 0) },
  });
  assert.deepEqual(badEdges.pills, []);
});

test('layoutGutterPills: hidden hours are always painted labels', () => {
  // hourLabelStep(20) = 2, so odd hours are not painted and must not be hidden.
  const labels = ALL_HOURS.filter((h) => h % 2 === 0);
  const { hiddenHours } = layoutGutterPills({
    hourH: 20,
    hourLabels: labels,
    now: new Date(2026, 8, 8, 12, 0, 0, 0),
    edges: null,
  });
  for (const h of hiddenHours) {
    assert.ok(labels.includes(h), `hour ${h} was not painted`);
  }
  assert.deepEqual(hiddenHours, [12]);
});

test('layoutGutterPills: GUTTER_HOUR_LABEL_H is the label box the clearance uses', () => {
  // A 9:30 edge at hourH 48 → top 456; the 9 o'clock label (432) is 24px off,
  // the 10 o'clock label (480) is 24px off: both clear of 13px half-heights.
  const { hiddenHours } = layoutGutterPills({
    hourH: 48,
    hourLabels: ALL_HOURS,
    now: null,
    edges: {
      start: new Date(2026, 8, 8, 9, 30),
      end: new Date(2026, 8, 8, 9, 30),
    },
  });
  assert.equal((GUTTER_PILL_H + GUTTER_HOUR_LABEL_H) / 2, 13);
  assert.deepEqual(hiddenHours, []);
});
