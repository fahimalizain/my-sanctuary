import { test } from 'node:test';
import assert from 'node:assert/strict';
import { msUntilNextMidnight } from './agenda-clock';

test('missing or unknown zone returns null', () => {
  assert.equal(msUntilNextMidnight(''), null);
  assert.equal(msUntilNextMidnight('Not/AZone'), null);
});

test('23:30 local (UTC+3) is 30 minutes to midnight', () => {
  assert.equal(
    msUntilNextMidnight('Asia/Riyadh', new Date('2026-10-17T20:30:00.000Z')),
    1_800_000,
  );
});

test('23:59:59 local is one second to midnight', () => {
  assert.equal(
    msUntilNextMidnight('Asia/Riyadh', new Date('2026-10-17T20:59:59.000Z')),
    1_000,
  );
});

test('exactly local midnight is a full day, not zero', () => {
  assert.equal(
    msUntilNextMidnight('Asia/Riyadh', new Date('2026-10-17T21:00:00.000Z')),
    86_400_000,
  );
});
