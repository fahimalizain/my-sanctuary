// Unit tests for Home's pure stream helpers (day label, date-window walk,
// stream bounds / grouping, month-clamped date math, pending predicate, the
// backlog split into dated / Planned / Open stretches (ADR 0010), the history
// window and its page 0 (`historyPageZero`) — the shared `['tasks']` patch, and
// the optimistic cache edits). The reorder tests pin the exact server semantics
// of `POST /api/agenda/items/:id/move`: peers at/after the target rank shift up
// one and the item lands on it; `moveStreamItem` pins the same rule for the
// cross-day `reschedule`. The backlog tests pin ADR 0007 / ADR 0010: a dated
// backlog row reorders with the agenda, an OPEN (or PLANNED) warehouse row
// through the Board's own same-status `reorder_in_place` (the hovered rank in
// both directions, peers shifted), a backlog drop appends the row at today+50y,
// and an unschedule parks a dated PLANNED row on the sentinel with no status
// change while a plan appends it to the Planned stretch.

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
  agendaShortDateLabel,
  appendRankOnStream,
  applyAgendaMove,
  applyOpenTaskReorder,
  canMoveToBacklog,
  canReschedule,
  canUnschedule,
  crossDaySortOrder,
  dedupeAgendaItems,
  eachCivilDate,
  groupItemsByDate,
  historyPageZero,
  isAgendaItemParked,
  isAgendaItemPending,
  isOpenBacklogTask,
  isPlannedBacklogTask,
  moveItemToBacklog,
  moveStreamItem,
  openTaskMoveTargetAt,
  partitionAgendaItems,
  planOpenItem,
  previousHistoryWindow,
  replaceCachedTask,
  rescheduleMaxDate,
  reschedulePickerMonth,
  reschedulePresets,
  splitBacklogSection,
  streamBounds,
  streamItemsOf,
  unscheduleItem,
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

/** The `pageParams` of a cache built in a test: each page's own window, in
 *  page order. The helpers never read them, and `writeAgendaPages` never
 *  replaces the history query's — they are carried along only. */
function storedParams(
  ...windows: { from: string; to: string }[]
): { from: string; to: string }[] {
  return windows;
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

test('agendaShortDateLabel: always absolute, with the year across years', () => {
  assert.equal(agendaShortDateLabel('2026-08-23', '2026-08-23'), 'Sun 23 Aug');
  assert.equal(agendaShortDateLabel('2026-08-24', '2026-08-23'), 'Mon 24 Aug');
  assert.equal(
    agendaShortDateLabel('2027-01-01', '2026-08-23'),
    'Fri 1 Jan 2027',
  );
  assert.equal(agendaShortDateLabel('2026-13-40', '2026-08-23'), '2026-13-40');
});

// ── reschedulePresets ─────────────────────────────────────────────────────

test('reschedulePresets: Tomorrow anchors to the SERVER today, not the row', () => {
  // The old popover used item.local_date + 1: a row three days out moved one
  // day LATER when “Tomorrow” was picked. Every preset now shifts from today.
  const row = { ...taskItem('a', 'PLANNED'), local_date: '2026-08-26' };
  const presets = reschedulePresets(row, TODAY);
  assert.equal(presets.find((p) => p.id === 'today')?.date, '2026-08-23');
  assert.equal(presets.find((p) => p.id === 'tomorrow')?.date, '2026-08-24');
  assert.equal(presets.find((p) => p.id === 'in-a-week')?.date, '2026-08-30');
});

test('reschedulePresets: hides the current date and every duplicate target', () => {
  // On a Monday, "Next Monday" and "In a week" are the same date; the first
  // wins. "Tomorrow" is also next Monday from a Sunday — again, one option.
  const monday = '2026-08-24';
  const mondayRow = { ...taskItem('a', 'PLANNED'), local_date: monday };
  const mondayPresets = reschedulePresets(mondayRow, monday);
  assert.deepEqual(
    mondayPresets.map((p) => [p.id, p.date]),
    [
      ['tomorrow', '2026-08-25'],
      ['next-monday', '2026-08-31'],
    ],
  );

  const sunday = '2026-08-23';
  const sundayRow = { ...taskItem('b', 'PLANNED'), local_date: sunday };
  const sundayPresets = reschedulePresets(sundayRow, sunday);
  assert.deepEqual(
    sundayPresets.map((p) => [p.id, p.date]),
    [
      ['tomorrow', '2026-08-24'],
      ['in-a-week', '2026-08-30'],
    ],
  );
});

test('reschedulePresets: Next Monday is the coming Monday, strictly after today', () => {
  // Wednesday 26 Aug 2026 → the next Monday is 31 Aug (not today, not +7).
  const wednesday = '2026-08-26';
  const row = { ...taskItem('a', 'PLANNED'), local_date: '2026-08-28' };
  const presets = reschedulePresets(row, wednesday);
  assert.deepEqual(
    presets.map((p) => [p.id, p.date]),
    [
      ['today', '2026-08-26'],
      ['tomorrow', '2026-08-27'],
      ['next-monday', '2026-08-31'],
      ['in-a-week', '2026-09-02'],
    ],
  );
});

test('reschedulePresets: a past row gets distinct Today and Tomorrow targets', () => {
  // Yesterday + 1 is today; the old control offered two buttons that both
  // moved to today. Tomorrow must mean today + 1.
  const row = { ...taskItem('a', 'PLANNED'), local_date: '2026-08-22' };
  const presets = reschedulePresets(row, TODAY);
  assert.equal(presets.find((p) => p.id === 'today')?.date, '2026-08-23');
  assert.equal(presets.find((p) => p.id === 'tomorrow')?.date, '2026-08-24');
});

test('reschedulePresets: invalid today yields no options', () => {
  assert.deepEqual(reschedulePresets(taskItem('a', 'PLANNED'), ''), []);
  assert.deepEqual(
    reschedulePresets(taskItem('a', 'PLANNED'), 'not-a-date'),
    [],
  );
});

// ── reschedulePickerMonth ────────────────────────────────────────────────

test('reschedulePickerMonth: in-range rows open on their own month', () => {
  assert.equal(reschedulePickerMonth(TODAY, TODAY), '2026-08');
  assert.equal(reschedulePickerMonth('2026-09-14', TODAY), '2026-09');
  assert.equal(reschedulePickerMonth('2036-08-22', TODAY), '2036-08');
});

test('reschedulePickerMonth: history and backlog rows open on today', () => {
  assert.equal(reschedulePickerMonth('2026-07-01', TODAY), '2026-08');
  // The backlog date is today+50y — far past the picker's exclusive 10y line.
  assert.equal(
    reschedulePickerMonth(addMonthsClamp(TODAY, 600), TODAY),
    '2026-08',
  );
  // Exactly today+10y is already outside the accepted [today, today+10y).
  assert.equal(reschedulePickerMonth('2036-08-23', TODAY), '2026-08');
});

test('reschedulePickerMonth: invalid today yields an empty month', () => {
  assert.equal(reschedulePickerMonth(TODAY, ''), '');
  assert.equal(reschedulePickerMonth(TODAY, 'not-a-date'), '');
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

// ── previousHistoryWindow / historyPageZero ─────────────────────────────

test('previousHistoryWindow: the anchor day alone names the week before it', () => {
  // `historyPageZero` is exactly this: the anchor day alone, as a page.
  const pages = [
    page('2026-08-23', '2026-08-23', [], { anchor_date: '2026-08-23' }),
  ];
  assert.deepEqual(previousHistoryWindow(pages), {
    from: '2026-08-16',
    to: '2026-08-22',
  });
});

test('previousHistoryWindow: the next window steps back from the oldest page', () => {
  // The history cache once page 0 has loaded and grown one week older: the
  // next window is the week before the OLDEST one, never another eve week.
  const pages = [
    page('2026-08-16', '2026-08-22', [], { anchor_date: '2026-08-23' }),
    page('2026-08-09', '2026-08-15', [], { anchor_date: '2026-08-23' }),
  ];
  assert.deepEqual(previousHistoryWindow(pages), {
    from: '2026-08-02',
    to: '2026-08-08',
  });
});

test('historyPageZero: the 7 days ending the anchor eve, floored at earliest', () => {
  assert.deepEqual(historyPageZero('2026-08-23', '2026-08-01'), {
    from: '2026-08-16',
    to: '2026-08-22',
  });
  // The floor clamps the window short instead of crossing it.
  assert.deepEqual(historyPageZero('2026-08-23', '2026-08-19'), {
    from: '2026-08-19',
    to: '2026-08-22',
  });
});

test('historyPageZero: nothing older than the anchor eve means no page 0', () => {
  // Earliest_date at the anchor itself: the eve is already before it, so the
  // history query must stay off rather than ask for an empty week.
  assert.equal(historyPageZero('2026-08-23', '2026-08-23'), undefined);
  // A malformed anchor/earliest is no window either.
  assert.equal(historyPageZero('', '2026-08-01'), undefined);
  assert.equal(historyPageZero('2026-08-23', ''), undefined);
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

// ── isOpenBacklogTask / isPlannedBacklogTask / splitBacklogSection ──────

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

test('isPlannedBacklogTask: only a PLANNED task on/past the 10-year line', () => {
  assert.equal(
    isPlannedBacklogTask(taskItemAt('a', 'PLANNED', BACKLOG_DATE), TODAY),
    true,
  );
  // Exactly the horizon counts (ADR 0010: a valid parking spot).
  assert.equal(
    isPlannedBacklogTask(taskItemAt('b', 'PLANNED', HORIZON), TODAY),
    true,
  );
  // One day under the line is still a dated row in the day stream.
  assert.equal(
    isPlannedBacklogTask(taskItemAt('c', 'PLANNED', '2036-08-22'), TODAY),
    false,
  );
  // The row must sit in the warehouse region AND the task be PLANNED.
  assert.equal(
    isPlannedBacklogTask(taskItemAt('d', 'OPEN', BACKLOG_DATE), TODAY),
    false,
  );
  assert.equal(
    isPlannedBacklogTask(taskItemAt('e', 'IN_PROGRESS', BACKLOG_DATE), TODAY),
    false,
  );
  assert.equal(
    isPlannedBacklogTask(taskItemAt('f', 'COMPLETED', BACKLOG_DATE), TODAY),
    false,
  );
  assert.equal(
    isPlannedBacklogTask(taskItemAt('g', 'DISCARDED', BACKLOG_DATE), TODAY),
    false,
  );
  assert.equal(
    isPlannedBacklogTask(occurrenceItemAt('h', 'pending', BACKLOG_DATE), TODAY),
    false,
  );
  assert.equal(isPlannedBacklogTask(item('orphan', 0), TODAY), false);
  // A missing/malformed today claims nothing.
  assert.equal(
    isPlannedBacklogTask(taskItemAt('i', 'PLANNED', BACKLOG_DATE), ''),
    false,
  );
  assert.equal(
    isPlannedBacklogTask(
      taskItemAt('j', 'PLANNED', BACKLOG_DATE),
      'not-a-date',
    ),
    false,
  );
});

test('splitBacklogSection: three stretches in server order, never re-sorted', () => {
  const dated = taskItemAt('d', 'PLANNED', '2026-08-30', 1);
  const open = taskItemAt('a', 'OPEN', BACKLOG_DATE, 9);
  const open2 = taskItemAt('b', 'OPEN', BACKLOG_DATE, 2);
  // A PLANNED row on the sentinel IS the Planned warehouse (ADR 0010) — no
  // longer a misplacement window waiting for a self-heal.
  const planned = taskItemAt('m', 'PLANNED', BACKLOG_DATE, 5);
  // Neither warehouse: a running task and an occurrence at the horizon are
  // omitted — the server omits them too, and neither reorders.
  const running = taskItemAt('r', 'IN_PROGRESS', BACKLOG_DATE, 0);
  const occurrence = occurrenceItemAt('o', 'pending', BACKLOG_DATE);
  const split = splitBacklogSection(
    [dated, open, open2, planned, running, occurrence],
    TODAY,
  );
  assert.deepEqual(
    split.dated.map((entry) => entry.id),
    ['d'],
  );
  // Server order (Board order) survives: a and b keep the array's order even
  // though b's own rank is lower, and the Planned warehouse is its own list.
  assert.deepEqual(
    split.planned.map((entry) => entry.id),
    ['m'],
  );
  assert.deepEqual(
    split.open.map((entry) => entry.id),
    ['a', 'b'],
  );
});

test('splitBacklogSection: a malformed today yields three empty stretches', () => {
  const split = splitBacklogSection(
    [
      taskItemAt('a', 'OPEN', BACKLOG_DATE),
      taskItemAt('b', 'PLANNED', '2026-08-30'),
    ],
    '',
  );
  assert.deepEqual(split.dated, []);
  assert.deepEqual(split.planned, []);
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
    pageParams: storedParams(
      { from: '2026-08-16', to: '2026-08-22' },
      { from: '2026-08-23', to: '2026-08-29' },
    ),
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
    pageParams: storedParams({ from: '2026-08-23', to: '2026-08-29' }),
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
    pageParams: storedParams({ from: '2026-08-23', to: '2026-08-29' }),
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
    pageParams: storedParams(
      { from: '2026-08-16', to: '2026-08-22' },
      { from: '2026-08-23', to: '2026-08-29' },
    ),
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
    pageParams: storedParams({ from: '2026-08-23', to: '2026-08-29' }),
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
    pageParams: storedParams({ from: '2026-08-23', to: '2026-08-29' }),
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
    pageParams: storedParams({ from: '2026-08-23', to: '2026-08-29' }),
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
    pageParams: storedParams({ from: '2026-08-23', to: '2026-08-29' }),
  };
  // The reschedule path opts in to the one status change: the server plans an
  // OPEN task it reschedules onto a date.
  const next = moveStreamItem(data, 'b', '2026-08-25', 3, true);
  const page0 = next?.pages[0];
  // No copy is left behind in the backlog…
  assert.deepEqual(page0?.backlog, []);
  // …and the row lands on the target day with its rank.
  const landed = page0?.items.find((entry) => entry.id === 'b');
  assert.equal(landed?.local_date, '2026-08-25');
  assert.equal(landed?.sort_order, 3);
  assert.equal(landed?.task?.status, 'PLANNED');
  assert.equal(data.pages[0].backlog.length, 1, 'input not mutated');
});

test('moveStreamItem: only the reschedule path plans an OPEN task', () => {
  // Move-to-today's sweep and a task's own placement change the DATE only —
  // an OPEN row stays OPEN (ADR 0007), so the paint must not flash Planned.
  const data = {
    pages: [
      page('2026-08-23', '2026-08-29', [], {
        backlog: [taskItemAt('b', 'OPEN', BACKLOG_DATE, 0)],
      }),
    ],
    pageParams: storedParams({ from: '2026-08-23', to: '2026-08-29' }),
  };
  const next = moveStreamItem(data, 'b', '2026-08-24', 0);
  const landed = next?.pages[0].items.find((entry) => entry.id === 'b');
  assert.equal(landed?.local_date, '2026-08-24');
  assert.equal(landed?.task?.status, 'OPEN');
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
    pageParams: storedParams({ from: '2026-08-23', to: '2026-08-29' }),
  };
  const afterTask = moveStreamItem(data, 'p', '2026-08-26', 0, true);
  assert.equal(
    afterTask?.pages[0].items.find((entry) => entry.id === 'p')?.task?.status,
    'PLANNED',
  );
  const afterOccurrence = moveStreamItem(data, 'o', '2026-08-27', 1, true);
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
    pageParams: storedParams({ from: '2026-08-23', to: '2026-08-29' }),
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
    pageParams: storedParams({ from: '2026-08-23', to: '2026-08-29' }),
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
    pageParams: storedParams(
      { from: '2026-08-16', to: '2026-08-22' },
      { from: '2026-08-23', to: '2026-08-29' },
    ),
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
    pageParams: storedParams({ from: '2026-08-23', to: '2026-08-29' }),
  };
  assert.equal(moveItemToBacklog(data, 'missing', BACKLOG_DATE), data);
  assert.equal(moveItemToBacklog(undefined, 'a', BACKLOG_DATE), undefined);
  const empty = { pages: [], pageParams: [] };
  assert.equal(moveItemToBacklog(empty, 'a', BACKLOG_DATE), empty);
});

// ── unscheduleItem / planOpenItem (ADR 0010) ────────────────────────────

test('unscheduleItem: a dated PLANNED row parks in the Planned stretch in Board order', () => {
  // The mover holds Planned rank 3; the existing planned peer holds 1 and an
  // OPEN sentinel row holds 0. The rebuilt array is dated, then Planned in
  // Board order (peer before mover), then Open — the mover's own agenda rank
  // never decides its place.
  const dated = taskItemAt('p', 'PLANNED', '2026-08-25', 3);
  // Make the agenda rank differ from the Board rank so the assertions can
  // tell "no agenda churn" from "no Board re-rank".
  dated.sort_order = 7;
  const plannedPeer = taskItemAt('a', 'PLANNED', BACKLOG_DATE, 1);
  const openPeer = taskItemAt('b', 'OPEN', BACKLOG_DATE, 0);
  const data = {
    pages: [
      page('2026-08-23', '2026-08-29', [dated, item('x', 0)], {
        backlog: [plannedPeer, openPeer],
      }),
    ],
    pageParams: storedParams({ from: '2026-08-23', to: '2026-08-29' }),
  };
  const next = unscheduleItem(data, 'p', BACKLOG_DATE, TODAY);
  const page0 = next?.pages[0];
  assert.deepEqual(
    page0?.items.map((entry) => entry.id),
    ['x'],
    'the row leaves its day',
  );
  assert.deepEqual(
    page0?.backlog.map((entry) => entry.id),
    ['a', 'p', 'b'],
  );
  const moved = page0?.backlog[1];
  assert.equal(moved?.local_date, BACKLOG_DATE);
  assert.equal(moved?.task?.status, 'PLANNED', 'no status change');
  assert.equal(moved?.task?.sort_order, 3, 'no Board re-rank');
  assert.equal(moved?.sort_order, 7, 'the agenda rank is untouched too');
  // The input cache is untouched.
  assert.equal(data.pages[0].items.length, 2);
  assert.equal(data.pages[0].backlog.length, 2);
});

test('unscheduleItem: a missing id (or no data) keeps the same reference', () => {
  const data = {
    pages: [page('2026-08-23', '2026-08-29', [item('a', 0)])],
    pageParams: storedParams({ from: '2026-08-23', to: '2026-08-29' }),
  };
  assert.equal(unscheduleItem(data, 'missing', BACKLOG_DATE, TODAY), data);
  assert.equal(unscheduleItem(undefined, 'a', BACKLOG_DATE, TODAY), undefined);
});

test('unscheduleItem: a row outside both warehouses survives the rebuild', () => {
  // A stale optimistic IN_PROGRESS row at the horizon is not a warehouse row
  // and is not dated, but the rebuild must not lose it.
  const dated = taskItemAt('p', 'PLANNED', '2026-08-25', 3);
  const stale = taskItemAt('r', 'IN_PROGRESS', HORIZON, 0);
  const data = {
    pages: [page('2026-08-23', '2026-08-29', [dated], { backlog: [stale] })],
    pageParams: storedParams({ from: '2026-08-23', to: '2026-08-29' }),
  };
  const next = unscheduleItem(data, 'p', BACKLOG_DATE, TODAY);
  assert.deepEqual(
    next?.pages[0].backlog.map((entry) => entry.id),
    ['r', 'p'],
    'the stale row stays, the mover joins the Planned stretch',
  );
});

test('planOpenItem: a dated OPEN row appends to Planned and leaves its day', () => {
  const dated = taskItemAt('o', 'OPEN', '2026-08-25', 2);
  dated.sort_order = 6;
  // The Planned column already holds rank 4 → the mover appends at max + 1,
  // never at 0.
  const plannedPeer = taskItemAt('p', 'PLANNED', BACKLOG_DATE, 4);
  const openPeer = taskItemAt('b', 'OPEN', BACKLOG_DATE, 0);
  const data = {
    pages: [
      page('2026-08-23', '2026-08-29', [dated, item('x', 0)], {
        backlog: [plannedPeer, openPeer],
      }),
    ],
    pageParams: storedParams({ from: '2026-08-23', to: '2026-08-29' }),
  };
  const next = planOpenItem(data, 'o', BACKLOG_DATE, TODAY);
  const page0 = next?.pages[0];
  assert.deepEqual(
    page0?.items.map((entry) => entry.id),
    ['x'],
    'a dated row leaves the day stream for the warehouse',
  );
  assert.deepEqual(
    page0?.backlog.map((entry) => entry.id),
    ['p', 'o', 'b'],
    'end of the Planned stretch, before Open',
  );
  const moved = page0?.backlog[1];
  assert.equal(moved?.local_date, BACKLOG_DATE, 'the sentinel, never today');
  assert.equal(moved?.task?.status, 'PLANNED');
  assert.equal(moved?.task?.sort_order, 5, 'append = max(4) + 1');
  assert.equal(moved?.sort_order, 6, 'the agenda rank is untouched');
  assert.equal(data.pages[0].items.length, 2, 'input not mutated');
});

test('planOpenItem: an OPEN warehouse row keeps its own sentinel and still appends', () => {
  // Already at/after the horizon: the server leaves the row's date alone, so
  // the paint must not retarget it onto today's exact sentinel.
  const sentinel = taskItemAt('s', 'OPEN', '2040-01-01', 1);
  sentinel.sort_order = 8;
  const plannedPeer = taskItemAt('p', 'PLANNED', BACKLOG_DATE, 9);
  const data = {
    pages: [
      page('2026-08-23', '2026-08-29', [], {
        backlog: [sentinel, plannedPeer],
      }),
    ],
    pageParams: storedParams({ from: '2026-08-23', to: '2026-08-29' }),
  };
  const next = planOpenItem(data, 's', BACKLOG_DATE, TODAY);
  const page0 = next?.pages[0];
  assert.deepEqual(
    page0?.backlog.map((entry) => entry.id),
    ['p', 's'],
  );
  const moved = page0?.backlog[1];
  assert.equal(moved?.local_date, '2040-01-01', 'not retargeted');
  assert.equal(moved?.task?.status, 'PLANNED');
  assert.equal(moved?.task?.sort_order, 10, 'append = max(9) + 1');
  assert.equal(moved?.sort_order, 8, 'the agenda rank is untouched');
});

test('planOpenItem: only an OPEN task is planned; a missing id keeps the reference', () => {
  const data = {
    pages: [
      page('2026-08-23', '2026-08-29', [
        taskItemAt('p', 'PLANNED', '2026-08-25', 0),
        item('orphan', 0),
      ]),
    ],
    pageParams: storedParams({ from: '2026-08-23', to: '2026-08-29' }),
  };
  assert.equal(planOpenItem(data, 'p', BACKLOG_DATE, TODAY), data);
  assert.equal(planOpenItem(data, 'orphan', BACKLOG_DATE, TODAY), data);
  assert.equal(planOpenItem(data, 'missing', BACKLOG_DATE, TODAY), data);
  assert.equal(planOpenItem(undefined, 'a', BACKLOG_DATE, TODAY), undefined);
});

test('planOpenItem: a row outside both warehouses survives the rebuild', () => {
  const dated = taskItemAt('o', 'OPEN', '2026-08-25', 2);
  const stale = taskItemAt('r', 'IN_PROGRESS', HORIZON, 0);
  const data = {
    pages: [page('2026-08-23', '2026-08-29', [dated], { backlog: [stale] })],
    pageParams: storedParams({ from: '2026-08-23', to: '2026-08-29' }),
  };
  const next = planOpenItem(data, 'o', BACKLOG_DATE, TODAY);
  assert.deepEqual(
    next?.pages[0].backlog.map((entry) => entry.id),
    ['r', 'o'],
    'the stale row stays, the plan joins the Planned stretch',
  );
});

// ── appendRankOnStream / streamItemsOf / dedupeAgendaItems ──────────────

test('appendRankOnStream: max + 1 on the date, 0 on an empty pile', () => {
  const data = {
    pages: [
      page('2026-08-16', '2026-08-22', [item('a', 4, '2026-08-20')]),
      page('2026-08-23', '2026-08-29', [item('b', 0), item('c', 2)]),
    ],
    pageParams: storedParams(
      { from: '2026-08-16', to: '2026-08-22' },
      { from: '2026-08-23', to: '2026-08-29' },
    ),
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
    pageParams: storedParams(
      { from: '2026-08-16', to: '2026-08-22' },
      { from: '2026-08-23', to: '2026-08-29' },
    ),
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
    pageParams: storedParams(
      { from: '2026-08-16', to: '2026-08-22' },
      { from: '2026-08-23', to: '2026-08-29' },
    ),
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
    pageParams: storedParams(
      { from: '2026-08-16', to: '2026-08-22' },
      { from: '2026-08-23', to: '2026-08-29' },
    ),
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

// ── openTaskMoveTargetAt / applyOpenTaskReorder ─────────────────────────

test('openTaskMoveTargetAt: the hovered row\u2019s Board rank, both directions', () => {
  // The agenda row's own `sort_order` is irrelevant in the OPEN stretch; the
  // Board's `tasks.sort_order` is the one order Home and Board share. A
  // same-status `/move` is `reorder_in_place`, which lands the mover ON the
  // hovered rank and shifts that row away — no `+ 1` in either direction.
  const items = [
    taskItemAt('a', 'OPEN', BACKLOG_DATE, 0),
    taskItemAt('b', 'OPEN', BACKLOG_DATE, 7),
    taskItemAt('c', 'OPEN', BACKLOG_DATE, 9),
  ];
  // c (index 2) dropped above a (index 0) takes a's Board rank.
  assert.equal(openTaskMoveTargetAt(items, 2, 0), 0);
  // a dropped below b takes b's rank (which then shifts to 1).
  assert.equal(openTaskMoveTargetAt(items, 0, 1), 7);
  // a dropped below c takes c's rank (which then shifts to 10).
  assert.equal(openTaskMoveTargetAt(items, 0, 2), 9);
  // b dropped below c: same rule downward.
  assert.equal(openTaskMoveTargetAt(items, 1, 2), 9);
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

test('applyOpenTaskReorder: a downward drop shifts the peers it passes down', () => {
  // ranks [0, 1, 2], drag index 0 onto index 1 → the sent rank is 1 and the
  // result is A=1, B=0, C=2 (the order [B, A, C]).
  const tasks = [task('a', 'OPEN'), task('b', 'OPEN'), task('c', 'OPEN')].map(
    (entry, index) => ({ ...entry, sort_order: index }),
  );
  const moved = applyOpenTaskReorder(tasks, 'a', 1);
  assert.deepEqual(
    moved.map((entry) => [entry.id, entry.sort_order]),
    [
      ['a', 1],
      ['b', 0],
      ['c', 2],
    ],
  );
});

test('applyOpenTaskReorder: an upward drop shifts the peers it passes up', () => {
  const tasks = [task('a', 'OPEN'), task('b', 'OPEN'), task('c', 'OPEN')].map(
    (entry, index) => ({ ...entry, sort_order: index }),
  );
  // c (2) onto a's slot (0): a and b shift up, c lands on 0.
  const moved = applyOpenTaskReorder(tasks, 'c', 0);
  assert.deepEqual(
    moved.map((entry) => [entry.id, entry.sort_order]),
    [
      ['a', 1],
      ['b', 2],
      ['c', 0],
    ],
  );
});

test('applyOpenTaskReorder: rank gaps shift only the crossed range', () => {
  // 0, 5, 9 — drag the 0 onto the 9: peers become 4 and 8, the mover 9.
  const tasks = [task('a', 'OPEN'), task('b', 'OPEN'), task('c', 'OPEN')].map(
    (entry, index) => ({ ...entry, sort_order: [0, 5, 9][index] }),
  );
  const moved = applyOpenTaskReorder(tasks, 'a', 9);
  assert.deepEqual(
    moved.map((entry) => [entry.id, entry.sort_order]),
    [
      ['a', 9],
      ['b', 4],
      ['c', 8],
    ],
  );
});

test('applyOpenTaskReorder: other statuses keep their ranks; no-op refs', () => {
  const tasks = [
    { ...task('a', 'OPEN'), sort_order: 0 },
    { ...task('p', 'PLANNED'), sort_order: 1 },
    { ...task('b', 'OPEN'), sort_order: 4 },
  ];
  const moved = applyOpenTaskReorder(tasks, 'a', 4);
  assert.deepEqual(
    moved.map((entry) => [entry.id, entry.sort_order]),
    [
      ['a', 4],
      ['p', 1],
      ['b', 3],
    ],
    'the PLANNED task is not part of the Open pile',
  );
  // `new === old` writes nothing → the very same array.
  assert.equal(applyOpenTaskReorder(tasks, 'a', 0), tasks);
  // A missing mover leaves the list alone too.
  assert.equal(applyOpenTaskReorder(tasks, 'missing', 2), tasks);
});

// ── replaceCachedTask ───────────────────────────────────────────────────

test('replaceCachedTask: the fresh row lands with its status and Board rank', () => {
  // A dated reschedule flips OPEN → PLANNED and appends the Planned column —
  // the shared `['tasks']` cache must follow BOTH.
  const open = { ...task('a', 'OPEN'), sort_order: 4 };
  const planned = { ...task('b', 'PLANNED'), sort_order: 0 };
  const fresh: TaskRecord = { ...task('a', 'PLANNED'), sort_order: 3 };
  const tasks = [open, planned];
  const next = replaceCachedTask(tasks, fresh);
  assert.deepEqual(
    next.map((entry) => [entry.id, entry.status, entry.sort_order]),
    [
      ['a', 'PLANNED', 3],
      ['b', 'PLANNED', 0],
    ],
  );
  assert.equal(next[0], fresh, 'the server row itself is cached');
  assert.equal(next[1], planned, 'every other row keeps its reference');
  assert.equal(tasks[0], open, 'the input array is not mutated');
});

test('replaceCachedTask: nothing to replace keeps the same reference', () => {
  const tasks = [task('a', 'OPEN'), task('b', 'PLANNED')];
  // No fresh row (an occurrence, a deleted task) or an id the cache does not
  // hold: React Query must be able to skip the re-render.
  assert.equal(replaceCachedTask(tasks, null), tasks);
  assert.equal(replaceCachedTask(tasks, undefined), tasks);
  assert.equal(replaceCachedTask(tasks, task('missing', 'OPEN')), tasks);
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

// ── canMoveToBacklog ─────────────────────────────────────────────────────

test('canMoveToBacklog: living tasks only, occurrences never', () => {
  assert.equal(canMoveToBacklog(taskItem('a', 'OPEN'), TODAY), true);
  assert.equal(canMoveToBacklog(taskItem('b', 'PLANNED'), TODAY), true);
  assert.equal(canMoveToBacklog(taskItem('c', 'IN_PROGRESS'), TODAY), false);
  assert.equal(canMoveToBacklog(taskItem('d', 'COMPLETED'), TODAY), false);
  assert.equal(canMoveToBacklog(taskItem('e', 'DISCARDED'), TODAY), false);
  assert.equal(canMoveToBacklog(occurrenceItem('f', 'pending'), TODAY), false);
  assert.equal(canMoveToBacklog(item('orphan', 0), TODAY), false);
});

test('canMoveToBacklog: a task already on the backlog date is not offered again', () => {
  const backlogDate = addMonthsClamp(TODAY, 600);
  const already = { ...taskItem('a', 'OPEN'), local_date: backlogDate };
  assert.equal(canMoveToBacklog(already, TODAY), false);
  // An OPEN row on a near date has not reached the backlog yet.
  const near = { ...taskItem('b', 'OPEN'), local_date: '2026-08-25' };
  assert.equal(canMoveToBacklog(near, TODAY), true);
});

test('canMoveToBacklog: a PLANNED warehouse row is still offered Backlog', () => {
  // ADR 0010: the drop unplans it (`PLANNED → OPEN`, Open-column append), so
  // it is a real transition even though the row stays on the sentinel.
  const planned = { ...taskItem('a', 'PLANNED'), local_date: BACKLOG_DATE };
  assert.equal(canMoveToBacklog(planned, TODAY), true);
  const plannedAtHorizon = { ...taskItem('b', 'PLANNED'), local_date: HORIZON };
  assert.equal(canMoveToBacklog(plannedAtHorizon, TODAY), true);
});

test('canMoveToBacklog: invalid server today hides the destination', () => {
  assert.equal(canMoveToBacklog(taskItem('a', 'PLANNED'), ''), false);
  assert.equal(canMoveToBacklog(taskItem('a', 'PLANNED'), 'not-a-date'), false);
});

// ── canUnschedule ───────────────────────────────────────────────────────

test('canUnschedule: a dated PLANNED task, past dates included', () => {
  assert.equal(canUnschedule(taskItem('a', 'PLANNED'), TODAY), true);
  const yesterday = { ...taskItem('b', 'PLANNED'), local_date: '2026-08-22' };
  assert.equal(canUnschedule(yesterday, TODAY), true);
  const past = { ...taskItem('c', 'PLANNED'), local_date: '2020-01-01' };
  assert.equal(canUnschedule(past, TODAY), true);
  // One day under the 10-year line is still a dated row.
  const lastDated = { ...taskItem('d', 'PLANNED'), local_date: '2036-08-22' };
  assert.equal(canUnschedule(lastDated, TODAY), true);
  const datedBacklog = {
    ...taskItem('e', 'PLANNED'),
    local_date: '2026-09-30',
  };
  assert.equal(canUnschedule(datedBacklog, TODAY), true);
});

test('canUnschedule: the horizon and the sentinel are already warehoused', () => {
  const atHorizon = { ...taskItem('a', 'PLANNED'), local_date: HORIZON };
  assert.equal(canUnschedule(atHorizon, TODAY), false);
  const sentinel = { ...taskItem('b', 'PLANNED'), local_date: BACKLOG_DATE };
  assert.equal(canUnschedule(sentinel, TODAY), false);
});

test('canUnschedule: only a PLANNED task — everything else is rejected', () => {
  // An OPEN row has its own destination (plan / backlog); the API 400s the
  // rest.
  assert.equal(canUnschedule(taskItem('a', 'OPEN'), TODAY), false);
  const openNear = { ...taskItem('b', 'OPEN'), local_date: '2026-08-25' };
  assert.equal(canUnschedule(openNear, TODAY), false);
  assert.equal(canUnschedule(taskItem('c', 'IN_PROGRESS'), TODAY), false);
  assert.equal(canUnschedule(taskItem('d', 'COMPLETED'), TODAY), false);
  assert.equal(canUnschedule(taskItem('e', 'DISCARDED'), TODAY), false);
  assert.equal(canUnschedule(occurrenceItem('f', 'pending'), TODAY), false);
  assert.equal(canUnschedule(item('orphan', 0), TODAY), false);
  // A missing/malformed today offers nothing.
  assert.equal(canUnschedule(taskItem('g', 'PLANNED'), ''), false);
  assert.equal(canUnschedule(taskItem('h', 'PLANNED'), 'not-a-date'), false);
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
