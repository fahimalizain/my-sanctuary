// Unit tests for the gutter time helpers: the compact pill format, the
// midnight-end rule, the pill / hour-label collision layout, and the
// visible-band stick that keeps a scrolled-off edge grabbable.
// No React / DOM.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  GUTTER_HOUR_LABEL_H,
  GUTTER_PILL_GAP,
  GUTTER_PILL_H,
  formatGutterTime,
  gutterEdgeMinutes,
  layoutGutterPills,
  separateEdgeCenters,
  type GutterPillLayout,
} from './gutter-time';
import { formatHourLabel } from './week-layout';

/** Every hour, as `hourLabelStep(48)` (step 1) would paint them. */
const ALL_HOURS = Array.from({ length: 24 }, (_, h) => h);

/** The separation the layout asks for: a pill box plus a hairline of clear. */
const MIN_SEP = GUTTER_PILL_H + GUTTER_PILL_GAP;

function at(hour: number, minute = 0): Date {
  return new Date(2026, 8, 8, hour, minute, 0, 0);
}

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
  // Both centers clamp toward the top (8 and 24), which is only 16px apart:
  // the pair is pushed apart to 18 instead, so 8 / 26.
  assert.equal(byKey(pills, 'start')?.top, GUTTER_PILL_H / 2);
  assert.equal(byKey(pills, 'start')?.top, 8);
  assert.equal(byKey(pills, 'end')?.top, 8 + MIN_SEP);
  assert.equal(byKey(pills, 'end')?.top, 26);
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

test('layoutGutterPills: a zero-length event keeps both pills apart', () => {
  const at9 = new Date(2026, 8, 8, 9, 0, 0, 0);
  const { pills, hiddenHours } = layoutGutterPills({
    hourH: 48,
    hourLabels: ALL_HOURS,
    now: null,
    edges: { start: at9, end: at9 },
  });

  // Both edges read 9:00a, and their centers straddle 9:00 (9 × 48 = 432) by
  // half the separation, so each one stays a grabbable pill of its own.
  assert.equal(pills.length, 2);
  assert.equal(byKey(pills, 'start')?.top, 432 - MIN_SEP / 2);
  assert.equal(byKey(pills, 'start')?.top, 423);
  assert.equal(byKey(pills, 'end')?.top, 432 + MIN_SEP / 2);
  assert.equal(byKey(pills, 'end')?.top, 441);
  assert.equal(byKey(pills, 'start')?.text, '9:00a');
  assert.equal(byKey(pills, 'end')?.text, '9:00a');
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
  // A 9:30 zero-length event at hourH 48 separates about 456 → 447 / 465, and
  // the 9 o'clock (432) and 10 o'clock (480) labels are 15px clear of each.
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

// ── separateEdgeCenters ─────────────────────────────────────────────────

test('separateEdgeCenters: two centers 4px apart become minSep apart', () => {
  const r = separateEdgeCenters(100, 104, 0, 200, MIN_SEP);
  assert.equal(r.startTop, 102 - MIN_SEP / 2);
  assert.equal(r.endTop, 102 + MIN_SEP / 2);
  assert.equal(r.endTop - r.startTop, MIN_SEP);
});

test('separateEdgeCenters: a pair near an end is slid back inside the range', () => {
  const r = separateEdgeCenters(4, 6, 8, 1144, MIN_SEP);
  assert.equal(r.startTop, 8);
  assert.equal(r.endTop, 8 + MIN_SEP);
});

test('separateEdgeCenters: an overnight pair keeps its ends apart', () => {
  // End above start: the pair spreads the way it already points, so the keys
  // are never swapped to sort it.
  const r = separateEdgeCenters(100, 98, 0, 200, MIN_SEP);
  assert.equal(r.startTop, 108);
  assert.equal(r.endTop, 90);
  assert.ok(r.endTop < r.startTop);
});

test('separateEdgeCenters: a range shorter than minSep still returns finite centers', () => {
  const r = separateEdgeCenters(4, 6, 0, 10, MIN_SEP);
  for (const v of [r.startTop, r.endTop]) {
    assert.ok(Number.isFinite(v));
    assert.ok(v >= 0 && v <= 10, `${v} outside the range`);
  }
});

test('separateEdgeCenters: non-finite centers pin to the top of the range', () => {
  assert.deepEqual(separateEdgeCenters(Number.NaN, 100, 8, 1144, MIN_SEP), {
    startTop: 8,
    endTop: 100,
  });
  assert.deepEqual(
    separateEdgeCenters(Number.NaN, Number.NaN, 8, 1144, MIN_SEP),
    { startTop: 8, endTop: 8 },
  );
});

// ── visible band stick ──────────────────────────────────────────────────

test('layoutGutterPills: a start off the top sticks to the visible band', () => {
  // Hours scrolled so 600 is the first visible row: the 9:00 start (432) is off
  // screen and pins to 600 + half a pill; the 4:00p end (768) is on it.
  const { pills } = layoutGutterPills({
    hourH: 48,
    hourLabels: ALL_HOURS,
    now: null,
    edges: { start: at(9), end: at(16) },
    visible: { top: 600, bottom: 800 },
  });
  assert.equal(byKey(pills, 'start')?.top, 600 + GUTTER_PILL_H / 2);
  assert.equal(byKey(pills, 'start')?.top, 608);
  assert.equal(byKey(pills, 'end')?.top, 16 * 48);
  assert.equal(byKey(pills, 'end')?.top, 768);
  assert.equal(byKey(pills, 'start')?.text, '9:00a');
});

test('layoutGutterPills: an end off the bottom sticks to the visible band', () => {
  const { pills } = layoutGutterPills({
    hourH: 48,
    hourLabels: ALL_HOURS,
    now: null,
    edges: { start: at(9), end: at(22) },
    visible: { top: 400, bottom: 800 },
  });
  assert.equal(byKey(pills, 'start')?.top, 9 * 48);
  assert.equal(byKey(pills, 'end')?.top, 800 - GUTTER_PILL_H / 2);
  assert.equal(byKey(pills, 'end')?.top, 792);
  assert.equal(byKey(pills, 'end')?.text, '10:00p');
});

test('layoutGutterPills: a visible band outside the hours box is ignored', () => {
  const { pills } = layoutGutterPills({
    hourH: 48,
    hourLabels: ALL_HOURS,
    now: null,
    edges: { start: at(9), end: at(10) },
    visible: { top: -500, bottom: -400 },
  });
  assert.equal(byKey(pills, 'start')?.top, 9 * 48);
  assert.equal(byKey(pills, 'end')?.top, 10 * 48);
});

test('layoutGutterPills: the now pill does not stick to the visible band', () => {
  // Hours scrolled to 600–800 with now at noon (12 × 48 = 576), a band away
  // above it. Sticking is for the grabbable edge pills: the now pill stays on
  // the now line and only the hours box bounds it.
  const { pills, hiddenHours } = layoutGutterPills({
    hourH: 48,
    hourLabels: ALL_HOURS,
    now: at(12),
    edges: null,
    visible: { top: 600, bottom: 800 },
  });
  const nowPill = byKey(pills, 'now');
  assert.ok(nowPill);
  assert.equal(nowPill.top, 576);
  assert.notEqual(nowPill.top, 600 + GUTTER_PILL_H / 2);
  assert.equal(nowPill.text, '12:00p');
  assert.equal(pills.length, 1);
  assert.deepEqual(hiddenHours, [12]);
});

test('layoutGutterPills: the now pill yields to the center the push landed on', () => {
  // Zero-length 9:00 → centers 423 / 441. A 9:20 now pill (448) is exactly one
  // pill clear of the unseparated 432, but not of the separated 441.
  const at9 = at(9);
  const { pills } = layoutGutterPills({
    hourH: 48,
    hourLabels: ALL_HOURS,
    now: at(9, 20),
    edges: { start: at9, end: at9 },
  });
  assert.equal(byKey(pills, 'start')?.top, 423);
  assert.equal(byKey(pills, 'end')?.top, 441);
  assert.equal(byKey(pills, 'now'), undefined);
  assert.equal(pills.length, 2);
  assert.deepEqual(
    layoutGutterPills({
      hourH: 48,
      hourLabels: ALL_HOURS,
      now: at(9, 20),
      edges: { start: at9, end: at9 },
    }).hiddenHours,
    [9],
  );
});

test('layoutGutterPills: hour hiding uses the centers after the push', () => {
  // 5:30–5:40 → 264 / 272, pushed to 259 / 277. The 6 o'clock label (288) is
  // 16px clear of the raw end but only 11px clear of the placed one, so only
  // the painted layout hides it.
  const { pills, hiddenHours } = layoutGutterPills({
    hourH: 48,
    hourLabels: ALL_HOURS,
    now: null,
    edges: { start: at(5, 30), end: at(5, 40) },
  });
  assert.equal(byKey(pills, 'start')?.top, 259);
  assert.equal(byKey(pills, 'end')?.top, 277);
  assert.deepEqual(hiddenHours, [6]);
});
