// Unit tests for Home's pure stream helpers (day label, date-window walk,
// stream bounds / grouping, pending predicate, and the optimistic cache
// edits). The reorder tests pin the exact server semantics of
// `POST /api/agenda/items/:id/move`: peers at/after the target rank shift up
// one and the item lands on it; `moveStreamItem` pins the same rule for the
// cross-day `reschedule`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import type {
  AgendaItemRecord,
  AgendaResponse,
  OccurrenceRecord,
  TaskRecord,
} from '../../types';
import {
  agendaDateLabel,
  agendaMoveTarget,
  agendaMoveTargetAt,
  appendRankOnStream,
  applyAgendaMove,
  canReschedule,
  crossDaySortOrder,
  dedupeAgendaItems,
  eachCivilDate,
  groupItemsByDate,
  isAgendaItemParked,
  isAgendaItemPending,
  moveStreamItem,
  partitionAgendaItems,
  streamBounds,
  streamItemsOf,
  updateStreamItem,
} from './agenda-helpers';

// ── Fixtures ────────────────────────────────────────────────────────────

const TODAY = '2026-08-23';

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

function item(id: string, sortOrder: number, date = TODAY): AgendaItemRecord {
  return {
    id,
    user_id: 'u-1',
    local_date: date,
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
  options: { updatedAt?: string; routineDeleted?: boolean } = {},
): AgendaItemRecord {
  return {
    ...item(id, 0),
    kind: 'occurrence',
    ref_id: `occ-${id}`,
    occurrence: {
      id: `occ-${id}`,
      routine_id: 'r-1',
      user_id: 'u-1',
      local_date: TODAY,
      title: null,
      resolved_title: 'Morning run',
      status,
      estimated_minutes: 30,
      rrule: 'DTSTART:20260823T070000\nRRULE:FREQ=DAILY',
      calendar_id: null,
      google_event_id: null,
      created_at: '2026-08-23T00:00:00Z',
      updated_at: options.updatedAt ?? '2026-08-23T00:00:00Z',
      routine_deleted: options.routineDeleted ?? false,
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

/** One stream page: `items` in a `from … to` window (server echo included). */
function page(
  from: string,
  to: string,
  items: AgendaItemRecord[],
): AgendaResponse {
  return {
    items,
    from,
    to,
    today: TODAY,
    time_zone: 'UTC',
    anchor_date: from,
    earliest_date: '2026-08-01',
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

test('agendaDateLabel: same-year dates render as "Tue 25 Aug"', () => {
  assert.equal(agendaDateLabel('2026-08-25', '2026-08-23'), 'Tue 25 Aug');
  assert.equal(agendaDateLabel('2026-12-01', '2026-08-23'), 'Tue 1 Dec');
});

test('agendaDateLabel: another year appends the year, relative labels never do', () => {
  assert.equal(agendaDateLabel('2027-01-01', '2026-08-23'), 'Fri 1 Jan 2027');
  assert.equal(agendaDateLabel('2025-12-30', '2026-08-23'), 'Tue 30 Dec 2025');
  // Yesterday / Today / Tomorrow stay bare even across a year boundary.
  assert.equal(agendaDateLabel('2025-12-31', '2026-01-01'), 'Yesterday');
  assert.equal(agendaDateLabel('2026-01-01', '2025-12-31'), 'Tomorrow');
});

test('agendaDateLabel: malformed input returns it unchanged', () => {
  assert.equal(agendaDateLabel('', '2026-08-23'), '');
  assert.equal(agendaDateLabel('not-a-date', '2026-08-23'), 'not-a-date');
  assert.equal(agendaDateLabel('2026-13-40', '2026-08-23'), '2026-13-40');
});

// ── eachCivilDate ───────────────────────────────────────────────────────

test('eachCivilDate: inclusive ascending run', () => {
  assert.deepEqual(eachCivilDate('2026-08-23', '2026-08-26'), [
    '2026-08-23',
    '2026-08-24',
    '2026-08-25',
    '2026-08-26',
  ]);
  assert.deepEqual(eachCivilDate('2026-08-23', '2026-08-23'), ['2026-08-23']);
});

test('eachCivilDate: walks a month boundary', () => {
  assert.deepEqual(eachCivilDate('2026-08-30', '2026-09-02'), [
    '2026-08-30',
    '2026-08-31',
    '2026-09-01',
    '2026-09-02',
  ]);
});

test('eachCivilDate: an inverted or bad range is empty', () => {
  assert.deepEqual(eachCivilDate('2026-08-24', '2026-08-23'), []);
  assert.deepEqual(eachCivilDate('not-a-date', '2026-08-23'), []);
  assert.deepEqual(eachCivilDate('2026-08-23', '2026-13-01'), []);
  assert.deepEqual(eachCivilDate('', ''), []);
});

// ── streamBounds ────────────────────────────────────────────────────────

test('streamBounds: no pages yields null', () => {
  assert.equal(streamBounds([]), null);
});

test('streamBounds: the first page starts the span, the last ends it', () => {
  const pages = [
    page('2026-08-16', '2026-08-22', []),
    page('2026-08-23', '2026-08-29', []),
    page('2026-08-30', '2026-09-05', []),
  ];
  assert.deepEqual(streamBounds(pages), {
    from: '2026-08-16',
    to: '2026-09-05',
  });
  assert.deepEqual(streamBounds([page('2026-08-23', '2026-08-29', [])]), {
    from: '2026-08-23',
    to: '2026-08-29',
  });
});

// ── groupItemsByDate ────────────────────────────────────────────────────

test('groupItemsByDate: buckets by local_date in input order', () => {
  const a = item('a', 1, '2026-08-23');
  const b = item('b', 0, '2026-08-23');
  const c = item('c', 0, '2026-08-25');
  const grouped = groupItemsByDate([a, c, b]);
  assert.deepEqual([...grouped.keys()], ['2026-08-23', '2026-08-25']);
  assert.deepEqual(
    grouped.get('2026-08-23')?.map((entry) => entry.id),
    ['a', 'b'],
  );
  assert.deepEqual(
    grouped.get('2026-08-25')?.map((entry) => entry.id),
    ['c'],
  );
  assert.equal(grouped.get('2026-08-24'), undefined);
});

test('groupItemsByDate: empty input yields an empty map', () => {
  assert.equal(groupItemsByDate([]).size, 0);
});

// ── isAgendaItemPending ─────────────────────────────────────────────────

test('isAgendaItemPending: living tasks count, terminal tasks do not', () => {
  assert.equal(isAgendaItemPending(taskItem('a', 'OPEN')), true);
  assert.equal(isAgendaItemPending(taskItem('b', 'PLANNED')), true);
  assert.equal(isAgendaItemPending(taskItem('c', 'IN_PROGRESS')), true);
  assert.equal(isAgendaItemPending(taskItem('d', 'COMPLETED')), false);
  assert.equal(isAgendaItemPending(taskItem('e', 'DISCARDED')), false);
});

test('isAgendaItemPending: occurrences count while waiting or running', () => {
  assert.equal(isAgendaItemPending(occurrenceItem('a', 'pending')), true);
  assert.equal(isAgendaItemPending(occurrenceItem('b', 'in_progress')), true);
  assert.equal(isAgendaItemPending(occurrenceItem('c', 'done')), false);
  assert.equal(isAgendaItemPending(occurrenceItem('d', 'skipped')), false);
});

test('isAgendaItemPending: a deleted routine\u2019s row is never pending', () => {
  assert.equal(
    isAgendaItemPending(
      occurrenceItem('a', 'pending', { routineDeleted: true }),
    ),
    false,
  );
  assert.equal(
    isAgendaItemPending(
      occurrenceItem('b', 'in_progress', { routineDeleted: true }),
    ),
    false,
  );
});

test('isAgendaItemPending: an embedless orphan is never pending', () => {
  assert.equal(isAgendaItemPending(item('orphan', 0)), false);
  assert.equal(
    isAgendaItemPending({ ...item('orphan', 0), kind: 'occurrence' }),
    false,
  );
});

// ── updateStreamItem ────────────────────────────────────────────────────

test('updateStreamItem: maps the matching item inside its own page', () => {
  const data = {
    pages: [
      page('2026-08-16', '2026-08-22', [item('a', 0, '2026-08-20')]),
      page('2026-08-23', '2026-08-29', [
        item('b', 0),
        occurrenceItem('c', 'pending'),
      ]),
    ],
    pageParams: [undefined, { from: '2026-08-23', to: '2026-08-29' }],
  };
  const next = updateStreamItem(data, 'c', (entry) => ({
    ...entry,
    sort_order: 7,
  }));
  assert.notEqual(next, data);
  assert.equal(next?.pages[0], data.pages[0], 'untouched page keeps its ref');
  assert.equal(next?.pages[1].items.map((entry) => entry.id).join(','), 'b,c');
  assert.equal(next?.pages[1].items[1].sort_order, 7);
  assert.equal(data.pages[1].items[1].sort_order, 0, 'input not mutated');
});

test('updateStreamItem: a missing id (or no data) keeps the same reference', () => {
  const data = {
    pages: [page('2026-08-23', '2026-08-29', [item('a', 0)])],
    pageParams: [undefined],
  };
  assert.equal(
    updateStreamItem(data, 'missing', (entry) => entry),
    data,
  );
  assert.equal(
    updateStreamItem(undefined, 'a', (entry) => entry),
    undefined,
  );
});

// ── moveStreamItem ──────────────────────────────────────────────────────

test('moveStreamItem: an item travels between pages and shifts its peer', () => {
  const leaving = item('a', 0, '2026-08-20');
  const peerBefore = item('p', 0, '2026-08-24');
  const peerAtRank = item('q', 1, '2026-08-24');
  const data = {
    pages: [
      page('2026-08-16', '2026-08-22', [leaving]),
      page('2026-08-23', '2026-08-29', [peerBefore, peerAtRank]),
    ],
    pageParams: [undefined, { from: '2026-08-23', to: '2026-08-29' }],
  };
  const next = moveStreamItem(data, 'a', '2026-08-24', 1);
  assert.notEqual(next, data);
  // Gone from its old page.
  assert.deepEqual(
    next?.pages[0].items.map((entry) => entry.id),
    [],
  );
  // Landed on the target date in the target page…
  const landed = next?.pages[1].items.find((entry) => entry.id === 'a');
  assert.equal(landed?.local_date, '2026-08-24');
  assert.equal(landed?.sort_order, 1);
  // …with the peer AT the rank shifted up one and the earlier peer untouched.
  const shifted = next?.pages[1].items.find((entry) => entry.id === 'q');
  assert.equal(shifted?.sort_order, 2);
  const untouched = next?.pages[1].items.find((entry) => entry.id === 'p');
  assert.equal(untouched?.sort_order, 0);
  // The input cache object is untouched.
  assert.equal(data.pages[0].items.length, 1);
  assert.equal(data.pages[1].items[1].sort_order, 1);
});

test('moveStreamItem: peers on OTHER dates never shift', () => {
  const data = {
    pages: [
      page('2026-08-23', '2026-08-29', [
        item('a', 0, '2026-08-23'),
        item('b', 0, '2026-08-24'),
      ]),
    ],
    pageParams: [undefined],
  };
  const next = moveStreamItem(data, 'a', '2026-08-24', 0);
  assert.equal(
    next?.pages[0].items.find((entry) => entry.id === 'a')?.sort_order,
    0,
  );
  // The 2026-08-24 peer at rank 0 shifts up even though the rank came from
  // another day's pile.
  assert.equal(
    next?.pages[0].items.find((entry) => entry.id === 'b')?.sort_order,
    1,
  );
});

test('moveStreamItem: an uncovered target date drops the row from the stream', () => {
  const data = {
    pages: [page('2026-08-23', '2026-08-29', [item('a', 0)])],
    pageParams: [undefined],
  };
  const next = moveStreamItem(data, 'a', '2026-09-15', 3);
  // No loaded window covers 2026-09-15, so the row cannot be painted on a
  // day this page does not hold: it leaves the cache and the real page will
  // paint the server's copy when it loads.
  assert.deepEqual(
    next?.pages.map((entry) => entry.items.map((row) => row.id)),
    [[]],
  );
  assert.equal(
    streamItemsOf(next).some((entry) => entry.id === 'a'),
    false,
    'gone from every page',
  );
  // The input cache object is untouched.
  assert.equal(data.pages[0].items.length, 1);
});

test('moveStreamItem: a missing id (or no data) keeps the same reference', () => {
  const data = {
    pages: [page('2026-08-23', '2026-08-29', [item('a', 0)])],
    pageParams: [undefined],
  };
  assert.equal(moveStreamItem(data, 'missing', TODAY, 0), data);
  assert.equal(moveStreamItem(undefined, 'a', TODAY, 0), undefined);
});

// ── appendRankOnStream / streamItemsOf / dedupeAgendaItems ──────────────

test('appendRankOnStream: max + 1 on the date, 0 on an empty pile', () => {
  const data = {
    pages: [
      page('2026-08-16', '2026-08-22', [item('a', 4, '2026-08-20')]),
      page('2026-08-23', '2026-08-29', [item('b', 0), item('c', 2)]),
    ],
    pageParams: [undefined, { from: '2026-08-23', to: '2026-08-29' }],
  };
  assert.equal(appendRankOnStream(data, TODAY), 3);
  assert.equal(appendRankOnStream(data, '2026-08-20'), 5);
  assert.equal(appendRankOnStream(data, '2026-08-25'), 0);
  assert.equal(appendRankOnStream(undefined, TODAY), 0);
});

test('streamItemsOf: flattens pages in order', () => {
  const data = {
    pages: [
      page('2026-08-16', '2026-08-22', [item('a', 0, '2026-08-20')]),
      page('2026-08-23', '2026-08-29', [item('b', 0)]),
    ],
    pageParams: [undefined, { from: '2026-08-23', to: '2026-08-29' }],
  };
  assert.deepEqual(
    streamItemsOf(data).map((entry) => entry.id),
    ['a', 'b'],
  );
  assert.deepEqual(streamItemsOf(undefined), []);
});

test('dedupeAgendaItems: one survivor per id, the first page wins', () => {
  // A stranded optimistic row can leave the same id in two pages; grouping
  // that would paint the day twice (duplicate keys and sortable ids).
  const data = {
    pages: [
      page('2026-08-16', '2026-08-22', [item('a', 0, '2026-08-20')]),
      page('2026-08-23', '2026-08-29', [
        item('a', 4, '2026-08-20'),
        item('b', 0),
      ]),
    ],
    pageParams: [undefined, { from: '2026-08-23', to: '2026-08-29' }],
  };
  const deduped = dedupeAgendaItems(streamItemsOf(data));
  assert.deepEqual(
    deduped.map((entry) => entry.id),
    ['a', 'b'],
  );
  assert.equal(
    deduped[0].sort_order,
    0,
    'the earlier page\u2019s copy survives',
  );
  // A list with no duplicates comes back with the same rows.
  assert.deepEqual(
    dedupeAgendaItems([item('b', 0)]).map((entry) => entry.id),
    ['b'],
  );
  assert.deepEqual(dedupeAgendaItems([]), []);
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

// ── crossDaySortOrder ───────────────────────────────────────────────────

test('crossDaySortOrder: a drop over a row sends that row\u2019s rank, not its index', () => {
  // Rank gaps and parked (Completed) rows keep their ranks in the same number
  // space, so a living list's index is not the rank the server wants.
  const living = [
    item('a', 0, '2026-08-25'),
    item('c', 5, '2026-08-25'),
    item('d', 6, '2026-08-25'),
  ];
  // D sits at index 2 but holds rank 6: the item must land on D's slot (D
  // shifts to 7), never at rank 2 (which would land it before C).
  assert.equal(crossDaySortOrder(living, 'd'), 6);
  assert.equal(crossDaySortOrder(living, 'c'), 5);
  assert.equal(crossDaySortOrder(living, 'a'), 0);
});

test('crossDaySortOrder: the day container and a missing row mean "append"', () => {
  const living = [item('a', 0, '2026-08-25'), item('c', 5, '2026-08-25')];
  // `null` = dropped on the day container itself; `undefined` tells the
  // caller to omit `sort_order` so the server appends at `max + 1`.
  assert.equal(crossDaySortOrder(living, null), undefined);
  assert.equal(crossDaySortOrder(living, 'missing'), undefined);
  assert.equal(crossDaySortOrder([], 'a'), undefined);
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

test('canReschedule: a deleted routine\u2019s history never moves', () => {
  assert.equal(
    canReschedule(occurrenceItem('a', 'skipped', { routineDeleted: true })),
    false,
  );
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
    {
      ...taskItem('t-done', 'COMPLETED', '2026-08-23T09:00:00Z'),
      sort_order: 1,
    },
    {
      ...occurrenceItem('o-done', 'done', {
        updatedAt: '2026-08-23T08:00:00Z',
      }),
      sort_order: 2,
    },
    { ...occurrenceItem('o-skipped', 'skipped'), sort_order: 3 },
    { ...occurrenceItem('o-pending', 'pending'), sort_order: 4 },
    { ...occurrenceItem('o-progress', 'in_progress'), sort_order: 5 },
    { ...taskItem('t-open', 'OPEN'), sort_order: 6 },
    {
      ...taskItem('t-drop', 'DISCARDED', '2026-08-23T07:00:00Z'),
      sort_order: 7,
    },
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
    occurrenceItem('old', 'done', { updatedAt: '2026-08-20T00:00:00Z' }),
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
