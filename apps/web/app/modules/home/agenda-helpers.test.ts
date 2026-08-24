// Unit tests for the Home agenda's pure helpers (date label, add-task picker
// filter, reorder rank math, living / Completed dump split). The reorder
// tests pin the exact server semantics of `POST /api/agenda/items/:id/move`:
// peers at/after the target rank shift up one and the item lands on it.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import type {
  AgendaItemRecord,
  OccurrenceRecord,
  TaskRecord,
} from '../../types';
import {
  agendaDateLabel,
  agendaMoveTarget,
  agendaMoveTargetAt,
  agendaTaskMatches,
  applyAgendaMove,
  canReschedule,
  filterAgendaPickerTasks,
  isAgendaItemParked,
  partitionAgendaItems,
} from './agenda-helpers';

// ── Fixtures ────────────────────────────────────────────────────────────

function task(
  id: string,
  status: TaskRecord['status'],
  title = `Task ${id}`,
  updatedAt = '2026-08-23T00:00:00Z',
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
    updated_at: updatedAt,
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

/** A task-kind agenda row with its task embed. */
function taskItem(
  id: string,
  status: TaskRecord['status'],
  updatedAt?: string,
): AgendaItemRecord {
  return { ...item(id, 0), task: task(id, status, `Task ${id}`, updatedAt) };
}

/** An occurrence-kind agenda row with its occurrence embed. */
function occurrenceItem(
  id: string,
  status: OccurrenceRecord['status'],
  updatedAt = '2026-08-23T00:00:00Z',
): AgendaItemRecord {
  return {
    ...item(id, 0),
    kind: 'occurrence',
    ref_id: `occ-${id}`,
    occurrence: {
      id: `occ-${id}`,
      routine_id: 'r-1',
      user_id: 'u-1',
      local_date: '2026-08-23',
      title: null,
      resolved_title: 'Morning run',
      status,
      estimated_minutes: 30,
      rrule: 'DTSTART:20260823T070000\nRRULE:FREQ=DAILY',
      calendar_id: null,
      google_event_id: null,
      created_at: '2026-08-23T00:00:00Z',
      updated_at: updatedAt,
      category: {
        id: 'cat-1',
        title: 'Work',
        slug: 'work',
        list_id: 'list-1',
        inherited_list_id: null,
        is_untracked: false,
        color: '#2a5c8a',
      },
    },
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

// ── agendaMoveTargetAt ───────────────────────────────────────────────────

test('agendaMoveTargetAt: dropped above takes the target rank', () => {
  const items = [item('a', 0), item('b', 1), item('c', 2)];
  // b (index 1) onto a's slot (index 0) → a's rank.
  assert.equal(agendaMoveTargetAt(items, 1, 0), items[0].sort_order);
  // c (index 2) onto b's slot (index 1) → b's rank.
  assert.equal(agendaMoveTargetAt(items, 2, 1), items[1].sort_order);
});

test('agendaMoveTargetAt: dropped below lands just after the target row', () => {
  const items = [item('a', 0), item('b', 1), item('c', 2)];
  // a (index 0) onto b's slot (index 1) → lands after b.
  assert.equal(agendaMoveTargetAt(items, 0, 1), items[1].sort_order + 1);
  assert.equal(agendaMoveTargetAt(items, 0, 2), items[2].sort_order + 1);
});

test('agendaMoveTargetAt: same index and out-of-range return null', () => {
  const items = [item('a', 0), item('b', 1)];
  assert.equal(agendaMoveTargetAt(items, 0, 0), null);
  assert.equal(agendaMoveTargetAt(items, 1, 1), null);
  assert.equal(agendaMoveTargetAt(items, -1, 0), null);
  assert.equal(agendaMoveTargetAt(items, 0, -1), null);
  assert.equal(agendaMoveTargetAt(items, 2, 0), null);
  assert.equal(agendaMoveTargetAt(items, 0, 2), null);
  assert.equal(agendaMoveTargetAt([], 0, 0), null);
});

test('agendaMoveTargetAt: rank gaps keep working (after an unpin)', () => {
  // Server keeps ranks after a hard delete, so a gap is legal — the target
  // reads the stored rank, not the pile position.
  const items = [item('a', 0), item('c', 2)];
  assert.equal(agendaMoveTargetAt(items, 1, 0), 0);
  assert.equal(agendaMoveTargetAt(items, 0, 1), 3);
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

// ── canReschedule ────────────────────────────────────────────────────────

test('canReschedule: occurrences move only while pending or skipped', () => {
  assert.equal(canReschedule(occurrenceItem('a', 'pending')), true);
  assert.equal(canReschedule(occurrenceItem('b', 'skipped')), true);
  // The API 400s these (in_progress / done) — the control hides with the
  // chip instead of inviting a failure banner.
  assert.equal(canReschedule(occurrenceItem('c', 'in_progress')), false);
  assert.equal(canReschedule(occurrenceItem('d', 'done')), false);
});

test('canReschedule: tasks move while living; completed/discarded stay put', () => {
  assert.equal(canReschedule(taskItem('a', 'OPEN')), true);
  assert.equal(canReschedule(taskItem('b', 'PLANNED')), true);
  assert.equal(canReschedule(taskItem('c', 'IN_PROGRESS')), true);
  // The API would allow these, but Home never invites moving a finished card.
  assert.equal(canReschedule(taskItem('d', 'COMPLETED')), false);
  assert.equal(canReschedule(taskItem('e', 'DISCARDED')), false);
});

test('canReschedule: an embedless row (orphan) is never reschedulable', () => {
  assert.equal(canReschedule(item('orphan', 0)), false);
});

// ── isAgendaItemParked ──────────────────────────────────────────────────

test('isAgendaItemParked: terminal tasks park, living tasks stay', () => {
  assert.equal(isAgendaItemParked(taskItem('a', 'COMPLETED')), true);
  assert.equal(isAgendaItemParked(taskItem('b', 'DISCARDED')), true);
  assert.equal(isAgendaItemParked(taskItem('c', 'OPEN')), false);
  assert.equal(isAgendaItemParked(taskItem('d', 'PLANNED')), false);
  assert.equal(isAgendaItemParked(taskItem('e', 'IN_PROGRESS')), false);
});

test('isAgendaItemParked: only done occurrences park', () => {
  assert.equal(isAgendaItemParked(occurrenceItem('a', 'done')), true);
  // skipped is a decline, not a finish — it stays living.
  assert.equal(isAgendaItemParked(occurrenceItem('b', 'pending')), false);
  assert.equal(isAgendaItemParked(occurrenceItem('c', 'skipped')), false);
  assert.equal(isAgendaItemParked(occurrenceItem('d', 'in_progress')), false);
});

test('isAgendaItemParked: orphans never park', () => {
  assert.equal(isAgendaItemParked(item('orphan', 0)), false);
});

// ── partitionAgendaItems ────────────────────────────────────────────────

test('partitionAgendaItems: splits a mixed pile; living keeps sort_order', () => {
  const pile = [
    { ...taskItem('t-done', 'COMPLETED', '2026-08-23T09:00:00Z'), sort_order: 1 },
    { ...occurrenceItem('o-done', 'done', '2026-08-23T08:00:00Z'), sort_order: 2 },
    { ...occurrenceItem('o-skipped', 'skipped'), sort_order: 3 },
    { ...occurrenceItem('o-pending', 'pending'), sort_order: 4 },
    { ...occurrenceItem('o-progress', 'in_progress'), sort_order: 5 },
    { ...taskItem('t-open', 'OPEN'), sort_order: 6 },
    { ...taskItem('t-drop', 'DISCARDED', '2026-08-23T07:00:00Z'), sort_order: 7 },
    { ...item('orphan', 8) },
  ];
  const { living, completed } = partitionAgendaItems(pile);
  // Living: skipped + pending + in_progress + living task + orphan, ranked.
  assert.deepEqual(
    living.map((e) => e.id),
    ['o-skipped', 'o-pending', 'o-progress', 't-open', 'orphan'],
  );
  // Completed: done occurrence + COMPLETED + DISCARDED, newest updated_at
  // first (t-done 09:00 → o-done 08:00 → t-drop 07:00).
  assert.deepEqual(
    completed.map((e) => e.id),
    ['t-done', 'o-done', 't-drop'],
  );
});

test('partitionAgendaItems: completed sorts newest embed updated_at first', () => {
  const pile = [
    occurrenceItem('old', 'done', '2026-08-20T00:00:00Z'),
    taskItem('mid', 'COMPLETED', '2026-08-22T00:00:00Z'),
    taskItem('new', 'COMPLETED', '2026-08-23T00:00:00Z'),
  ];
  const { completed } = partitionAgendaItems(pile);
  assert.deepEqual(
    completed.map((e) => e.id),
    ['new', 'mid', 'old'],
  );
});

test('partitionAgendaItems: equal updated_at tie-breaks by id ascending', () => {
  const pile = [
    taskItem('b', 'COMPLETED'),
    occurrenceItem('a', 'done'),
    taskItem('c', 'DISCARDED'),
  ];
  const { completed } = partitionAgendaItems(pile);
  assert.deepEqual(
    completed.map((e) => e.id),
    ['a', 'b', 'c'],
  );
});

test('partitionAgendaItems: missing/invalid updated_at sorts last (epoch 0)', () => {
  const pile = [
    taskItem('bad', 'COMPLETED', 'not-a-date'),
    taskItem('empty', 'DISCARDED', ''),
    taskItem('fresh', 'COMPLETED', '2026-08-23T00:00:00Z'),
  ];
  const { completed } = partitionAgendaItems(pile);
  // fresh has a real timestamp; bad and empty both read as epoch 0 and
  // tie-break by id ascending ('bad' before 'empty').
  assert.deepEqual(
    completed.map((e) => e.id),
    ['fresh', 'bad', 'empty'],
  );
});

test('partitionAgendaItems: does not mutate the input array', () => {
  const pile = [
    taskItem('a', 'COMPLETED'),
    occurrenceItem('b', 'done'),
    occurrenceItem('c', 'pending'),
    taskItem('d', 'OPEN'),
    item('e', 2),
  ];
  const before = [...pile];
  partitionAgendaItems(pile);
  // Same references, same order, unchanged after the call.
  assert.equal(pile.length, before.length);
  before.forEach((entry, i) => assert.equal(pile[i], entry));
});

test('partitionAgendaItems: empty input yields two empty piles', () => {
  const { living, completed } = partitionAgendaItems([]);
  assert.deepEqual(living, []);
  assert.deepEqual(completed, []);
});
