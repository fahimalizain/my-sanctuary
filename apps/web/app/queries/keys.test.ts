import { test } from 'node:test';
import assert from 'node:assert/strict';
import { queryKeys } from './keys';

test('queryKeys.lists.all is ["lists"]', () => {
  assert.deepEqual(queryKeys.lists.all, ['lists']);
});

test('queryKeys.tasks.all is ["tasks"]', () => {
  assert.deepEqual(queryKeys.tasks.all, ['tasks']);
});

test('queryKeys.agenda.current() is ["agenda", "current"]', () => {
  assert.deepEqual(queryKeys.agenda.current(), ['agenda', 'current']);
});

test('queryKeys.agenda.current() starts with queryKeys.agenda.all', () => {
  assert.deepEqual(
    queryKeys.agenda.current().slice(0, queryKeys.agenda.all.length),
    queryKeys.agenda.all,
  );
});

test('queryKeys.agenda.history(anchor) is ["agenda", "history", anchor]', () => {
  assert.deepEqual(queryKeys.agenda.history('2026-08-20'), [
    'agenda',
    'history',
    '2026-08-20',
  ]);
});

test('queryKeys.agenda.history() starts with queryKeys.agenda.all', () => {
  assert.deepEqual(
    queryKeys.agenda
      .history('2026-08-20')
      .slice(0, queryKeys.agenda.all.length),
    queryKeys.agenda.all,
  );
});

test('queryKeys.agenda.history(): each anchor is its own cache', () => {
  // The anchor IS the history key: a moved anchor must never read the
  // previous anchor's pages (they end before the new anchor's eve).
  assert.notDeepEqual(
    queryKeys.agenda.history('2026-08-20'),
    queryKeys.agenda.history('2026-08-18'),
  );
  assert.deepEqual(queryKeys.agenda.history('2026-08-18'), [
    'agenda',
    'history',
    '2026-08-18',
  ]);
});

test('queryKeys.calendar.events("a", "b") is ["calendar", "events", "a", "b"]', () => {
  assert.deepEqual(queryKeys.calendar.events('a', 'b'), [
    'calendar',
    'events',
    'a',
    'b',
  ]);
});

test('queryKeys.calendar.calendars() is ["calendar", "calendars"]', () => {
  assert.deepEqual(queryKeys.calendar.calendars(), ['calendar', 'calendars']);
});

test('queryKeys.auth.me() starts with queryKeys.auth.all', () => {
  assert.deepEqual(
    queryKeys.auth.me().slice(0, queryKeys.auth.all.length),
    queryKeys.auth.all,
  );
});
