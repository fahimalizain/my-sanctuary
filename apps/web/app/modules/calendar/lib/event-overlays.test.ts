// Unit tests for calendar event overlay merge (optimistic move/resize paint).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { CalendarEvent } from '@/app/types';
import {
  applyEventOverlays,
  isTempEventId,
  resetEventOverlays,
  TEMP_EVENT_PREFIX,
  type EventOverlay,
} from './event-overlays';

test('isTempEventId: true for tmp_ prefix', () => {
  assert.equal(isTempEventId(`${TEMP_EVENT_PREFIX}abc`), true);
  assert.equal(isTempEventId('tmp_'), true);
});

test('isTempEventId: false for server ids', () => {
  assert.equal(isTempEventId('evt-123'), false);
  assert.equal(isTempEventId('tmp'), false);
  assert.equal(isTempEventId('xtmp_1'), false);
});

test('isTempEventId: false for empty string', () => {
  assert.equal(isTempEventId(''), false);
});

function makeEvent(
  overrides: Partial<CalendarEvent> &
    Pick<CalendarEvent, 'id' | 'start_time' | 'end_time'>,
): CalendarEvent {
  return {
    calendar_id: 'cal-1',
    google_event_id: 'g-1',
    title: 'Event',
    description: '',
    last_synced_at: '2026-09-08T00:00:00.000Z',
    ...overrides,
  };
}

const e1 = makeEvent({
  id: 'e1',
  title: 'One',
  start_time: '2026-09-08T09:00:00.000Z',
  end_time: '2026-09-08T10:00:00.000Z',
});
const e2 = makeEvent({
  id: 'e2',
  title: 'Two',
  start_time: '2026-09-08T11:00:00.000Z',
  end_time: '2026-09-08T12:00:00.000Z',
});
const e3 = makeEvent({
  id: 'e3',
  title: 'Three',
  start_time: '2026-09-08T13:00:00.000Z',
  end_time: '2026-09-08T14:00:00.000Z',
});

test('applyEventOverlays: empty overlays returns server list unchanged', () => {
  const server = [e1, e2];
  const result = applyEventOverlays(server, []);
  assert.deepEqual(result, server);
  assert.notEqual(result, server); // new array
});

test('applyEventOverlays: empty server + upsert appends', () => {
  const result = applyEventOverlays([], [{ op: 'upsert', event: e1 }]);
  assert.deepEqual(result, [e1]);
});

test('applyEventOverlays: empty server + delete is no-op', () => {
  const result = applyEventOverlays([], [{ op: 'delete', id: 'missing' }]);
  assert.deepEqual(result, []);
});

test('applyEventOverlays: upsert replaces matching id in place', () => {
  const moved = makeEvent({
    id: 'e1',
    title: 'One',
    start_time: '2026-09-08T15:00:00.000Z',
    end_time: '2026-09-08T16:00:00.000Z',
  });
  const result = applyEventOverlays(
    [e1, e2, e3],
    [{ op: 'upsert', event: moved }],
  );
  assert.equal(result.length, 3);
  assert.equal(result[0], moved);
  assert.equal(result[1], e2);
  assert.equal(result[2], e3);
  assert.equal(result[0].start_time, '2026-09-08T15:00:00.000Z');
});

test('applyEventOverlays: upsert of unknown id appends at end', () => {
  const result = applyEventOverlays([e1, e2], [{ op: 'upsert', event: e3 }]);
  assert.deepEqual(
    result.map((e) => e.id),
    ['e1', 'e2', 'e3'],
  );
  assert.equal(result[2], e3);
});

test('applyEventOverlays: delete drops matching id', () => {
  const result = applyEventOverlays([e1, e2, e3], [{ op: 'delete', id: 'e2' }]);
  assert.deepEqual(
    result.map((e) => e.id),
    ['e1', 'e3'],
  );
});

test('applyEventOverlays: delete of unknown id is no-op', () => {
  const result = applyEventOverlays(
    [e1, e2],
    [{ op: 'delete', id: 'missing' }],
  );
  assert.deepEqual(
    result.map((e) => e.id),
    ['e1', 'e2'],
  );
});

test('applyEventOverlays: delete then upsert → last wins (event present)', () => {
  const overlays: EventOverlay[] = [
    { op: 'delete', id: 'e1' },
    { op: 'upsert', event: e1 },
  ];
  const result = applyEventOverlays([e1, e2], overlays);
  assert.deepEqual(
    result.map((e) => e.id),
    ['e1', 'e2'],
  );
  assert.equal(result[0], e1);
});

test('applyEventOverlays: upsert then delete → last wins (event gone)', () => {
  const moved = makeEvent({
    id: 'e1',
    title: 'Moved',
    start_time: '2026-09-08T15:00:00.000Z',
    end_time: '2026-09-08T16:00:00.000Z',
  });
  const overlays: EventOverlay[] = [
    { op: 'upsert', event: moved },
    { op: 'delete', id: 'e1' },
  ];
  const result = applyEventOverlays([e1, e2], overlays);
  assert.deepEqual(
    result.map((e) => e.id),
    ['e2'],
  );
});

test('applyEventOverlays: two upserts same id → last wins', () => {
  const first = makeEvent({
    id: 'e1',
    title: 'First',
    start_time: '2026-09-08T10:00:00.000Z',
    end_time: '2026-09-08T11:00:00.000Z',
  });
  const second = makeEvent({
    id: 'e1',
    title: 'Second',
    start_time: '2026-09-08T14:00:00.000Z',
    end_time: '2026-09-08T15:00:00.000Z',
  });
  const result = applyEventOverlays(
    [e1],
    [
      { op: 'upsert', event: first },
      { op: 'upsert', event: second },
    ],
  );
  assert.equal(result.length, 1);
  assert.equal(result[0], second);
  assert.equal(result[0].title, 'Second');
});

test('applyEventOverlays: preserves relative order of server rows', () => {
  const moved2 = makeEvent({
    id: 'e2',
    title: 'Two moved',
    start_time: '2026-09-08T18:00:00.000Z',
    end_time: '2026-09-08T19:00:00.000Z',
  });
  const result = applyEventOverlays(
    [e1, e2, e3],
    [{ op: 'upsert', event: moved2 }],
  );
  assert.deepEqual(
    result.map((e) => e.id),
    ['e1', 'e2', 'e3'],
  );
});

test('applyEventOverlays: multiple overlays on different ids', () => {
  const moved1 = makeEvent({
    id: 'e1',
    title: 'One moved',
    start_time: '2026-09-08T07:00:00.000Z',
    end_time: '2026-09-08T08:00:00.000Z',
  });
  const result = applyEventOverlays(
    [e1, e2, e3],
    [
      { op: 'upsert', event: moved1 },
      { op: 'delete', id: 'e3' },
    ],
  );
  assert.deepEqual(
    result.map((e) => e.id),
    ['e1', 'e2'],
  );
  assert.equal(result[0], moved1);
});

test('applyEventOverlays: accepts Map values iterable', () => {
  const moved = makeEvent({
    id: 'e2',
    title: 'Two',
    start_time: '2026-09-08T20:00:00.000Z',
    end_time: '2026-09-08T21:00:00.000Z',
  });
  const map = new Map<string, EventOverlay>([
    ['e2', { op: 'upsert', event: moved }],
  ]);
  const result = applyEventOverlays([e1, e2], map.values());
  assert.equal(result[1], moved);
});

test('resetEventOverlays: clears map so apply matches server list', () => {
  const moved = makeEvent({
    id: 'e1',
    title: 'Moved',
    start_time: '2026-09-08T15:00:00.000Z',
    end_time: '2026-09-08T16:00:00.000Z',
  });
  const map = new Map<string, EventOverlay>([
    ['e1', { op: 'upsert', event: moved }],
    ['e2', { op: 'delete', id: 'e2' }],
  ]);
  const server = [e1, e2];
  assert.notDeepEqual(
    applyEventOverlays(server, map.values()).map((e) => e.id),
    server.map((e) => e.id),
  );

  resetEventOverlays(map);
  assert.equal(map.size, 0);
  assert.deepEqual(applyEventOverlays(server, map.values()), server);
});
