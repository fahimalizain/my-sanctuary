import { test } from 'node:test';
import assert from 'node:assert/strict';
import { queryKeys } from './keys';

test('queryKeys.lists.all is ["lists"]', () => {
  assert.deepEqual(queryKeys.lists.all, ['lists']);
});

test('queryKeys.tasks.all is ["tasks"]', () => {
  assert.deepEqual(queryKeys.tasks.all, ['tasks']);
});

test('queryKeys.agenda.byDate() with no/empty/undefined date uses the today sentinel', () => {
  assert.deepEqual(queryKeys.agenda.byDate(), ['agenda', 'today']);
  assert.deepEqual(queryKeys.agenda.byDate(''), ['agenda', 'today']);
  assert.deepEqual(queryKeys.agenda.byDate(undefined), ['agenda', 'today']);
});

test('queryKeys.agenda.byDate("2026-08-23") is ["agenda", "2026-08-23"]', () => {
  assert.deepEqual(queryKeys.agenda.byDate('2026-08-23'), [
    'agenda',
    '2026-08-23',
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

test('queryKeys.auth.me() starts with queryKeys.auth.all', () => {
  assert.deepEqual(
    queryKeys.auth.me().slice(0, queryKeys.auth.all.length),
    queryKeys.auth.all,
  );
});