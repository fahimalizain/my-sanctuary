import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { RoutineRecord } from '@/app/types';
import { planRoutineReorder, routinesInIdOrder } from './reorder';

/** A row the helpers only read `id`, `title`, and `sort_order` from. */
function routine(id: string, sort_order: number): RoutineRecord {
  return {
    id,
    user_id: 'u-1',
    title: `Title ${id}`,
    estimated_minutes: 15,
    rrule: 'DTSTART:20260101T063000\nRRULE:FREQ=DAILY',
    sort_order,
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    category: {
      id: 'cat-1',
      title: 'Fitness',
      slug: 'fitness',
      list_id: null,
      inherited_list_id: null,
      is_untracked: false,
      color: '#22c55e',
    },
  };
}

// ── planRoutineReorder ───────────────────────────────────────────────────

test('planRoutineReorder: a drag down re-ranks to 0..n-1 and keeps the titles', () => {
  const routines = [routine('a', 10), routine('b', 20), routine('c', 30)];

  const plan = planRoutineReorder(routines, 'a', 'c');

  assert.ok(plan);
  assert.deepEqual(plan.ids, ['b', 'c', 'a']);
  assert.deepEqual(
    plan.routines.map((row) => row.sort_order),
    [0, 1, 2],
  );
  assert.deepEqual(
    plan.routines.map((row) => row.title),
    ['Title b', 'Title c', 'Title a'],
  );
});

test('planRoutineReorder: a drag up re-ranks to 0..n-1 and keeps the titles', () => {
  const routines = [routine('a', 10), routine('b', 20), routine('c', 30)];

  const plan = planRoutineReorder(routines, 'c', 'a');

  assert.ok(plan);
  assert.deepEqual(plan.ids, ['c', 'a', 'b']);
  assert.deepEqual(
    plan.routines.map((row) => row.sort_order),
    [0, 1, 2],
  );
  assert.deepEqual(
    plan.routines.map((row) => row.title),
    ['Title c', 'Title a', 'Title b'],
  );
});

test('planRoutineReorder: same id is a no-op', () => {
  const routines = [routine('a', 10), routine('b', 20)];

  assert.equal(planRoutineReorder(routines, 'a', 'a'), null);
});

test('planRoutineReorder: a missing over id is a no-op', () => {
  const routines = [routine('a', 10), routine('b', 20)];

  assert.equal(planRoutineReorder(routines, 'a', null), null);
  assert.equal(planRoutineReorder(routines, 'a', undefined), null);
});

test('planRoutineReorder: unknown ids return null instead of throwing', () => {
  const routines = [routine('a', 10), routine('b', 20)];

  assert.equal(planRoutineReorder(routines, 'stale', 'b'), null);
  assert.equal(planRoutineReorder(routines, 'a', 'stale'), null);
});

// ── routinesInIdOrder ────────────────────────────────────────────────────

test('routinesInIdOrder: ignores unknown ids and repeats, keeps every row', () => {
  const routines = [routine('a', 10), routine('b', 20)];

  const ordered = routinesInIdOrder(routines, ['nope', 'b', 'a', 'b']);

  assert.deepEqual(
    ordered.map((row) => row.id),
    ['b', 'a'],
  );
  assert.deepEqual(
    ordered.map((row) => row.sort_order),
    [0, 1],
  );
  // The input array is untouched.
  assert.deepEqual(
    routines.map((row) => row.id),
    ['a', 'b'],
  );
});

test('routinesInIdOrder: a partial id list appends the unnamed rows', () => {
  const routines = [routine('a', 10), routine('b', 20), routine('c', 30)];

  const ordered = routinesInIdOrder(routines, ['c']);

  assert.deepEqual(
    ordered.map((row) => row.id),
    ['c', 'a', 'b'],
  );
  assert.deepEqual(
    ordered.map((row) => row.sort_order),
    [0, 1, 2],
  );
});
