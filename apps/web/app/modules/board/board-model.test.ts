import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { TaskRecord, TaskStatus } from '../../types';
import {
  applyOptimisticMove,
  defaultMoveRank,
  destIndexInRemaining,
  resolveSortOrder,
} from './board-model';

// ── Fixtures (inline — never import mock-data) ───────────────────────────

let seq = 0;

function task(
  overrides: Partial<TaskRecord> & { status: TaskStatus },
): TaskRecord {
  const id = overrides.id ?? `task-${++seq}`;
  const { status, ...rest } = overrides;
  return {
    id,
    user_id: 'u1',
    title: 'Work',
    display_title: 'Work',
    description: '',
    duration_minutes: 15,
    priority: 'medium',
    difficulty: 'easy',
    sort_order: 0,
    status,
    focused: false,
    created_at: '2026-08-20T00:00:00Z',
    updated_at: '2026-08-20T00:00:00Z',
    category: {
      id: 'cat-1',
      title: 'Work',
      slug: 'work',
      list_id: 'list-1',
      inherited_list_id: 'list-1',
      is_untracked: false,
      color: '#2a5c8a',
    },
    ...rest,
  };
}

// ── defaultMoveRank (mirrors the Rust default_move_rank) ─────────────────

test('defaultMoveRank: empty pile appends at 0', () => {
  assert.equal(defaultMoveRank('PLANNED', 'OPEN', [], 'ghost'), 0);
});

test('defaultMoveRank: two OPEN (0,1) unplan appends at 2', () => {
  const tasks = [
    task({ status: 'OPEN', sort_order: 0 }),
    task({ status: 'OPEN', sort_order: 1 }),
  ];
  assert.equal(defaultMoveRank('PLANNED', 'OPEN', tasks, 'ghost'), 2);
});

test('defaultMoveRank: OPEN→PLANNED on an empty pile is 0', () => {
  assert.equal(defaultMoveRank('OPEN', 'PLANNED', [], 'ghost'), 0);
});

test('defaultMoveRank: OPEN→PLANNED with a 0 already appends at 1', () => {
  // Other-status cards (even at huge ranks) never inflate the PLANNED max.
  const tasks = [
    task({ status: 'PLANNED', sort_order: 0 }),
    task({ status: 'OPEN', sort_order: 99 }),
    task({ status: 'COMPLETED', sort_order: 99 }),
  ];
  assert.equal(defaultMoveRank('OPEN', 'PLANNED', tasks, 'ghost'), 1);
});

test('defaultMoveRank: IN_PROGRESS→PLANNED prepends 0 even when Planned has cards', () => {
  const tasks = [
    task({ status: 'PLANNED', sort_order: 0 }),
    task({ status: 'PLANNED', sort_order: 3 }),
    task({ status: 'PLANNED', sort_order: 9 }),
  ];
  assert.equal(defaultMoveRank('IN_PROGRESS', 'PLANNED', tasks, 'ghost'), 0);
});

test('defaultMoveRank: COMPLETED / DISCARDED / IN_PROGRESS prepend 0', () => {
  const tasks = [
    task({ status: 'COMPLETED', sort_order: 5 }),
    task({ status: 'DISCARDED', sort_order: 5 }),
    task({ status: 'IN_PROGRESS', sort_order: 5 }),
  ];
  assert.equal(defaultMoveRank('OPEN', 'COMPLETED', tasks, 'ghost'), 0);
  assert.equal(defaultMoveRank('OPEN', 'DISCARDED', tasks, 'ghost'), 0);
  assert.equal(defaultMoveRank('OPEN', 'IN_PROGRESS', tasks, 'ghost'), 0);
});

test('defaultMoveRank: excludeId keeps the mover out of its own append target', () => {
  // The mover may already sit in the target pile holding a leftover rank
  // (the server state at dispatch time) — excluded, so it cannot inflate
  // the max the way a real peer would.
  const tasks = [
    task({ id: 'mover', status: 'OPEN', sort_order: 9 }),
    task({ id: 'peer', status: 'OPEN', sort_order: 1 }),
  ];
  assert.equal(defaultMoveRank('PLANNED', 'OPEN', tasks, 'mover'), 2);
  assert.equal(defaultMoveRank('PLANNED', 'OPEN', tasks, 'ghost'), 10);
});

// ── resolveSortOrder (remaining = dest column minus dragged id; destIndex is
//    a view index into the column's filtered, capped display list) ─────────

test('resolveSortOrder: empty In Progress pile drops at 0', () => {
  assert.equal(resolveSortOrder([], 0, 'IN_PROGRESS'), 0);
});

test('resolveSortOrder: In Progress drop onto the top takes the first card rank', () => {
  const tasks = [
    task({ status: 'IN_PROGRESS', sort_order: 2 }),
    task({ status: 'IN_PROGRESS', sort_order: 7 }),
  ];
  // Prepend-compatible: the first visible card's rank (not hardcoded 0).
  assert.equal(resolveSortOrder(tasks, 0, 'IN_PROGRESS'), 2);
});

test('resolveSortOrder: In Progress insert before the second card is not always 0', () => {
  const tasks = [
    task({ status: 'IN_PROGRESS', sort_order: 0 }),
    task({ status: 'IN_PROGRESS', sort_order: 1 }),
  ];
  // destIndex 1 = insert before the card at view index 1 → its rank, 1.
  assert.equal(resolveSortOrder(tasks, 1, 'IN_PROGRESS'), 1);
});

test('resolveSortOrder: In Progress drop past the end appends last rank + 1', () => {
  const tasks = [
    task({ status: 'IN_PROGRESS', sort_order: 0 }),
    task({ status: 'IN_PROGRESS', sort_order: 1 }),
  ];
  // Unranked beyond the pile → last visible rank + 1 (no 20-card cap).
  assert.equal(resolveSortOrder(tasks, 2, 'IN_PROGRESS'), 2);
  assert.equal(resolveSortOrder(tasks, 5, 'IN_PROGRESS'), 2);
});

// ── destIndexInRemaining (shown index includes the mover; `remaining`
//    excludes it, so resolveSortOrder must not read the next card) ─────────

test('destIndexInRemaining: mover not in dest column leaves the index alone', () => {
  // Cross-column drop onto a card: oldIndex is -1.
  assert.equal(destIndexInRemaining(2, -1), 2);
  assert.equal(destIndexInRemaining(0, -1), 0);
});

test('destIndexInRemaining: mover after target (move up) leaves the index alone', () => {
  // Last→first: shown index 0, oldIndex 3 — the mover sat after the target,
  // so removing it does not shift the target's index.
  assert.equal(destIndexInRemaining(0, 3), 0);
});

test('destIndexInRemaining: mover before target (move down) shifts the index down one', () => {
  // First→third: shown index 2, oldIndex 0 — removing the mover from before
  // the target shifts every later index down by one.
  assert.equal(destIndexInRemaining(2, 0), 1);
});

test('destIndexInRemaining: drop on self stays unchanged', () => {
  assert.equal(destIndexInRemaining(2, 2), 2);
});

test('destIndexInRemaining: column-append with the mover in the column returns remaining.length', () => {
  // destIndex === shown.length (4), mover in the column (oldIndex 0): the
  // insert lands at shown.length - 1, which equals remaining.length.
  assert.equal(destIndexInRemaining(4, 0), 3);
  assert.equal(destIndexInRemaining(4, 3), 3);
});

// ── applyOptimisticMove (mirrors reorder_in_place / place_at) ─────────────

// The order `tasksForColumn` renders: sort_order ASC, then created_at ASC.
function displayIds(tasks: TaskRecord[], status: TaskStatus): string[] {
  return tasks
    .filter((task) => task.status === status)
    .sort(
      (a, b) =>
        a.sort_order - b.sort_order ||
        a.created_at.localeCompare(b.created_at),
    )
    .map((task) => task.id);
}

// The confirmed repro: last of 4 dropped onto the first must paint as
// D first. Distinct created_at (D latest) so a buggy same-rank tie would
// sort D *after* A.
test('applyOptimisticMove: last→first (D to rank 0) paints D,A,B,C', () => {
  const tasks = [
    task({ id: 'A', status: 'OPEN', sort_order: 0, created_at: '2026-08-20T00:00:00Z' }),
    task({ id: 'B', status: 'OPEN', sort_order: 1, created_at: '2026-08-20T01:00:00Z' }),
    task({ id: 'C', status: 'OPEN', sort_order: 2, created_at: '2026-08-20T02:00:00Z' }),
    task({ id: 'D', status: 'OPEN', sort_order: 3, created_at: '2026-08-20T03:00:00Z' }),
  ];
  const result = applyOptimisticMove(tasks, 'D', 'OPEN', 0);
  assert.deepEqual(
    displayIds(result, 'OPEN'),
    ['D', 'A', 'B', 'C'],
    'peers in [0, 3) shift up one so D sorts first',
  );
  assert.deepEqual(
    result.map((t) => [t.id, t.sort_order]),
    [
      ['A', 1],
      ['B', 2],
      ['C', 3],
      ['D', 0],
    ],
  );
});

test('applyOptimisticMove: first→third (A from 0 to rank 2) shifts peers down one', () => {
  // Mirrors move_same_column_reorder_shifts_neighbors: peers in (0, 2] shift
  // down one: B→0, C→1, A→2.
  const tasks = [
    task({ id: 'A', status: 'OPEN', sort_order: 0 }),
    task({ id: 'B', status: 'OPEN', sort_order: 1 }),
    task({ id: 'C', status: 'OPEN', sort_order: 2 }),
    task({ id: 'D', status: 'OPEN', sort_order: 3 }),
  ];
  const result = applyOptimisticMove(tasks, 'A', 'OPEN', 2);
  assert.deepEqual(
    result.map((t) => [t.id, t.sort_order]),
    [
      ['A', 2],
      ['B', 0],
      ['C', 1],
      ['D', 3],
    ],
  );
  assert.deepEqual(displayIds(result, 'OPEN'), ['B', 'C', 'A', 'D']);
});

test('applyOptimisticMove: first→last appends at last rank + 1 and shifts the rest', () => {
  // A (0) to rank 4 (append): peers in (0, 4] shift down one → B,C,D at
  // 0,1,2 and A at 4.
  const tasks = [
    task({ id: 'A', status: 'OPEN', sort_order: 0 }),
    task({ id: 'B', status: 'OPEN', sort_order: 1 }),
    task({ id: 'C', status: 'OPEN', sort_order: 2 }),
    task({ id: 'D', status: 'OPEN', sort_order: 3 }),
  ];
  const result = applyOptimisticMove(tasks, 'A', 'OPEN', 4);
  assert.deepEqual(
    result.map((t) => [t.id, t.sort_order]),
    [
      ['A', 4],
      ['B', 0],
      ['C', 1],
      ['D', 2],
    ],
  );
  assert.deepEqual(displayIds(result, 'OPEN'), ['B', 'C', 'D', 'A']);
});

test('applyOptimisticMove: same rank is a no-op for siblings', () => {
  const tasks = [
    task({ id: 'A', status: 'OPEN', sort_order: 0 }),
    task({ id: 'B', status: 'OPEN', sort_order: 1 }),
  ];
  const result = applyOptimisticMove(tasks, 'A', 'OPEN', 0);
  assert.deepEqual(result, tasks);
});

test('applyOptimisticMove: cross-column shifts dest peers at >= insert, source is not compacted', () => {
  const tasks = [
    task({ id: 'A', status: 'OPEN', sort_order: 0 }),
    task({ id: 'B', status: 'OPEN', sort_order: 1 }),
    task({ id: 'P', status: 'PLANNED', sort_order: 0 }),
  ];
  const result = applyOptimisticMove(tasks, 'A', 'PLANNED', 0);
  assert.deepEqual(
    result.map((t) => [t.id, t.status, t.sort_order]),
    [
      ['A', 'PLANNED', 0],
      ['B', 'OPEN', 1], // source column untouched (server does not compact)
      ['P', 'PLANNED', 1], // dest peer at >= 0 shifts up so A is really first
    ],
  );
  assert.deepEqual(displayIds(result, 'PLANNED'), ['A', 'P']);
  assert.deepEqual(displayIds(result, 'OPEN'), ['B']);
});

test('applyOptimisticMove: unknown id returns the array unchanged', () => {
  const tasks = [
    task({ id: 'A', status: 'OPEN', sort_order: 0 }),
    task({ id: 'B', status: 'OPEN', sort_order: 1 }),
  ];
  assert.equal(applyOptimisticMove(tasks, 'ghost', 'OPEN', 0), tasks);
});
