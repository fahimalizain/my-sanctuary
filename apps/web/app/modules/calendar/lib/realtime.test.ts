// Unit tests for UserHub realtime message parsing.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parseRealtimeMessage } from './realtime';

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
  const msg = parseRealtimeMessage(JSON.stringify({ type: 'calendar.changed' }));
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
