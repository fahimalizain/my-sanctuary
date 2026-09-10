// Unit tests for UserHub realtime message parsing and catch-up policy.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  calendarCatchupQueryKeys,
  parseRealtimeMessage,
  shouldInvalidateCalendarOnMessage,
  shouldInvalidateCalendarOnOpen,
} from './realtime';

test('parseRealtimeMessage: valid calendar.changed with calendar_id', () => {
  const msg = parseRealtimeMessage(
    JSON.stringify({ type: 'calendar.changed', calendar_id: 'cal-1' }),
  );
  assert.deepEqual(msg, {
    type: 'calendar.changed',
    calendar_id: 'cal-1',
  });
});

test('parseRealtimeMessage: valid calendar.changed without calendar_id', () => {
  const msg = parseRealtimeMessage(
    JSON.stringify({ type: 'calendar.changed' }),
  );
  assert.deepEqual(msg, { type: 'calendar.changed' });
});

test('parseRealtimeMessage: unknown type → null', () => {
  assert.equal(
    parseRealtimeMessage(JSON.stringify({ type: 'unknown.event' })),
    null,
  );
});

test('parseRealtimeMessage: garbage → null', () => {
  assert.equal(parseRealtimeMessage('not-json'), null);
  assert.equal(parseRealtimeMessage(''), null);
  assert.equal(parseRealtimeMessage('null'), null);
  assert.equal(parseRealtimeMessage('[]'), null);
  assert.equal(parseRealtimeMessage('42'), null);
});

test('shouldInvalidateCalendarOnOpen: always true', () => {
  assert.equal(shouldInvalidateCalendarOnOpen(), true);
});

test('shouldInvalidateCalendarOnMessage: calendar.changed with calendar_id', () => {
  const msg = parseRealtimeMessage(
    JSON.stringify({ type: 'calendar.changed', calendar_id: 'cal-1' }),
  );
  assert.equal(shouldInvalidateCalendarOnMessage(msg), true);
});

test('shouldInvalidateCalendarOnMessage: calendar.changed without calendar_id', () => {
  const msg = parseRealtimeMessage(
    JSON.stringify({ type: 'calendar.changed' }),
  );
  assert.equal(shouldInvalidateCalendarOnMessage(msg), true);
});

test('shouldInvalidateCalendarOnMessage: null / unknown / garbage → false', () => {
  assert.equal(shouldInvalidateCalendarOnMessage(null), false);
  assert.equal(
    shouldInvalidateCalendarOnMessage(
      parseRealtimeMessage(JSON.stringify({ type: 'unknown.event' })),
    ),
    false,
  );
  assert.equal(
    shouldInvalidateCalendarOnMessage(parseRealtimeMessage('not-json')),
    false,
  );
});

test('calendarCatchupQueryKeys: events is prefix, not a range', () => {
  assert.deepEqual(calendarCatchupQueryKeys().events, ['calendar', 'events']);
});

test('calendarCatchupQueryKeys: calendars prefix', () => {
  assert.deepEqual(calendarCatchupQueryKeys().calendars, [
    'calendar',
    'calendars',
  ]);
});
