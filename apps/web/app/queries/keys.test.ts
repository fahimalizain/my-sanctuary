import { test } from 'node:test';
import assert from 'node:assert/strict';
import { queryKeys } from './keys';

test('queryKeys.lists.all is ["lists"]', () => {
  assert.deepEqual(queryKeys.lists.all, ['lists']);
});

test('queryKeys.tasks.all is ["tasks"]', () => {
  assert.deepEqual(queryKeys.tasks.all, ['tasks']);
});

test('queryKeys.agenda.stream() is ["agenda", "stream"]', () => {
  assert.deepEqual(queryKeys.agenda.stream(), ['agenda', 'stream']);
});

test('queryKeys.agenda.stream() starts with queryKeys.agenda.all', () => {
  assert.deepEqual(
    queryKeys.agenda.stream().slice(0, queryKeys.agenda.all.length),
    queryKeys.agenda.all,
  );
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
