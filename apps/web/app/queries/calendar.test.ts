import { test } from 'node:test';
import assert from 'node:assert/strict';
import type {
  CalendarEvent,
  CalendarEventsResponse,
  CalendarEventsSync,
} from '@/app/types';
import {
  applyCalendarEventRemove,
  applyCalendarEventUpsert,
} from './calendar-events-cache';

const sync: CalendarEventsSync = {
  status: 'degraded',
  calendars: [
    {
      calendar_id: 'cal-1',
      state: 'retrying',
      initial_sync_complete: true,
      last_success_at: '2026-09-01T00:00:00.000Z',
      last_attempt_at: '2026-09-10T00:00:00.000Z',
      stale: true,
      error_code: 'rate_limited',
      retry_after_seconds: 60,
      projection: 'timed_masters_and_exceptions',
      cache_revision: 3,
      watch_coverage: 'missing',
    },
  ],
};

function event(
  overrides: Partial<CalendarEvent> & Pick<CalendarEvent, 'id'>,
): CalendarEvent {
  return {
    calendar_id: 'cal-1',
    google_event_id: 'g-1',
    title: 'Event',
    description: '',
    start_time: '2026-09-10T09:00:00.000Z',
    end_time: '2026-09-10T10:00:00.000Z',
    last_synced_at: '2026-09-10T00:00:00.000Z',
    ...overrides,
  };
}

const e1 = event({ id: 'e1', title: 'One' });
const e2 = event({ id: 'e2', title: 'Two', google_event_id: 'g-2' });

function envelope(
  events: CalendarEvent[],
  extras?: Partial<CalendarEventsResponse>,
): CalendarEventsResponse {
  return {
    events,
    source: 'cache',
    sync,
    ...extras,
  };
}

test('applyCalendarEventUpsert: replace keeps sync + source', () => {
  const old = envelope([e1, e2]);
  const updated = event({ id: 'e1', title: 'One moved' });
  const next = applyCalendarEventUpsert(old, updated);
  assert.ok(next);
  assert.equal(next.events[0]?.title, 'One moved');
  assert.equal(next.events.length, 2);
  assert.equal(next.source, 'cache');
  assert.deepEqual(next.sync, sync);
  assert.notEqual(next, old);
});

test('applyCalendarEventUpsert: append keeps sync + source', () => {
  const old = envelope([e1], { source: 'window' });
  const added = event({ id: 'e3', title: 'Three', google_event_id: 'g-3' });
  const next = applyCalendarEventUpsert(old, added);
  assert.ok(next);
  assert.equal(next.events.length, 2);
  assert.equal(next.events[1]?.id, 'e3');
  assert.equal(next.source, 'window');
  assert.deepEqual(next.sync, sync);
});

test('applyCalendarEventRemove: keeps sync + source', () => {
  const old = envelope([e1, e2], { source: 'mixed' });
  const next = applyCalendarEventRemove(old, 'e1');
  assert.ok(next);
  assert.deepEqual(
    next.events.map((e) => e.id),
    ['e2'],
  );
  assert.equal(next.source, 'mixed');
  assert.deepEqual(next.sync, sync);
});

test('applyCalendarEventUpsert: missing old is no-op', () => {
  assert.equal(applyCalendarEventUpsert(undefined, e1), undefined);
});

test('applyCalendarEventUpsert: missing events is no-op', () => {
  const old = { source: 'cache', sync } as CalendarEventsResponse;
  assert.equal(applyCalendarEventUpsert(old, e1), old);
});

test('applyCalendarEventRemove: missing old is no-op', () => {
  assert.equal(applyCalendarEventRemove(undefined, 'e1'), undefined);
});

test('applyCalendarEventRemove: missing events is no-op', () => {
  const old = { source: 'cache', sync } as CalendarEventsResponse;
  assert.equal(applyCalendarEventRemove(old, 'e1'), old);
});

test('applyCalendarEventRemove: unknown id returns same reference', () => {
  const old = envelope([e1]);
  assert.equal(applyCalendarEventRemove(old, 'missing'), old);
});
