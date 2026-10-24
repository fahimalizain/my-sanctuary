// Unit tests for the per-window server-read stamps (the clock half of the
// local change layer's retirement rule).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  calendarReadWindowKey,
  calendarServerReadStamp,
  nextCalendarStamp,
  stampCalendarServerReadWindow,
} from './calendar-server-reads';

const MIN = '2026-10-18T00:00:00.000Z';
const MAX = '2026-10-25T00:00:00.000Z';
const key = calendarReadWindowKey(MIN, MAX);

test('a window starts unread (0) and takes the latest completed read stamp', () => {
  assert.equal(calendarServerReadStamp(MIN, MAX), 0);

  const first = nextCalendarStamp();
  stampCalendarServerReadWindow(MIN, MAX, first);
  assert.equal(calendarServerReadStamp(MIN, MAX), first);

  const second = nextCalendarStamp();
  stampCalendarServerReadWindow(MIN, MAX, second);
  assert.equal(calendarServerReadStamp(MIN, MAX), second);
});

test('an older read that resolves later cannot lower the window stamp', () => {
  const older = nextCalendarStamp();
  const newer = nextCalendarStamp();
  stampCalendarServerReadWindow(MIN, MAX, newer);
  // The `older` read (smaller start stamp) resolves afterwards: it may not
  // take the window back in time.
  stampCalendarServerReadWindow(MIN, MAX, older);
  assert.equal(calendarServerReadStamp(MIN, MAX), newer);
});

test('windows are independent', () => {
  const otherMin = '2026-11-01T00:00:00.000Z';
  const otherMax = '2026-11-08T00:00:00.000Z';
  const stamp = nextCalendarStamp();
  stampCalendarServerReadWindow(MIN, MAX, stamp);

  assert.notEqual(
    calendarReadWindowKey(MIN, MAX),
    calendarReadWindowKey(otherMin, otherMax),
  );
  assert.equal(calendarServerReadStamp(otherMin, otherMax), 0);
  assert.equal(key, calendarReadWindowKey(MIN, MAX));
});
