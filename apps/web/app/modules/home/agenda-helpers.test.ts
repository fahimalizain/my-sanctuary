// Unit tests for Home's pure stream helpers (day label, date-window walk,
// stream bounds / grouping, month-clamped date math, pending predicate, the
// backlog split, the history window, and the optimistic cache edits). The
// reorder tests pin the exact server semantics of
// `POST /api/agenda/items/:id/move`: peers at/after the target rank shift up
// one and the item lands on it; `moveStreamItem` pins the same rule for the
// cross-day `reschedule`. The backlog tests pin ADR 0007: a dated backlog row
// reorders with the agenda, an OPEN row through the Board's `tasks.sort_order`,
// and a backlog drop appends the row at today+50y.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import type {
  AgendaItemRecord,
  AgendaResponse,
  OccurrenceRecord,
  TaskRecord,
} from '../../types';
import {
  addMonthsClamp,
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
  isOpenBacklogTask,
  moveItemToBacklog,
  moveStreamItem,
  openTaskMoveTargetAt,
  partitionAgendaItems,
  previousHistoryWindow,
  rescheduleMaxDate,
  splitBacklogSection,
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

/** One stream page: `items` in a `from … to` window (server echo included).
 *  `overrides` pins anything else the case cares about — `backlog`, a moved
 *  `anchor_date`, a different `earliest_date`. */
function page(
  from: string,
  to: string,
  items: AgendaItemRecord[],
  overrides: Partial<AgendaResponse> = {},
): AgendaResponse {
  return {
    items,
    backlog: [],
    from,
    to,
    today: TODAY,
    time_zone: 'UTC',
    anchor_date: from,
    earliest_date: '2026-08-01',
    ...overrides,
  };
}

/** The backlog date for TODAY: civil today + 50 years (ADR 0007). */
const BACKLOG_DATE = '2076-08-23';
/** The 10-year line for TODAY: the first date in the backlog region. */
const HORIZON = '2036-08-23';

/** A task-kind agenda row on `date` whose task carries its own Board rank. */
function taskItemAt(
  id: string,
  status: TaskRecord['status'],
  date: string,
  rank = 0,
): AgendaItemRecord {
  return {
    ...item(id, rank, date),
    task: { ...task(id, status, `Task ${id}`), sort_order: rank },
  };
}

/** An occurrence-kind agenda row moved onto `date`. */
function occurrenceItemAt(
  id: string,
  status: OccurrenceRecord['status'],
  date: string,
): AgendaItemRecord {
  const base = occurrenceItem(id, status);
  return {
    ...base,
    local_date: date,
    occurrence: { ...base.occurrence!, local_date: date },
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

// ── addMonthsClamp / rescheduleMaxDate ──────────────────────────────────

test('addMonthsClamp: clamps Feb 29 the way chrono checked_add_months does', () => {
  // The backlog date and the 10-year line are both month adds, so Feb 29 must
  // land on Feb 28 — never JS setUTCMonth's roll into March.
  assert.equal(addMonthsClamp('2024-02-29', 120), '2034-02-28');
  assert.equal(addMonthsClamp('2024-02-29', 600), '2074-02-28');
  // A day that exists in the target month keeps its day-of-month.
  assert.equal(addMonthsClamp('2026-08-23', 120), '2036-08-23');
  assert.equal(addMonthsClamp('2026-08-23', 600), '2076-08-23');
  // Crosses the year boundary forward and back.
  assert.equal(addMonthsClamp('2026-11-30', 3), '2027-02-28');
  assert.equal(addMonthsClamp('2026-03-31', -1), '2026-02-28');
  assert.equal(addMonthsClamp('2026-01-15', 0), '2026-01-15');
});

test('addMonthsClamp: malformed input returns the input', () => {
  assert.equal(addMonthsClamp('', 120), '');
  assert.equal(addMonthsClamp('not-a-date', 600), 'not-a-date');
  // A component rollover (month 13) is malformed too.
  assert.equal(addMonthsClamp('2026-13-01', 120), '2026-13-01');
  assert.equal(addMonthsClamp('2026-02-30', 120), '2026-02-30');
});

test('rescheduleMaxDate: today+10y−1 (the API accepts [today, today+10y))', () => {
  assert.equal(rescheduleMaxDate(TODAY), '2036-08-22');
  assert.equal(rescheduleMaxDate('2024-02-29'), '2034-02-27');
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

// ── previousHistoryWindow ───────────────────────────────────────────────

test('previousHistoryWindow: the default page alone fills the week before the anchor', () => {
  const pages = [
    page('2026-08-23', '2026-08-29', [], { anchor_date: '2026-08-23' }),
  ];
  assert.deepEqual(previousHistoryWindow(pages), {
    from: '2026-08-16',
    to: '2026-08-22',
  });
});

test('previousHistoryWindow: the next window steps back from the oldest page', () => {
  const pages = [
    page('2026-08-16', '2026-08-22', [], { anchor_date: '2026-08-23' }),
    page('2026-08-23', '2026-08-29', [], { anchor_date: '2026-08-23' }),
  ];
  assert.deepEqual(previousHistoryWindow(pages), {
    from: '2026-08-09',
    to: '2026-08-15',
  });
});

test('previousHistoryWindow: there is nothing before the anchor eve', () => {
  assert.equal(
    previousHistoryWindow([
      page('2026-08-23', '2026-08-29', [], {
        anchor_date: '2026-08-23',
        earliest_date: '2026-08-23',
      }),
    ]),
    undefined,
  );
  // The oldest row is the anchor's own eve: no older window exists.
  assert.equal(
    previousHistoryWindow([
      page('2026-08-16', '2026-08-22', [], {
        anchor_date: '2026-08-23',
        earliest_date: '2026-08-22',
      }),
    ]),
    undefined,
  );
  assert.equal(previousHistoryWindow([]), undefined);
});

test('previousHistoryWindow: a gap between history and the anchor is filled first', () => {
  // The anchor jumped forward: a window ending the week before the oldest
  // page would leave 2026-08-08…19 unpainted forever.
  const pages = [
    page('2026-08-01', '2026-08-07', [], { anchor_date: '2026-08-20' }),
    page('2026-08-20', '2026-08-26', [], { anchor_date: '2026-08-20' }),
  ];
  assert.deepEqual(previousHistoryWindow(pages), {
    from: '2026-08-13',
    to: '2026-08-19',
  });
});

test('previousHistoryWindow: clamps to earliest_date and never reaches the anchor', () => {
  const pages = [
    page('2026-08-16', '2026-08-22', [], {
      anchor_date: '2026-08-23',
      earliest_date: '2026-08-10',
    }),
  ];
  const window = previousHistoryWindow(pages);
  assert.deepEqual(window, { from: '2026-08-10', to: '2026-08-15' });
  assert.ok(window && window.to < '2026-08-23');
  assert.ok(window && eachCivilDate(window.from, window.to).length <= 7);
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

// ── isOpenBacklogTask / splitBacklogSection ─────────────────────────────

test('isOpenBacklogTask: only an OPEN task on/past the 10-year line', () => {
  assert.equal(
    isOpenBacklogTask(taskItemAt('a', 'OPEN', BACKLOG_DATE), TODAY),
    true,
  );
  assert.equal(
    isOpenBacklogTask(taskItemAt('b', 'OPEN', HORIZON), TODAY),
    true,
  );
  // One day under the line is still the dated stretch.
  assert.equal(
    isOpenBacklogTask(taskItemAt('c', 'OPEN', '2036-08-22'), TODAY),
    false,
  );
  // The row must sit in the backlog region AND the task be OPEN.
  assert.equal(
    isOpenBacklogTask(taskItemAt('d', 'PLANNED', BACKLOG_DATE), TODAY),
    false,
  );
  assert.equal(
    isOpenBacklogTask(taskItemAt('e', 'IN_PROGRESS', BACKLOG_DATE), TODAY),
    false,
  );
  assert.equal(
    isOpenBacklogTask(taskItemAt('f', 'COMPLETED', BACKLOG_DATE), TODAY),
    false,
  );
  assert.equal(isOpenBacklogTask(occurrenceItem('g', 'pending'), TODAY), false);
  assert.equal(isOpenBacklogTask(item('orphan', 0), TODAY), false);
  // A missing today claims nothing.
  assert.equal(
    isOpenBacklogTask(taskItemAt('h', 'OPEN', BACKLOG_DATE), ''),
    false,
  );
});

test('splitBacklogSection: keeps the dated stretch and never re-sorts it', () => {
  const dated = taskItemAt('d', 'PLANNED', '2026-08-30', 1);
  const open = taskItemAt('a', 'OPEN', BACKLOG_DATE, 9);
  const open2 = taskItemAt('b', 'OPEN', BACKLOG_DATE, 2);
  // The self-heal has not run: a PLANNED row on the backlog date is in
  // neither stretch — the server omits it, and it is not an OPEN target.
  const misplaced = taskItemAt('m', 'PLANNED', BACKLOG_DATE, 5);
  const split = splitBacklogSection([dated, open, open2, misplaced], TODAY);
  assert.deepEqual(
    split.dated.map((entry) => entry.id),
    ['d'],
  );
  // Server order (Board order) survives: a and b keep the array's order even
  // though b's own rank is lower.
  assert.deepEqual(
    split.open.map((entry) => entry.id),
    ['a', 'b'],
  );
});

test('splitBacklogSection: a malformed today yields two empty stretches', () => {
  const split = splitBacklogSection(
    [
      taskItemAt('a', 'OPEN', BACKLOG_DATE),
      taskItemAt('b', 'PLANNED', '2026-08-30'),
    ],
    '',
  );
  assert.deepEqual(split.dated, []);
  assert.deepEqual(split.open, []);
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

test('updateStreamItem: maps a backlog row too', () => {
  const data = {
    pages: [
      page('2026-08-23', '2026-08-29', [item('a', 0)], {
        backlog: [taskItemAt('b', 'OPEN', BACKLOG_DATE, 0)],
      }),
    ],
    pageParams: [undefined],
  };
  const next = updateStreamItem(data, 'b', (entry) => ({
    ...entry,
    sort_order: 9,
  }));
  assert.notEqual(next, data);
  assert.equal(next?.pages[0].backlog[0].sort_order, 9);
  assert.equal(next?.pages[0].items[0].sort_order, 0, 'items untouched');
  assert.equal(data.pages[0].backlog[0].sort_order, 0, 'input not mutated');
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

test('moveStreamItem: a backlog row leaves the backlog and lands on a covered day', () => {
  const data = {
    pages: [
      page('2026-08-23', '2026-08-29', [item('a', 0)], {
        backlog: [taskItemAt('b', 'OPEN', BACKLOG_DATE, 0)],
      }),
    ],
    pageParams: [undefined],
  };
  const next = moveStreamItem(data, 'b', '2026-08-25', 3);
  const page0 = next?.pages[0];
  // No copy is left behind in the backlog…
  assert.deepEqual(page0?.backlog, []);
  // …and the row lands on the target day with its rank.
  const landed = page0?.items.find((entry) => entry.id === 'b');
  assert.equal(landed?.local_date, '2026-08-25');
  assert.equal(landed?.sort_order, 3);
  // An OPEN task is planned by the server on reschedule; the paint mirrors it.
  assert.equal(landed?.task?.status, 'PLANNED');
  assert.equal(data.pages[0].backlog.length, 1, 'input not mutated');
});

test('moveStreamItem: a PLANNED task and an occurrence keep their status', () => {
  const data = {
    pages: [
      page('2026-08-23', '2026-08-29', [item('a', 0)], {
        backlog: [
          taskItemAt('p', 'PLANNED', '2026-08-30', 0),
          occurrenceItemAt('o', 'pending', '2026-08-31'),
        ],
      }),
    ],
    pageParams: [undefined],
  };
  const afterTask = moveStreamItem(data, 'p', '2026-08-26', 0);
  assert.equal(
    afterTask?.pages[0].items.find((entry) => entry.id === 'p')?.task?.status,
    'PLANNED',
  );
  const afterOccurrence = moveStreamItem(data, 'o', '2026-08-27', 1);
  const occurrence = afterOccurrence?.pages[0].items.find(
    (entry) => entry.id === 'o',
  );
  assert.equal(occurrence?.occurrence?.status, 'pending');
  assert.equal(occurrence?.task, null);
});

// ── moveItemToBacklog ───────────────────────────────────────────────────

test('moveItemToBacklog: a PLANNED row flips OPEN and appends the Board rank', () => {
  const planned = taskItemAt('p', 'PLANNED', '2026-08-25', 3);
  const first = taskItemAt('a', 'OPEN', BACKLOG_DATE, 0);
  const last = taskItemAt('b', 'OPEN', BACKLOG_DATE, 7);
  const data = {
    pages: [
      page('2026-08-23', '2026-08-29', [planned, item('x', 0)], {
        backlog: [first, last],
      }),
    ],
    pageParams: [undefined],
  };
  const next = moveItemToBacklog(data, 'p', BACKLOG_DATE);
  const page0 = next?.pages[0];
  assert.deepEqual(
    page0?.items.map((entry) => entry.id),
    ['x'],
  );
  assert.deepEqual(
    page0?.backlog.map((entry) => entry.id),
    ['a', 'b', 'p'],
  );
  const moved = page0?.backlog[2];
  assert.equal(moved?.local_date, BACKLOG_DATE);
  assert.equal(moved?.task?.status, 'OPEN');
  // Append = max(0, 7) + 1 over the OPEN tasks already there.
  assert.equal(moved?.task?.sort_order, 8);
  // The input cache is untouched.
  assert.equal(data.pages[0].items.length, 2);
  assert.equal(data.pages[0].backlog.length, 2);
});

test('moveItemToBacklog: an already-OPEN row keeps its Board rank', () => {
  const open = taskItemAt('o', 'OPEN', '2026-08-25', 4);
  const data = {
    pages: [
      page('2026-08-23', '2026-08-29', [open], {
        backlog: [taskItemAt('a', 'OPEN', BACKLOG_DATE, 9)],
      }),
    ],
    pageParams: [undefined],
  };
  const next = moveItemToBacklog(data, 'o', BACKLOG_DATE);
  const page0 = next?.pages[0];
  assert.deepEqual(
    page0?.items.map((entry) => entry.id),
    [],
  );
  assert.deepEqual(
    page0?.backlog.map((entry) => entry.id),
    ['a', 'o'],
  );
  assert.equal(page0?.backlog[1].task?.sort_order, 4);
});

test('moveItemToBacklog: appends on the greatest-to page and drops the old copy', () => {
  // A row already parked in a history page's backlog (a stale optimistic
  // paint) must not survive there.
  const stale = taskItemAt('s', 'PLANNED', '2026-08-30', 1);
  const data = {
    pages: [
      page('2026-08-16', '2026-08-22', [], { backlog: [stale] }),
      page('2026-08-23', '2026-08-29', [item('a', 0)], {
        backlog: [taskItemAt('b', 'OPEN', BACKLOG_DATE, 2)],
      }),
    ],
    pageParams: [undefined, { from: '2026-08-23', to: '2026-08-29' }],
  };
  const next = moveItemToBacklog(data, 's', BACKLOG_DATE);
  assert.deepEqual(next?.pages[0].backlog, []);
  assert.deepEqual(
    next?.pages[1].backlog.map((entry) => entry.id),
    ['b', 's'],
  );
  assert.equal(
    next?.pages[1].backlog[1].task?.sort_order,
    3,
    'append after the existing OPEN row',
  );
});

test('moveItemToBacklog: a missing id (or no data) keeps the same reference', () => {
  const data = {
    pages: [page('2026-08-23', '2026-08-29', [item('a', 0)])],
    pageParams: [undefined],
  };
  assert.equal(moveItemToBacklog(data, 'missing', BACKLOG_DATE), data);
  assert.equal(moveItemToBacklog(undefined, 'a', BACKLOG_DATE), undefined);
  const empty = { pages: [], pageParams: [] };
  assert.equal(moveItemToBacklog(empty, 'a', BACKLOG_DATE), empty);
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

test('streamItemsOf: each page contributes its items then its backlog', () => {
  // A backlog row must be reachable from a drag handler, so it is part of the
  // stream's lookup list — but never part of the day spine.
  const data = {
    pages: [
      page('2026-08-16', '2026-08-22', [item('a', 0, '2026-08-20')], {
        backlog: [taskItemAt('h', 'OPEN', BACKLOG_DATE, 0)],
      }),
      page('2026-08-23', '2026-08-29', [item('b', 0)], {
        backlog: [taskItemAt('c', 'OPEN', BACKLOG_DATE, 1)],
      }),
    ],
    pageParams: [undefined, { from: '2026-08-23', to: '2026-08-29' }],
  };
  assert.deepEqual(
    streamItemsOf(data).map((entry) => entry.id),
    ['a', 'h', 'b', 'c'],
  );
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

// ── openTaskMoveTargetAt ────────────────────────────────────────────────

test('openTaskMoveTargetAt: the rank is the task\u2019s Board sort_order', () => {
  // The agenda row's own `sort_order` is irrelevant in the OPEN stretch; the
  // Board's `tasks.sort_order` is the one order Home and Board share.
  const items = [
    taskItemAt('a', 'OPEN', BACKLOG_DATE, 0),
    taskItemAt('b', 'OPEN', BACKLOG_DATE, 7),
    taskItemAt('c', 'OPEN', BACKLOG_DATE, 9),
  ];
  // c (index 2) dropped above a (index 0) takes a's Board rank.
  assert.equal(openTaskMoveTargetAt(items, 2, 0), 0);
  // a dropped below b lands just after b.
  assert.equal(openTaskMoveTargetAt(items, 0, 1), 8);
  // a dropped below c lands just after c.
  assert.equal(openTaskMoveTargetAt(items, 0, 2), 10);
});

test('openTaskMoveTargetAt: same index, out of range and embedless rows return null', () => {
  const items = [
    taskItemAt('a', 'OPEN', BACKLOG_DATE, 0),
    taskItemAt('b', 'OPEN', BACKLOG_DATE, 1),
  ];
  assert.equal(openTaskMoveTargetAt(items, 0, 0), null);
  assert.equal(openTaskMoveTargetAt(items, -1, 0), null);
  assert.equal(openTaskMoveTargetAt(items, 0, -1), null);
  assert.equal(openTaskMoveTargetAt(items, 2, 0), null);
  assert.equal(openTaskMoveTargetAt(items, 0, 2), null);
  assert.equal(openTaskMoveTargetAt([], 0, 0), null);
  assert.equal(openTaskMoveTargetAt([item('orphan', 0), items[1]], 0, 1), null);
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

test('canReschedule: tasks move while living; a running task or a finished card stays put', () => {
  assert.equal(canReschedule(taskItem('a', 'OPEN')), true);
  assert.equal(canReschedule(taskItem('b', 'PLANNED')), true);
  // The API 400s `cannot reschedule an in_progress task` — pause first, so
  // the control hides instead of inviting a failure banner.
  assert.equal(canReschedule(taskItem('c', 'IN_PROGRESS')), false);
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
