import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { TaskRecord, TaskStatus } from '../../types';
import { defaultMoveRank } from './board-model';

// ── Fixtures (inline — never import mock-data) ───────────────────────────

let seq = 0;

function task(overrides: Partial<TaskRecord> & { status: TaskStatus }): TaskRecord {
  const id = overrides.id ?? `task-${++seq}`;
  const { status, ...rest } = overrides;
  return {
    id,
    user_id: 'u1',
    title: 'Work',
    description: '',
    duration_minutes: 15,
    priority: 'medium',
    difficulty: 'easy',
    sort_order: 0,
    status,
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
