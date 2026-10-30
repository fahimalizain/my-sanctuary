import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  expiryLabel,
  formatExpiryDate,
  isExpired,
  lastUsedLabel,
} from './api-key-format';

// A fixed instant so the boundary tests never depend on the wall clock.
const NOW = Date.parse('2026-11-12T12:00:00Z');

// ── formatExpiryDate ─────────────────────────────────────────────────────

test('formatExpiryDate: YYYY-MM-DD → day month year', () => {
  assert.equal(formatExpiryDate('2026-11-12'), '12 Nov 2026');
});

test('formatExpiryDate: single-digit day is not zero-padded', () => {
  assert.equal(formatExpiryDate('2026-11-02'), '2 Nov 2026');
  assert.equal(formatExpiryDate('2026-01-05'), '5 Jan 2026');
});

test('formatExpiryDate: leap day formats like any other date', () => {
  assert.equal(formatExpiryDate('2024-02-29'), '29 Feb 2024');
});

test('formatExpiryDate is manual — the civil date survives every TZ', () => {
  // `new Date('2026-11-12')` is UTC midnight, so a negative-offset zone would
  // render the day before. The helper never constructs a Date, and no TZ is
  // set here: the string in is the string out, always.
  assert.equal(formatExpiryDate('2026-11-12'), '12 Nov 2026');
});

// ── isExpired ────────────────────────────────────────────────────────────

test('isExpired: null never expires', () => {
  assert.equal(isExpired(null, NOW), false);
});

test('isExpired: a future instant is live', () => {
  assert.equal(isExpired('2026-11-12T12:00:01Z', NOW), false);
});

test('isExpired: a past instant is expired', () => {
  assert.equal(isExpired('2026-11-12T11:59:59Z', NOW), true);
});

test('isExpired: the exact instant counts as expired (server parity)', () => {
  assert.equal(isExpired('2026-11-12T12:00:00Z', NOW), true);
});

test('isExpired: an unparseable instant is treated as live', () => {
  assert.equal(isExpired('not-a-date', NOW), false);
});

// ── expiryLabel ──────────────────────────────────────────────────────────

test('expiryLabel: no expiry reads "Never expires"', () => {
  assert.equal(
    expiryLabel({ expires_on: null, expires_at: null }, NOW),
    'Never expires',
  );
});

test('expiryLabel: a live key shows its civil expiry date', () => {
  assert.equal(
    expiryLabel(
      { expires_on: '2026-12-01', expires_at: '2026-12-01T00:00:00Z' },
      NOW,
    ),
    'Expires 1 Dec 2026',
  );
});

test('expiryLabel: a past key reads "Expired" with the civil date', () => {
  assert.equal(
    expiryLabel(
      { expires_on: '2026-11-01', expires_at: '2026-11-01T00:00:00Z' },
      NOW,
    ),
    'Expired 1 Nov 2026',
  );
});

test('expiryLabel: the verdict follows expires_at, the date follows expires_on', () => {
  assert.equal(
    expiryLabel(
      { expires_on: '2026-12-01', expires_at: '2026-12-02T00:00:00Z' },
      NOW,
    ),
    'Expires 1 Dec 2026',
  );
});

test('expiryLabel: the exact instant flips the row to Expired', () => {
  assert.equal(
    expiryLabel(
      { expires_on: '2026-11-12', expires_at: '2026-11-12T12:00:00Z' },
      NOW,
    ),
    'Expired 12 Nov 2026',
  );
});

// ── lastUsedLabel ────────────────────────────────────────────────────────

test('lastUsedLabel: null reads "Never used"', () => {
  assert.equal(lastUsedLabel(null), 'Never used');
});

test('lastUsedLabel: an unparseable instant reads "Never used"', () => {
  assert.equal(lastUsedLabel('not-a-date'), 'Never used');
});

test('lastUsedLabel: en-GB medium date + short time', () => {
  // No `Z`: parsed as local time, so the formatted clock time is 14:05 in
  // every zone (the expectation is not TZ-dependent).
  assert.equal(
    lastUsedLabel('2026-11-12T14:05:00'),
    'Last used 12 Nov 2026, 14:05',
  );
});
