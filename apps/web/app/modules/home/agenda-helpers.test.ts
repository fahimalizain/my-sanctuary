// Unit tests for the Home agenda's pure helpers (date label, add-task picker
// filter, reorder rank math). The reorder tests pin the exact server
// semantics of `POST /api/agenda/items/:id/move`: peers at/after the target
// rank shift up one and the item lands on it.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { AgendaItemRecord, TaskRecord } from '../../types';
import {
  agendaDateLabel,
  agendaMoveTarget,
  agendaTaskMatches,
  applyAgendaMove,
  filterAgendaPickerTasks,
} from './agenda-helpers';

// ── Fixtures ────────────────────────────────────────────────────────────

function task(
  id: string,
  status: TaskRecord['status'],
  title = `Task ${id}`,
): TaskRecord {
  return {
    id,
    user_id: 'u-1',
    title,
    display_title: title,
    description: '',
    duration_minutes: 15,
    priority: 'medium',
    difficulty: 'easy',
    sort_order: 0,
    status,
    created_at: '2026-08-23T00:00:00Z',
    updated_at: '2026-08-23T00:00:00Z',
    focused: false,
    category: {
      id: 'cat-1',
      title: 'Work',
      slug: 'work',
      list_id: 'list-1',
      inherited_list_id: null,
      is_untracked: false,
      color: '#2a5c8a',
    },
  };
}

function item(id: string, sortOrder: number): AgendaItemRecord {
  return {
    id,
    user_id: 'u-1',
    local_date: '2026-08-23',
    kind: 'task',
    ref_id: `task-${id}`,
    sort_order: sortOrder,
    task: null,
    occurrence: null,
  };
}

// ── agendaDateLabel ─────────────────────────────────────────────────────

test('agendaDateLabel: today/tomorrow/yesterday relative to today', () => {
  const today = '2026-08-23';
  assert.equal(agendaDateLabel('2026-08-23', today), 'Today');
  assert.equal(agendaDateLabel('2026-08-24', today), 'Tomorrow');
  assert.equal(agendaDateLabel('2026-08-22', today), 'Yesterday');
});

test('agendaDateLabel: relative labels survive month boundaries', () => {
  const today = '2026-08-31';
  assert.equal(agendaDateLabel('2026-09-01', today), 'Tomorrow');
  assert.equal(agendaDateLabel('2026-08-30', today), 'Yesterday');
  // Year boundary too.
  assert.equal(agendaDateLabel('2026-01-01', '2025-12-31'), 'Tomorrow');
});

test('agendaDateLabel: other dates render as "Tue 25 Aug"', () => {
  assert.equal(agendaDateLabel('2026-08-25', '2026-08-23'), 'Tue 25 Aug');
  assert.equal(agendaDateLabel('2026-12-01', '2026-08-23'), 'Tue 1 Dec');
  assert.equal(agendaDateLabel('2027-01-01', '2026-08-23'), 'Fri 1 Jan');
});

test('agendaDateLabel: malformed input returns it unchanged', () => {
  assert.equal(agendaDateLabel('', '2026-08-23'), '');
  assert.equal(agendaDateLabel('not-a-date', '2026-08-23'), 'not-a-date');
  assert.equal(agendaDateLabel('2026-13-40', '2026-08-23'), '2026-13-40');
});

// ── filterAgendaPickerTasks ─────────────────────────────────────────────

test('filterAgendaPickerTasks: keeps living statuses only', () => {
  const tasks = [
    task('a', 'OPEN'),
    task('b', 'PLANNED'),
    task('c', 'IN_PROGRESS'),
    task('d', 'COMPLETED'),
    task('e', 'DISCARDED'),
  ];
  const ids = filterAgendaPickerTasks(tasks, new Set()).map((t) => t.id);
  assert.deepEqual(ids, ['a', 'b', 'c']);
});

test('filterAgendaPickerTasks: excludes tasks already on the date', () => {
  const tasks = [task('a', 'OPEN'), task('b', 'PLANNED')];
  const ids = filterAgendaPickerTasks(tasks, new Set(['a'])).map((t) => t.id);
  assert.deepEqual(ids, ['b']);
});

test('filterAgendaPickerTasks: empty pools', () => {
  assert.deepEqual(filterAgendaPickerTasks([], new Set()), []);
  assert.deepEqual(
    filterAgendaPickerTasks([task('a', 'COMPLETED')], new Set()),
    [],
  );
});

// ── agendaTaskMatches ────────────────────────────────────────────────────

test('agendaTaskMatches: blank query matches everything', () => {
  assert.equal(agendaTaskMatches(task('a', 'OPEN', 'Review Q3'), ''), true);
  assert.equal(agendaTaskMatches(task('a', 'OPEN', 'Review Q3'), '   '), true);
});

test('agendaTaskMatches: case-insensitive substring over title', () => {
  assert.equal(
    agendaTaskMatches(task('a', 'OPEN', 'Review Q3'), 'review'),
    true,
  );
  assert.equal(agendaTaskMatches(task('a', 'OPEN', 'Review Q3'), 'q3'), true);
  assert.equal(agendaTaskMatches(task('a', 'OPEN', 'Review Q3'), 'q4'), false);
});

test('agendaTaskMatches: matches the display title (hole) too', () => {
  const t = task('a', 'OPEN', 'Work | SpicyHome');
  t.display_title = 'SpicyHome';
  assert.equal(agendaTaskMatches(t, 'spicy'), true);
  assert.equal(agendaTaskMatches(t, 'work'), true);
});

// ── agendaMoveTarget ─────────────────────────────────────────────────────

test('agendaMoveTarget: up takes the above rank, down lands after the below', () => {
  const items = [item('a', 0), item('b', 1), item('c', 2)];
  assert.equal(agendaMoveTarget(items, 'b', 'up'), 0);
  assert.equal(agendaMoveTarget(items, 'b', 'down'), 3); // 2 + 1
});

test('agendaMoveTarget: edges return null', () => {
  const items = [item('a', 0), item('b', 1)];
  assert.equal(agendaMoveTarget(items, 'a', 'up'), null);
  assert.equal(agendaMoveTarget(items, 'b', 'down'), null);
  assert.equal(agendaMoveTarget(items, 'missing', 'up'), null);
  assert.equal(agendaMoveTarget([], 'a', 'up'), null);
});

test('agendaMoveTarget: rank gaps keep working (after an unpin)', () => {
  // Server keeps ranks after a hard delete, so a gap is legal.
  const items = [item('a', 0), item('c', 2)];
  assert.equal(agendaMoveTarget(items, 'c', 'up'), 0);
  assert.equal(agendaMoveTarget(items, 'a', 'down'), 3);
});

// ── applyAgendaMove ──────────────────────────────────────────────────────

test('applyAgendaMove: down shifts peers at/after the target up (server mirror)', () => {
  const items = [item('a', 0), item('b', 1), item('c', 2)];
  const moved = applyAgendaMove(items, 'a', 2);
  assert.deepEqual(
    moved.map((entry) => [entry.id, entry.sort_order]),
    [
      ['b', 1],
      ['a', 2],
      ['c', 3],
    ],
  );
});

test('applyAgendaMove: up shifts peers at/after the target up', () => {
  const items = [item('a', 0), item('b', 1), item('c', 2)];
  const moved = applyAgendaMove(items, 'c', 0);
  assert.deepEqual(
    moved.map((entry) => [entry.id, entry.sort_order]),
    [
      ['c', 0],
      ['a', 1],
      ['b', 2],
    ],
  );
});

test('applyAgendaMove: no-op when already at the target rank', () => {
  const items = [item('a', 0), item('b', 1)];
  assert.equal(applyAgendaMove(items, 'a', 0), items);
});

test('applyAgendaMove: missing item returns the input unchanged', () => {
  const items = [item('a', 0)];
  assert.equal(applyAgendaMove(items, 'missing', 1), items);
});

test('applyAgendaMove: consecutive moves keep ranks server-exact', () => {
  // A down, then A down again — the second target is computed from the
  // mirrored state and must match what the server would produce (peers keep
  // their shifted ranks; only the ORDER of the pile is meaningful).
  let items = [item('a', 0), item('b', 1), item('c', 2), item('d', 3)];
  items = applyAgendaMove(items, 'a', agendaMoveTarget(items, 'a', 'down')!);
  assert.deepEqual(
    items.map((entry) => [entry.id, entry.sort_order]),
    [
      ['b', 1],
      ['a', 2],
      ['c', 3],
      ['d', 4],
    ],
  );
  items = applyAgendaMove(items, 'a', agendaMoveTarget(items, 'a', 'down')!);
  assert.deepEqual(
    items.map((entry) => [entry.id, entry.sort_order]),
    [
      ['b', 1],
      ['c', 3],
      ['a', 4],
      ['d', 5],
    ],
  );
  // And back up twice: A bubbles to the front, ranks keep shifting up.
  items = applyAgendaMove(items, 'a', agendaMoveTarget(items, 'a', 'up')!);
  assert.deepEqual(
    items.map((entry) => [entry.id, entry.sort_order]),
    [
      ['b', 1],
      ['a', 3],
      ['c', 4],
      ['d', 6],
    ],
  );
  items = applyAgendaMove(items, 'a', agendaMoveTarget(items, 'a', 'up')!);
  assert.deepEqual(
    items.map((entry) => [entry.id, entry.sort_order]),
    [
      ['a', 1],
      ['b', 2],
      ['c', 5],
      ['d', 7],
    ],
  );
});
