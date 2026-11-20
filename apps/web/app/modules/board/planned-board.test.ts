import { test } from 'node:test';
import assert from 'node:assert/strict';
import type {
  AgendaItemRecord,
  PlannedBoardRow,
  TaskRecord,
  TaskStatus,
} from '../../types';
import {
  PLANNED_GROUP_PREFIX,
  PLANNED_UNDATED_GROUP_KEY,
  applyPlannedAgendaMove,
  applyPlannedBoardReorder,
  buildPlannedGroups,
  findPlannedRow,
  markPlannedRowUndated,
  movePlannedRowToDate,
  plannedAgendaRankForDrop,
  plannedBoardRankForDrop,
  plannedGroupId,
  plannedGroupKeyFromId,
  plannedGroupOfTask,
  plannedRowFromItem,
  plannedRowsByTaskId,
  removePlannedTask,
  resolvePlannedDrop,
  upsertPlannedRow,
  visiblePlannedPreviewIndex,
  visiblePlannedTaskIds,
} from './planned-board';
import type { PlannedBoardGroup, PlannedDropTarget } from './planned-board';

// ── Fixtures (inline — never import mock-data) ───────────────────────────

let seq = 0;

function task(
  overrides: Partial<TaskRecord> & { status?: TaskStatus } = {},
): TaskRecord {
  const id = overrides.id ?? `task-${++seq}`;
  const { status = 'PLANNED', ...rest } = overrides;
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

function row(
  taskId: string,
  overrides: Partial<PlannedBoardRow> = {},
): PlannedBoardRow {
  return {
    task_id: taskId,
    item_id: `item-${taskId}`,
    local_date: '2026-11-21',
    agenda_sort_order: 0,
    board_sort_order: 0,
    undated: false,
    ...overrides,
  };
}

function taskMap(tasks: TaskRecord[]): Map<string, TaskRecord> {
  return new Map(tasks.map((entry) => [entry.id, entry]));
}

function agendaItem(
  overrides: Partial<AgendaItemRecord> & { ref_id: string },
): AgendaItemRecord {
  return {
    id: 'item-1',
    user_id: 'u1',
    local_date: '2026-11-21',
    kind: 'task',
    sort_order: 0,
    task: null,
    occurrence: null,
    ...overrides,
  };
}

// ── Group ids ────────────────────────────────────────────────────────────

test('plannedGroupId / plannedGroupKeyFromId round-trip a date and Undated', () => {
  assert.equal(PLANNED_GROUP_PREFIX, 'planned-group:');
  assert.equal(PLANNED_UNDATED_GROUP_KEY, 'undated');
  assert.equal(plannedGroupId('2026-11-21'), 'planned-group:2026-11-21');
  assert.equal(plannedGroupId('undated'), 'planned-group:undated');
  assert.equal(plannedGroupKeyFromId('planned-group:2026-11-21'), '2026-11-21');
  assert.equal(plannedGroupKeyFromId('planned-group:undated'), 'undated');
});

test('plannedGroupKeyFromId: non-group ids and an empty key return null', () => {
  assert.equal(plannedGroupKeyFromId('column:PLANNED'), null);
  assert.equal(plannedGroupKeyFromId('2026-11-21'), null);
  assert.equal(plannedGroupKeyFromId(PLANNED_GROUP_PREFIX), null);
});

// ── buildPlannedGroups: dated groups ─────────────────────────────────────

test('buildPlannedGroups: dated groups sort by date, members by agenda rank then id', () => {
  const tasks = [
    task({ id: 'b-task' }),
    task({ id: 'a-task' }),
    task({ id: 'c-task' }),
    task({ id: 'later' }),
    task({ id: 'open', status: 'OPEN' }),
    task({ id: 'done', status: 'COMPLETED' }),
    task({ id: 'undated' }),
  ];
  const rows = [
    row('b-task', { agenda_sort_order: 1 }),
    row('a-task', { agenda_sort_order: 1 }),
    row('c-task', { agenda_sort_order: 0 }),
    row('later', { local_date: '2026-11-25', agenda_sort_order: 0 }),
    row('open', { local_date: '2026-11-20' }),
    row('done', { local_date: '2026-11-19' }),
  ];

  const groups = buildPlannedGroups(tasks, rows, () => true);

  assert.deepEqual(
    groups.map((group) => group.key),
    ['2026-11-21', '2026-11-25', PLANNED_UNDATED_GROUP_KEY],
  );
  assert.deepEqual(groups[0], {
    id: 'planned-group:2026-11-21',
    key: '2026-11-21',
    date: '2026-11-21',
    undated: false,
    taskIds: ['c-task', 'a-task', 'b-task'],
  });
  assert.deepEqual(groups[1].taskIds, ['later']);
  // Only living PLANNED tasks are grouped: OPEN / COMPLETED and their dates
  // never surface.
  assert.equal(
    groups.some((group) => group.key === '2026-11-20'),
    false,
  );
  assert.equal(
    groups.some((group) => group.key === '2026-11-19'),
    false,
  );
});

// ── buildPlannedGroups: Undated fallback and Board order ──────────────────

test('buildPlannedGroups: rowless / undated / null-date rows fall into Undated last, in task order', () => {
  const tasks = [
    task({ id: 'rowless', sort_order: 1 }),
    task({
      id: 'horizon',
      sort_order: 4,
      created_at: '2026-08-20T02:00:00Z',
    }),
    task({
      id: 'sentinel',
      sort_order: 4,
      created_at: '2026-08-20T01:00:00Z',
    }),
    task({
      id: 'nulldate',
      sort_order: 9,
      created_at: '2026-08-20T00:00:00Z',
    }),
  ];
  const rows = [
    row('horizon', {
      local_date: '2076-11-20',
      undated: true,
      board_sort_order: 4,
    }),
    row('sentinel', {
      local_date: '2076-11-20',
      undated: true,
      board_sort_order: 4,
    }),
    // A stale/low row rank must NOT reorder the card: display follows the
    // task's own `sort_order` (9 here), exactly like the Undated reorder math.
    row('nulldate', {
      item_id: null,
      local_date: null,
      agenda_sort_order: null,
      board_sort_order: 0,
      undated: false,
    }),
  ];

  const groups = buildPlannedGroups(tasks, rows, () => true);

  assert.equal(groups.length, 1);
  assert.deepEqual(groups[0], {
    id: 'planned-group:undated',
    key: PLANNED_UNDATED_GROUP_KEY,
    date: null,
    undated: true,
    taskIds: ['rowless', 'sentinel', 'horizon', 'nulldate'],
  });
});

// ── buildPlannedGroups: filters ──────────────────────────────────────────

test('buildPlannedGroups: a filter hides its tasks and any emptied dated group, but Undated stays', () => {
  const tasks = [
    task({ id: 'keep' }),
    task({ id: 'hidden-dated' }),
    task({ id: 'hidden-undated' }),
  ];
  const rows = [
    row('keep', { local_date: '2026-11-21' }),
    row('hidden-dated', { local_date: '2026-11-22' }),
  ];
  const isVisible = (entry: TaskRecord) => !entry.id.startsWith('hidden');

  const groups = buildPlannedGroups(tasks, rows, isVisible);

  assert.deepEqual(
    groups.map((group) => group.key),
    ['2026-11-21', PLANNED_UNDATED_GROUP_KEY],
  );
  assert.deepEqual(groups[0].taskIds, ['keep']);
  // The Undated target survives an empty visible pile; a dated group never
  // does.
  assert.deepEqual(groups[1].taskIds, []);
  assert.equal(groups[1].undated, true);
  assert.deepEqual(
    buildPlannedGroups(tasks, rows, () => false),
    [
      {
        id: 'planned-group:undated',
        key: PLANNED_UNDATED_GROUP_KEY,
        date: null,
        undated: true,
        taskIds: [],
      },
    ],
  );
});

// ── Row lookup ───────────────────────────────────────────────────────────

test('findPlannedRow / plannedRowsByTaskId find the row and keep the first duplicate', () => {
  const rows = [
    row('a', { item_id: 'first' }),
    row('a', { item_id: 'second' }),
    row('b'),
  ];

  assert.equal(findPlannedRow(rows, 'b')?.task_id, 'b');
  assert.equal(findPlannedRow(rows, 'ghost'), undefined);
  assert.equal(plannedRowsByTaskId(rows).get('a')?.item_id, 'first');
  assert.equal(plannedRowsByTaskId(rows).get('ghost'), undefined);
});

test('plannedGroupOfTask returns the owning group or null', () => {
  const tasks = [task({ id: 'a' }), task({ id: 'u' })];
  const rows = [
    row('a', { local_date: '2026-11-21' }),
    row('u', { local_date: '2076-11-20', undated: true }),
  ];
  const groups = buildPlannedGroups(tasks, rows, () => true);

  assert.equal(plannedGroupOfTask(groups, 'a')?.key, '2026-11-21');
  assert.equal(plannedGroupOfTask(groups, 'u')?.key, 'undated');
  assert.equal(plannedGroupOfTask(groups, 'ghost'), null);
});

// ── Hiding the lifted card inside its committed group ────────────────────

test('visiblePlannedTaskIds: hides only the lifted card and keeps the group', () => {
  const group = datedGroup(['a', 'b', 'c']);

  assert.deepEqual(visiblePlannedTaskIds(group, 'b'), ['a', 'c']);
  // A single-card group keeps its identity (and therefore its droppable body)
  // while its only card is lifted.
  assert.deepEqual(visiblePlannedTaskIds(datedGroup(['only']), 'only'), []);
  // Absent / null keeps the same array reference.
  assert.equal(visiblePlannedTaskIds(group, 'ghost'), group.taskIds);
  assert.equal(visiblePlannedTaskIds(group, null), group.taskIds);
});

test('visiblePlannedPreviewIndex: the committed hover slot is already visible-relative', () => {
  const group = datedGroup(['a', 'b', 'c']);

  // Hovering a (its own slot), b, c or the body. Clamped to the two visible
  // cards still present after a is hidden.
  assert.equal(visiblePlannedPreviewIndex(group, 'a', 0), 0);
  assert.equal(visiblePlannedPreviewIndex(group, 'a', 1), 1);
  assert.equal(visiblePlannedPreviewIndex(group, 'a', 2), 2);
  assert.equal(visiblePlannedPreviewIndex(group, 'a', 3), 2);
  // Downward [A, B]: hovering B keeps index 1, which is AFTER B once A is
  // hidden — matching the rank math's `B.rank + 1`.
  assert.equal(visiblePlannedPreviewIndex(datedGroup(['A', 'B']), 'A', 1), 1);
  // Upward [A, B]: hovering A keeps index 0, before A.
  assert.equal(visiblePlannedPreviewIndex(datedGroup(['A', 'B']), 'B', 0), 0);
  // Hovering another card in a multi-card group keeps its committed index.
  assert.equal(visiblePlannedPreviewIndex(group, 'b', 2), 2);
  // Absent / null hidden ids leave the index alone.
  assert.equal(visiblePlannedPreviewIndex(group, 'ghost', 2), 2);
  assert.equal(visiblePlannedPreviewIndex(group, null, 2), 2);
  assert.equal(visiblePlannedPreviewIndex(group, 'a', null), null);
  // Clamped to the visible pile (a single hidden card leaves two).
  assert.equal(visiblePlannedPreviewIndex(group, 'a', 9), 2);
  assert.equal(visiblePlannedPreviewIndex(datedGroup(['only']), 'only', 1), 0);
});

// ── Dated rank math (mirrors agendaMoveTargetAt) ─────────────────────────

function datedGroup(taskIds: string[]): PlannedBoardGroup {
  return {
    id: plannedGroupId('2026-11-21'),
    key: '2026-11-21',
    date: '2026-11-21',
    undated: false,
    taskIds,
  };
}

test('plannedAgendaRankForDrop: below the target takes rank + 1', () => {
  const group = datedGroup(['a', 'b', 'c']);
  const rows = plannedRowsByTaskId([
    row('a', { agenda_sort_order: 0 }),
    row('b', { agenda_sort_order: 2 }),
    row('c', { agenda_sort_order: 5 }),
  ]);

  assert.equal(plannedAgendaRankForDrop(group, rows, 'c', 'a'), 6);
  assert.equal(plannedAgendaRankForDrop(group, rows, 'b', 'a'), 3);
});

test('plannedAgendaRankForDrop: above the target or from another group takes its rank', () => {
  const group = datedGroup(['a', 'b', 'c']);
  const rows = plannedRowsByTaskId([
    row('a', { agenda_sort_order: 0 }),
    row('b', { agenda_sort_order: 2 }),
    row('c', { agenda_sort_order: 5 }),
  ]);

  assert.equal(plannedAgendaRankForDrop(group, rows, 'a', 'c'), 0);
  assert.equal(plannedAgendaRankForDrop(group, rows, 'b', 'c'), 2);
  assert.equal(plannedAgendaRankForDrop(group, rows, 'b', 'outsider'), 2);
});

test('plannedAgendaRankForDrop: body / self / missing rank / Undated return undefined', () => {
  const group = datedGroup(['a', 'b']);
  const rows = plannedRowsByTaskId([row('a', { agenda_sort_order: 0 })]);

  assert.equal(plannedAgendaRankForDrop(group, rows, null, 'a'), undefined);
  assert.equal(plannedAgendaRankForDrop(group, rows, 'a', 'a'), undefined);
  assert.equal(plannedAgendaRankForDrop(group, rows, 'b', 'a'), undefined);
  assert.equal(
    plannedAgendaRankForDrop(
      { ...group, undated: true, date: null },
      rows,
      'a',
      'b',
    ),
    undefined,
  );
});

// ── Undated rank math (mirrors resolveSortOrder) ─────────────────────────

test('plannedBoardRankForDrop: the hovered card rank in both directions', () => {
  const tasks = [
    task({ id: 'a', sort_order: 0 }),
    task({ id: 'b', sort_order: 2 }),
    task({ id: 'c', sort_order: 5 }),
  ];

  assert.equal(plannedBoardRankForDrop(tasks, 'b', 'c'), 2);
  assert.equal(plannedBoardRankForDrop(tasks, 'b', 'a'), 2);
});

test('plannedBoardRankForDrop: group body appends last + 1, empty pile is 0', () => {
  const tasks = [
    task({ id: 'a', sort_order: 0 }),
    task({ id: 'b', sort_order: 2 }),
    task({ id: 'c', sort_order: 5 }),
  ];

  assert.equal(plannedBoardRankForDrop(tasks, null, null), 6);
  // The mover is excluded from the append target, like the Board's `remaining`.
  assert.equal(plannedBoardRankForDrop(tasks, null, 'c'), 3);
  assert.equal(plannedBoardRankForDrop([], null, null), 0);
  assert.equal(plannedBoardRankForDrop(tasks, null, 'a'), 6);
});

test('plannedBoardRankForDrop: an unknown target falls back to append', () => {
  const tasks = [task({ id: 'a', sort_order: 4 })];
  assert.equal(plannedBoardRankForDrop(tasks, 'ghost', null), 5);
});

// ── Drop decisions ───────────────────────────────────────────────────────

const TODAY = '2026-11-20';

function decisionFixture() {
  const tasks = [
    task({ id: 'a', sort_order: 0 }),
    task({ id: 'b', sort_order: 1 }),
    task({ id: 'c', sort_order: 2 }),
    task({ id: 'noitem', sort_order: 3 }),
    task({ id: 'x', sort_order: 4 }),
    task({ id: 'u', sort_order: 5 }),
    task({ id: 'v', sort_order: 6 }),
    task({ id: 'past', sort_order: 7 }),
    task({ id: 'past2', sort_order: 8 }),
    task({ id: 'open', status: 'OPEN', sort_order: 0 }),
    task({ id: 'done', status: 'COMPLETED', sort_order: 0 }),
    task({ id: 'rowless', sort_order: 8 }),
  ];
  const rows = [
    row('a', { local_date: '2026-11-21', agenda_sort_order: 0 }),
    row('b', { local_date: '2026-11-21', agenda_sort_order: 1 }),
    row('c', { local_date: '2026-11-21', agenda_sort_order: 2 }),
    row('noitem', {
      local_date: '2026-11-21',
      agenda_sort_order: 3,
      item_id: null,
    }),
    row('x', { local_date: '2026-11-22', agenda_sort_order: 0 }),
    row('u', {
      local_date: '2076-11-20',
      undated: true,
      board_sort_order: 5,
    }),
    row('v', {
      local_date: '2076-11-20',
      undated: true,
      board_sort_order: 6,
    }),
    row('past', { local_date: '2026-11-19', agenda_sort_order: 0 }),
    row('past2', { local_date: '2026-11-19', agenda_sort_order: 1 }),
    row('open', {
      local_date: '2076-11-20',
      undated: true,
      board_sort_order: 0,
    }),
    row('done', { local_date: '2026-11-19', agenda_sort_order: 0 }),
  ];
  const groups = buildPlannedGroups(tasks, rows, () => true);
  const byKey = new Map(groups.map((group) => [group.key, group]));
  const rowsByTask = plannedRowsByTaskId(rows);
  const byTask = taskMap(tasks);
  const group = (key: string): PlannedBoardGroup => {
    const found = byKey.get(key);
    assert.ok(found, `missing group ${key}`);
    return found;
  };
  const resolve = (
    activeTaskId: string,
    over: PlannedDropTarget,
    activeStatus: TaskStatus = 'PLANNED',
    activeGroup: PlannedBoardGroup | null = plannedGroupOfTask(
      groups,
      activeTaskId,
    ),
  ) =>
    resolvePlannedDrop({
      activeTaskId,
      activeStatus,
      activeGroup,
      over,
      groups,
      rows: rowsByTask,
      tasks: byTask,
      today: TODAY,
    });
  return { tasks, rows, groups, group, resolve };
}

test('resolvePlannedDrop: PLANNED same dated group card reorders with agenda rank', () => {
  const { group, resolve } = decisionFixture();

  assert.deepEqual(resolve('a', { kind: 'card', taskId: 'c' }), {
    kind: 'reorderDated',
    group: group('2026-11-21'),
    itemId: 'item-a',
    targetRank: 3,
  });
});

test('resolvePlannedDrop: PLANNED card in another dated group moves to that date', () => {
  const { group, resolve } = decisionFixture();

  assert.deepEqual(resolve('a', { kind: 'card', taskId: 'x' }), {
    kind: 'moveToDate',
    group: group('2026-11-22'),
    itemId: 'item-a',
    targetRank: 0,
  });
  // Undated → dated card carries the hovered card's rank too.
  assert.deepEqual(resolve('u', { kind: 'card', taskId: 'x' }), {
    kind: 'moveToDate',
    group: group('2026-11-22'),
    itemId: 'item-u',
    targetRank: 0,
  });
});

test('resolvePlannedDrop: a dated group body appends (no targetRank)', () => {
  const { group, resolve } = decisionFixture();

  assert.deepEqual(
    resolve('a', { kind: 'group', groupId: group('2026-11-22').id }),
    { kind: 'moveToDate', group: group('2026-11-22'), itemId: 'item-a' },
  );
  assert.deepEqual(
    resolve('u', { kind: 'group', groupId: group('2026-11-22').id }),
    { kind: 'moveToDate', group: group('2026-11-22'), itemId: 'item-u' },
  );
});

test('resolvePlannedDrop: dropping on itself or its own group body is none', () => {
  const { group, resolve } = decisionFixture();

  assert.deepEqual(resolve('a', { kind: 'card', taskId: 'a' }), {
    kind: 'none',
  });
  assert.deepEqual(
    resolve('a', { kind: 'group', groupId: group('2026-11-21').id }),
    {
      kind: 'none',
    },
  );
  assert.deepEqual(
    resolve('u', { kind: 'group', groupId: group('undated').id }),
    {
      kind: 'none',
    },
  );
});

test('resolvePlannedDrop: PLANNED dated → Undated card/group unschedules, column body is inert', () => {
  const { group, resolve } = decisionFixture();

  assert.deepEqual(resolve('a', { kind: 'card', taskId: 'u' }), {
    kind: 'unschedule',
    itemId: 'item-a',
  });
  assert.deepEqual(
    resolve('a', { kind: 'group', groupId: group('undated').id }),
    { kind: 'unschedule', itemId: 'item-a' },
  );
  // The column's padding is not the Undated group: a PLANNED date never
  // changes from a body drop.
  assert.deepEqual(resolve('a', { kind: 'column' }), { kind: 'none' });
});

test('resolvePlannedDrop: PLANNED Undated card reorders, Undated body/column is none', () => {
  const { resolve } = decisionFixture();

  assert.deepEqual(resolve('u', { kind: 'card', taskId: 'v' }), {
    kind: 'reorderUndated',
    taskId: 'u',
    targetRank: 6,
  });
  assert.deepEqual(resolve('u', { kind: 'column' }), { kind: 'none' });
});

test('resolvePlannedDrop: PLANNED on another status card leaves the column', () => {
  const { resolve } = decisionFixture();

  assert.deepEqual(resolve('a', { kind: 'card', taskId: 'done' }), {
    kind: 'leavePlanned',
  });
  // An OPEN card is another column's card too.
  assert.deepEqual(resolve('u', { kind: 'card', taskId: 'open' }), {
    kind: 'leavePlanned',
  });
});

test('resolvePlannedDrop: non-PLANNED joins a dated card or body', () => {
  const { group, resolve } = decisionFixture();

  assert.deepEqual(
    resolve('open', { kind: 'card', taskId: 'x' }, 'OPEN', null),
    {
      kind: 'joinDated',
      group: group('2026-11-22'),
      targetRank: 0,
    },
  );
  assert.deepEqual(
    resolve(
      'open',
      { kind: 'group', groupId: group('2026-11-21').id },
      'OPEN',
      null,
    ),
    { kind: 'joinDated', group: group('2026-11-21') },
  );
});

test('resolvePlannedDrop: non-PLANNED joins Undated by card rank or append', () => {
  const { group, resolve } = decisionFixture();

  assert.deepEqual(
    resolve('open', { kind: 'card', taskId: 'v' }, 'OPEN', null),
    {
      kind: 'joinUndated',
      targetRank: 6,
    },
  );
  assert.deepEqual(
    resolve(
      'open',
      { kind: 'group', groupId: group('undated').id },
      'OPEN',
      null,
    ),
    { kind: 'joinUndated', targetRank: 9 },
  );
  assert.deepEqual(resolve('open', { kind: 'column' }, 'OPEN', null), {
    kind: 'joinUndated',
    targetRank: 9,
  });
});

test('resolvePlannedDrop: non-PLANNED on a non-Planned card is none', () => {
  const { resolve } = decisionFixture();

  assert.deepEqual(
    resolve('open', { kind: 'card', taskId: 'done' }, 'OPEN', null),
    {
      kind: 'none',
    },
  );
  assert.deepEqual(
    resolve('open', { kind: 'card', taskId: 'ghost' }, 'OPEN', null),
    {
      kind: 'none',
    },
  );
});

test('resolvePlannedDrop: past dated targets are inert in every direction', () => {
  const { group, resolve } = decisionFixture();
  const pastGroup = group('2026-11-19');

  assert.deepEqual(resolve('a', { kind: 'card', taskId: 'past' }), {
    kind: 'none',
  });
  assert.deepEqual(resolve('a', { kind: 'group', groupId: pastGroup.id }), {
    kind: 'none',
  });
  // Even the past group's own members cannot reorder there.
  assert.deepEqual(resolve('past', { kind: 'group', groupId: pastGroup.id }), {
    kind: 'none',
  });
  assert.deepEqual(
    resolve('open', { kind: 'card', taskId: 'past' }, 'OPEN', null),
    { kind: 'none' },
  );
  assert.deepEqual(
    resolve('open', { kind: 'group', groupId: pastGroup.id }, 'OPEN', null),
    { kind: 'none' },
  );
});

test('resolvePlannedDrop: a past dated group still reorders inside itself', () => {
  const { group, resolve } = decisionFixture();
  const pastGroup = group('2026-11-19');

  // A same-group reorder is the agenda `/move`, which has no date floor: the
  // card may reorder inside its OWN past day. `past` (rank 0) dropped over
  // `past2` (rank 1) lands just after it.
  assert.deepEqual(resolve('past', { kind: 'card', taskId: 'past2' }), {
    kind: 'reorderDated',
    group: pastGroup,
    itemId: 'item-past',
    targetRank: 2,
  });
  // The reverse direction takes the hovered card's own rank.
  assert.deepEqual(resolve('past2', { kind: 'card', taskId: 'past' }), {
    kind: 'reorderDated',
    group: pastGroup,
    itemId: 'item-past2',
    targetRank: 0,
  });
  // A card from another group still cannot move into the past day.
  assert.deepEqual(resolve('a', { kind: 'card', taskId: 'past2' }), {
    kind: 'none',
  });
});

test('resolvePlannedDrop: today itself is a valid dated target', () => {
  const tasks = [
    task({ id: 'a', sort_order: 0 }),
    task({ id: 'today', sort_order: 1 }),
  ];
  const rows = [
    row('a', { local_date: '2026-11-21', agenda_sort_order: 0 }),
    row('today', { local_date: TODAY, agenda_sort_order: 0 }),
  ];
  const groups = buildPlannedGroups(tasks, rows, () => true);
  const todayGroup = groups.find((group) => group.key === TODAY);
  assert.ok(todayGroup);

  assert.deepEqual(
    resolvePlannedDrop({
      activeTaskId: 'a',
      activeStatus: 'PLANNED',
      activeGroup: plannedGroupOfTask(groups, 'a'),
      over: { kind: 'group', groupId: todayGroup.id },
      groups,
      rows: plannedRowsByTaskId(rows),
      tasks: taskMap(tasks),
      today: TODAY,
    }),
    { kind: 'moveToDate', group: todayGroup, itemId: 'item-a' },
  );
});

test('resolvePlannedDrop: a rowless PLANNED task has no item id, so date writes are refused', () => {
  const { resolve } = decisionFixture();

  // `noitem` is dated but its row has no item_id; `rowless` has no row at all.
  assert.deepEqual(resolve('noitem', { kind: 'card', taskId: 'x' }), {
    kind: 'none',
  });
  assert.deepEqual(resolve('noitem', { kind: 'column' }), { kind: 'none' });
  assert.deepEqual(resolve('rowless', { kind: 'card', taskId: 'x' }), {
    kind: 'none',
  });
});

test('resolvePlannedDrop: unknown or malformed group ids are none', () => {
  const { resolve } = decisionFixture();

  assert.deepEqual(resolve('a', { kind: 'group', groupId: '2026-11-22' }), {
    kind: 'none',
  });
  assert.deepEqual(
    resolve('a', { kind: 'group', groupId: 'planned-group:2030-01-01' }),
    { kind: 'none' },
  );
});

// ── No mutation of inputs ────────────────────────────────────────────────

test('buildPlannedGroups and resolvePlannedDrop never mutate their inputs', () => {
  const tasks = [
    task({ id: 'b', sort_order: 1 }),
    task({ id: 'a', sort_order: 0 }),
    task({ id: 'u', sort_order: 2 }),
  ];
  const rows = [
    row('b', { local_date: '2026-11-21', agenda_sort_order: 1 }),
    row('a', { local_date: '2026-11-21', agenda_sort_order: 0 }),
    row('u', { local_date: '2076-11-20', undated: true, board_sort_order: 2 }),
  ];
  const tasksBefore = JSON.parse(JSON.stringify(tasks));
  const rowsBefore = JSON.parse(JSON.stringify(rows));

  const groups = buildPlannedGroups(tasks, rows, () => true);
  const groupsBefore = JSON.parse(JSON.stringify(groups));
  resolvePlannedDrop({
    activeTaskId: 'a',
    activeStatus: 'PLANNED',
    activeGroup: plannedGroupOfTask(groups, 'a'),
    over: { kind: 'card', taskId: 'b' },
    groups,
    rows: plannedRowsByTaskId(rows),
    tasks: taskMap(tasks),
    today: TODAY,
  });
  resolvePlannedDrop({
    activeTaskId: 'a',
    activeStatus: 'PLANNED',
    activeGroup: plannedGroupOfTask(groups, 'a'),
    over: { kind: 'group', groupId: groups[0].id },
    groups,
    rows: plannedRowsByTaskId(rows),
    tasks: taskMap(tasks),
    today: TODAY,
  });

  assert.deepEqual(tasks, tasksBefore);
  assert.deepEqual(rows, rowsBefore);
  assert.deepEqual(groups, groupsBefore);
});

// ── Optimistic row edits ─────────────────────────────────────────────────

const DATE = '2026-11-21';

test('applyPlannedAgendaMove: shifts only target-date dated peers at/after the rank', () => {
  const rows = [
    row('a', {
      item_id: 'item-a',
      local_date: DATE,
      agenda_sort_order: 0,
    }),
    row('b', {
      item_id: 'item-b',
      local_date: DATE,
      agenda_sort_order: 1,
    }),
    row('c', {
      item_id: 'item-c',
      local_date: DATE,
      agenda_sort_order: 2,
    }),
    row('x', {
      item_id: 'item-x',
      local_date: '2026-11-22',
      agenda_sort_order: 5,
    }),
    row('u', {
      item_id: 'item-u',
      local_date: '2076-11-20',
      agenda_sort_order: 5,
      undated: true,
    }),
  ];
  const before = JSON.parse(JSON.stringify(rows));

  const next = applyPlannedAgendaMove(rows, DATE, 'item-a', 2);

  assert.deepEqual(
    next.map((entry) => [entry.task_id, entry.agenda_sort_order]),
    [
      ['a', 2],
      ['b', 1],
      ['c', 3],
      ['x', 5],
      ['u', 5],
    ],
  );
  assert.deepEqual(rows, before);
  assert.notEqual(next, rows);
});

test('applyPlannedAgendaMove: no-op, missing mover and an undated mover keep the reference', () => {
  const rows = [
    row('a', { item_id: 'item-a', local_date: DATE, agenda_sort_order: 0 }),
    row('b', { item_id: 'item-b', local_date: DATE, agenda_sort_order: 1 }),
    row('u', {
      item_id: 'item-u',
      local_date: '2076-11-20',
      agenda_sort_order: 4,
      undated: true,
    }),
  ];

  // `b` already sits at the target rank and has no peer at/after it.
  assert.equal(applyPlannedAgendaMove(rows, DATE, 'item-b', 1), rows);
  assert.equal(applyPlannedAgendaMove(rows, DATE, 'ghost', 0), rows);
  assert.equal(applyPlannedAgendaMove(rows, '2076-11-20', 'item-u', 0), rows);
});

test('applyPlannedBoardReorder: an upward move shifts the crossed peers down', () => {
  const rows = [
    row('a', { board_sort_order: 0 }),
    row('b', { board_sort_order: 1 }),
    row('c', { board_sort_order: 2 }),
    row('u', { board_sort_order: 3, undated: true }),
  ];
  const before = JSON.parse(JSON.stringify(rows));

  const next = applyPlannedBoardReorder(rows, 'u', 0);

  assert.deepEqual(
    next.map((entry) => [entry.task_id, entry.board_sort_order]),
    [
      ['a', 1],
      ['b', 2],
      ['c', 3],
      ['u', 0],
    ],
  );
  assert.deepEqual(rows, before);
});

test('applyPlannedBoardReorder: a downward move mirrors reorder_in_place', () => {
  const rows = [
    row('a', { board_sort_order: 0 }),
    row('b', { board_sort_order: 1 }),
    row('c', { board_sort_order: 2 }),
  ];

  // A dropped onto C sends C's own rank; the crossed peers (B, C) shift up
  // one, so the visible order is [B, C, A], not [B, A, C].
  const next = applyPlannedBoardReorder(rows, 'a', 2);

  assert.deepEqual(
    next.map((entry) => [entry.task_id, entry.board_sort_order]),
    [
      ['a', 2],
      ['b', 0],
      ['c', 1],
    ],
  );
});

test('applyPlannedBoardReorder: a no-op and a missing task keep the reference', () => {
  const rows = [
    row('a', { board_sort_order: 0 }),
    row('b', { board_sort_order: 1 }),
  ];

  assert.equal(applyPlannedBoardReorder(rows, 'b', 1), rows);
  assert.equal(applyPlannedBoardReorder(rows, 'ghost', 0), rows);
});

test('movePlannedRowToDate: an explicit rank shifts target peers and keeps ids/Board rank', () => {
  const rows = [
    row('a', {
      item_id: 'item-a',
      local_date: '2026-11-21',
      agenda_sort_order: 4,
      board_sort_order: 9,
    }),
    row('b', {
      item_id: 'item-b',
      local_date: '2026-11-22',
      agenda_sort_order: 0,
      board_sort_order: 1,
    }),
    row('c', {
      item_id: 'item-c',
      local_date: '2026-11-22',
      agenda_sort_order: 1,
      board_sort_order: 2,
    }),
    row('u', {
      item_id: 'item-u',
      local_date: '2076-11-20',
      agenda_sort_order: 7,
      undated: true,
      board_sort_order: 3,
    }),
  ];
  const before = JSON.parse(JSON.stringify(rows));

  const next = movePlannedRowToDate(rows, 'item-a', '2026-11-22', 1);

  assert.deepEqual(
    next.map((entry) => [
      entry.task_id,
      entry.local_date,
      entry.undated,
      entry.agenda_sort_order,
      entry.board_sort_order,
    ]),
    [
      ['a', '2026-11-22', false, 1, 9],
      ['b', '2026-11-22', false, 0, 1],
      ['c', '2026-11-22', false, 2, 2],
      ['u', '2076-11-20', true, 7, 3],
    ],
  );
  assert.equal(next[0].item_id, 'item-a');
  assert.deepEqual(rows, before);
});

test('movePlannedRowToDate: without a rank appends, 0 on an empty day', () => {
  const rows = [
    row('a', {
      item_id: 'item-a',
      local_date: '2026-11-21',
      agenda_sort_order: 4,
    }),
    row('b', {
      item_id: 'item-b',
      local_date: '2026-11-22',
      agenda_sort_order: 0,
    }),
    row('c', {
      item_id: 'item-c',
      local_date: '2026-11-22',
      agenda_sort_order: 2,
    }),
  ];

  const appended = movePlannedRowToDate(rows, 'item-a', '2026-11-22');
  assert.equal(appended[0].agenda_sort_order, 3);
  assert.equal(appended[0].local_date, '2026-11-22');
  assert.equal(appended[0].undated, false);

  const empty = movePlannedRowToDate(rows, 'item-a', '2026-11-25');
  assert.equal(empty[0].agenda_sort_order, 0);
  assert.equal(empty[0].local_date, '2026-11-25');
});

test('movePlannedRowToDate: same date/rank and a missing item keep the reference', () => {
  const rows = [
    row('a', {
      item_id: 'item-a',
      local_date: '2026-11-22',
      agenda_sort_order: 1,
    }),
  ];

  assert.equal(movePlannedRowToDate(rows, 'item-a', '2026-11-22', 1), rows);
  assert.equal(movePlannedRowToDate(rows, 'ghost', '2026-11-22', 1), rows);
});

test('markPlannedRowUndated: keeps local_date and Board rank, clears the agenda rank', () => {
  const rows = [
    row('a', {
      item_id: 'item-a',
      local_date: DATE,
      agenda_sort_order: 3,
      board_sort_order: 7,
    }),
    row('b', { item_id: 'item-b', agenda_sort_order: 4 }),
  ];
  const before = JSON.parse(JSON.stringify(rows));

  const next = markPlannedRowUndated(rows, 'item-a');

  assert.deepEqual(next[0], {
    ...rows[0],
    undated: true,
    agenda_sort_order: null,
  });
  assert.equal(next[0].local_date, DATE);
  assert.equal(next[0].board_sort_order, 7);
  assert.equal(next[1], rows[1]);
  assert.deepEqual(rows, before);
});

test('markPlannedRowUndated: already-undated and missing ids keep the reference', () => {
  const rows = [
    row('u', {
      item_id: 'item-u',
      local_date: '2076-11-20',
      agenda_sort_order: null,
      undated: true,
    }),
  ];

  assert.equal(markPlannedRowUndated(rows, 'item-u'), rows);
  assert.equal(markPlannedRowUndated(rows, 'ghost'), rows);
});

test('removePlannedTask: drops the row and keeps the reference when missing', () => {
  const rows = [row('a'), row('b'), row('c')];

  const next = removePlannedTask(rows, 'b');

  assert.deepEqual(
    next.map((entry) => entry.task_id),
    ['a', 'c'],
  );
  assert.equal(next[0], rows[0]);
  assert.equal(next[1], rows[2]);
  assert.equal(removePlannedTask(rows, 'ghost'), rows);
});

test('upsertPlannedRow: replaces by task_id and preserves the other row references', () => {
  const rows = [
    row('a', { board_sort_order: 1 }),
    row('b', { board_sort_order: 2 }),
  ];
  const replacement = row('a', { board_sort_order: 9 });

  const replaced = upsertPlannedRow(rows, replacement);

  assert.equal(replaced.length, 2);
  assert.equal(replaced[0], replacement);
  assert.equal(replaced[1], rows[1]);

  const appended = upsertPlannedRow(rows, row('c', { board_sort_order: 3 }));
  assert.deepEqual(
    appended.map((entry) => entry.task_id),
    ['a', 'b', 'c'],
  );
  assert.equal(appended[0], rows[0]);
  assert.equal(appended[1], rows[1]);
});

test('upsertPlannedRow: an identical row keeps the array reference', () => {
  const rows = [row('a', { board_sort_order: 1 })];

  assert.equal(upsertPlannedRow(rows, rows[0]), rows);
  assert.equal(upsertPlannedRow(rows, { ...rows[0] }), rows);
});

test('plannedRowFromItem: builds a dated row from the agenda item and its task rank', () => {
  const item = agendaItem({
    id: 'item-a',
    ref_id: 'task-a',
    sort_order: 4,
    task: task({ id: 'task-a', sort_order: 7 }),
  });

  assert.deepEqual(plannedRowFromItem(item), {
    task_id: 'task-a',
    item_id: 'item-a',
    local_date: '2026-11-21',
    agenda_sort_order: 4,
    board_sort_order: 7,
    undated: false,
  });

  // No task embed: the caller's fallback wins, then 0.
  assert.equal(
    plannedRowFromItem(agendaItem({ ref_id: 'task-b' }), 5).board_sort_order,
    5,
  );
  assert.equal(
    plannedRowFromItem(agendaItem({ ref_id: 'task-b' })).board_sort_order,
    0,
  );
});
