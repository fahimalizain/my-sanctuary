import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  consumptionGroupLabel,
  consumptionGroupLine,
  consumptionShareLabel,
  durationLabel,
} from './consumption-model';

// ── shares ───────────────────────────────────────────────────────────────

test('consumptionShareLabel: a whole percent of the window total', () => {
  assert.equal(consumptionShareLabel(2_000, 10_000), '20%');
  assert.equal(consumptionShareLabel(10_000, 10_000), '100%');
  assert.equal(consumptionShareLabel(0, 10_000), '0%');
});

test('consumptionShareLabel: a share that rounds to zero is <1%, not 0%', () => {
  // 1 second out of 10 000 rounds to 0 — but the platform did log time.
  assert.equal(consumptionShareLabel(1, 10_000), '<1%');
  // A share that rounds to 1 stays 1%.
  assert.equal(consumptionShareLabel(100, 10_000), '1%');
});

test('consumptionShareLabel: nothing to divide by is 0%, never NaN', () => {
  assert.equal(consumptionShareLabel(100, 0), '0%');
  assert.equal(consumptionShareLabel(0, 0), '0%');
  assert.equal(consumptionShareLabel(100, -10), '0%');
  assert.equal(consumptionShareLabel(-100, 10_000), '0%');
});

// ── durations ────────────────────────────────────────────────────────────

test('durationLabel: minutes under an hour, to the nearest minute', () => {
  assert.equal(durationLabel(0), '0m');
  assert.equal(durationLabel(-30), '0m');
  assert.equal(durationLabel(60), '1m');
  assert.equal(durationLabel(840), '14m');
  assert.equal(durationLabel(3_000), '50m');
});

test('durationLabel: 30–89 seconds is 1m, 1–29 seconds is <1m', () => {
  // Math.round(0.5) is 1, so the half-minute rounds up.
  assert.equal(durationLabel(30), '1m');
  assert.equal(durationLabel(89), '1m');
  assert.equal(durationLabel(29), '<1m');
  assert.equal(durationLabel(1), '<1m');
});

test('durationLabel: hours with a zero-padded minute part', () => {
  assert.equal(durationLabel(12_960), '3h36m');
  assert.equal(durationLabel(36_540), '10h09m');
  assert.equal(durationLabel(7_980), '2h13m');
  assert.equal(durationLabel(3_600), '1h00m');
  // 59m40s rounds to 60 minutes: the carry is an hour, never `60m`.
  assert.equal(durationLabel(3_580), '1h00m');
});

// ── group labels ─────────────────────────────────────────────────────────

test('consumptionGroupLabel: known groups in words', () => {
  assert.equal(consumptionGroupLabel('series'), 'Series');
  assert.equal(consumptionGroupLabel('social'), 'Social');
  assert.equal(consumptionGroupLabel('messaging'), 'Messaging');
  assert.equal(consumptionGroupLabel('gaming'), 'Gaming');
});

test('consumptionGroupLabel: an unknown key is shown as itself', () => {
  assert.equal(consumptionGroupLabel('social_video'), 'social_video');
});

// ── group line ───────────────────────────────────────────────────────────

test('consumptionGroupLine: hours and share per group, server order kept', () => {
  const groups = [
    { group: 'series', seconds: 77_760 },
    { group: 'social', seconds: 73_800 },
  ];
  assert.equal(
    consumptionGroupLine(groups, 168_000),
    'Series 21.6h (46%) \u00b7 Social 20.5h (44%)',
  );
  // Space, U+00B7, space — not a hyphen or an ASCII middle dot.
  assert.ok(consumptionGroupLine(groups, 168_000).includes(' \u00b7 '));
});

test('consumptionGroupLine: the order passed in is preserved, not re-sorted', () => {
  const groups = [
    { group: 'gaming', seconds: 600 },
    { group: 'series', seconds: 3_600 },
  ];
  assert.equal(
    consumptionGroupLine(groups, 4_200),
    'Gaming 0.2h (14%) \u00b7 Series 1.0h (86%)',
  );
});

test('consumptionGroupLine: an unknown group is echoed back in the line', () => {
  assert.equal(
    consumptionGroupLine([{ group: 'media', seconds: 3_600 }], 3_600),
    'media 1.0h (100%)',
  );
});

test('consumptionGroupLine: a tiny group keeps the <1% form', () => {
  assert.equal(
    consumptionGroupLine([{ group: 'social', seconds: 1 }], 10_000),
    'Social 0.0h (<1%)',
  );
});

test('consumptionGroupLine: no groups is an empty line', () => {
  assert.equal(consumptionGroupLine([], 0), '');
  assert.equal(
    consumptionGroupLine([{ group: 'series', seconds: 0 }], 0),
    '',
  );
});
