// Pure helpers for Home's agenda stream (ADR 0004 amendment — slice 5, ADR
// 0007 — slice 6, ADR 0010 — slice 7): the day label, the move presets /
// picker bounds, the reorder rank math, the living / Completed dump split, the
// backlog split into its dated / Planned / Open stretches, the date-window
// walk, and the optimistic edits over React Query's infinite `{ pages }` cache.
// Kept React-free so `node:test` can cover them (HomePage.tsx is presentational
// glue on top).

import type { InfiniteData } from '@tanstack/react-query';
import type {
  AgendaItemRecord,
  AgendaResponse,
  TaskRecord,
  TaskStatus,
} from '../../types';
import { addCivilDays, isCivilDateValid } from '../routines/rrule-preview';

/** Every day's drop container id (`day-2026-08-23`) — also the day section's
 *  DOM id, which is how the initial scroll and the Today button find a day. */
export const DAY_ID_PREFIX = 'day-';
/** One day's Waiting section droppable id (`waiting-day-2026-08-23`). It is
 *  separate from the day's main pile: a drop here is a hand-off (normal row →
 *  WAITING) or a waiting in-section move, never a normal day reschedule. */
export const WAITING_DAY_ID_PREFIX = 'waiting-day-';
/** The Backlog section's droppable id — a constant, never a `day-` id. */
export const BACKLOG_DROPPABLE_ID = 'backlog';
/** The Planned section's droppable id — a constant, never a `day-` id and
 *  never the sentinel date: a day header at 2076 would be a future droppable
 *  and the API would 400 the reschedule it implies. */
export const PLANNED_DROPPABLE_ID = 'planned';
/** A dated stretch group inside the Backlog (past today+6 … today+10y−1). */
export const BACKLOG_DAY_ID_PREFIX = 'backlog-day-';

const WEEKDAY_SHORT = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
const MONTH_SHORT = [
  'Jan',
  'Feb',
  'Mar',
  'Apr',
  'May',
  'Jun',
  'Jul',
  'Aug',
  'Sep',
  'Oct',
  'Nov',
  'Dec',
];

function pad(n: number): string {
  return String(n).padStart(2, '0');
}

/** The absolute day label: "Tue 25 Aug", plus the year ("Fri 1 Jan 2027")
 *  when it differs from `today`'s year. Parsed with the same UTC-carrier trick
 *  as `rrule-preview`. Malformed input returns the input unchanged. */
function absoluteDateLabel(date: string, today: string): string {
  const m = date.trim().match(/^(\d{4})-(\d{2})-(\d{2})$/);
  if (!m) return date;
  // Component round-trip rejects rollovers (month 13, day 40) — same strict
  // gate as `isCivilDateValid`.
  const carrier = new Date(
    Date.UTC(Number(m[1]), Number(m[2]) - 1, Number(m[3])),
  );
  const y = m[1];
  const mo = m[2];
  const d = m[3];
  if (
    `${carrier.getUTCFullYear()}` !== y ||
    pad(carrier.getUTCMonth() + 1) !== mo ||
    pad(carrier.getUTCDate()) !== d
  ) {
    return date;
  }
  const label = `${WEEKDAY_SHORT[carrier.getUTCDay()]} ${Number(d)} ${MONTH_SHORT[Number(mo) - 1]}`;
  return y === today.trim().slice(0, 4) ? label : `${label} ${y}`;
}

/** Day-header title for one civil date: "Today" / "Tomorrow" / "Yesterday"
 *  relative to `today`, otherwise the absolute "Tue 25 Aug" (with a year when
 *  it differs). Malformed input returns the input unchanged. */
export function agendaDateLabel(date: string, today: string): string {
  if (date === today) return 'Today';
  if (date === addCivilDays(today, 1)) return 'Tomorrow';
  if (date === addCivilDays(today, -1)) return 'Yesterday';
  return absoluteDateLabel(date, today);
}

/** The absolute day label ("Sun 23 Aug", or "Fri 1 Jan 2027" across years) for
 *  a move preset's subtitle. Relative labels stay in `agendaDateLabel`; this
 *  one never says Today/Tomorrow, so a preset's target is never ambiguous. */
export function agendaShortDateLabel(date: string, today: string): string {
  return absoluteDateLabel(date, today);
}

/** Every civil date from `from` through `to`, inclusive and ascending — the
 *  stream's day spine, which must have no gaps even when a day has no items.
 *  Empty when either bound is malformed or `to < from`. */
export function eachCivilDate(from: string, to: string): string[] {
  const start = from.trim();
  const end = to.trim();
  if (!isCivilDateValid(start) || !isCivilDateValid(end) || end < start) {
    return [];
  }
  const dates: string[] = [];
  for (let date = start; date <= end; date = addCivilDays(date, 1)) {
    dates.push(date);
  }
  return dates;
}

/** `date` shifted by whole months, CLAMPING the day exactly like chrono's
 *  `checked_add_months` (ADR 0007): 2024-02-29 + 120 months = 2034-02-28, not
 *  JS `setUTCMonth`'s roll into March. The server computes the backlog date
 *  (today+600) and the 10-year line (today+120) this way, so the client must
 *  match it or the two disagree on Feb 29. Malformed input returns the input
 *  unchanged. */
export function addMonthsClamp(date: string, months: number): string {
  const trimmed = date.trim();
  if (!isCivilDateValid(trimmed)) return date;
  const [y, m, d] = trimmed.split('-').map(Number);
  // Total months from year 0, so a negative offset floors correctly.
  const total = y * 12 + (m - 1) + months;
  const year = Math.floor(total / 12);
  const month = ((total % 12) + 12) % 12; // 0-based
  const lastDay = new Date(Date.UTC(year, month + 1, 0)).getUTCDate();
  const day = Math.min(d, lastDay);
  return `${String(year).padStart(4, '0')}-${pad(month + 1)}-${pad(day)}`;
}

/** The date picker's upper bound: today + 10 years − 1 day, so the last day
 *  the API accepts for a reschedule (`target < today+10y`) is selectable. Built
 *  from the SERVER's civil today with the same month clamp as the API. */
export function rescheduleMaxDate(today: string): string {
  return addCivilDays(addMonthsClamp(today, 120), -1);
}

/** Sunday = 0 … Saturday = 6 for a civil date, or null when malformed. The
 *  UTC carrier keeps the weekday independent of the browser's timezone. */
function civilDayOfWeek(date: string): number | null {
  const trimmed = date.trim();
  if (!isCivilDateValid(trimmed)) return null;
  const [y, m, d] = trimmed.split('-').map(Number);
  return new Date(Date.UTC(y, m - 1, d)).getUTCDay();
}

/** The coming Monday, STRICTLY after `date` (a Monday maps to +7 days; a
 *  Sunday maps to +1). Malformed input returns the input unchanged. */
function nextMondayAfter(date: string): string {
  const day = civilDayOfWeek(date);
  if (day === null) return date;
  const delta = (1 - day + 7) % 7 || 7;
  return addCivilDays(date, delta);
}

/** The `YYYY-MM` month the move picker opens on: the row's month when its
 *  date is inside the API's `[today, today+10y)` range, otherwise today's
 *  month. A backlog row sits at today+50y and a history row is in the past;
 *  opening either on the row's own month would strand the user far from a
 *  selectable day. Invalid today yields an empty month. */
export function reschedulePickerMonth(itemDate: string, today: string): string {
  if (!isCivilDateValid(today)) return '';
  const max = rescheduleMaxDate(today);
  const seed = itemDate >= today && itemDate <= max ? itemDate : today;
  return seed.slice(0, 7);
}

export interface ReschedulePreset {
  id: 'today' | 'tomorrow' | 'in-a-week' | 'next-monday';
  label: string;
  /** Target civil date, already shifted from the SERVER's today. */
  date: string;
}

/** Quick-move destinations for the agenda row menu, anchored to the SERVER's
 *  civil today — never the row's own date. In particular "Tomorrow" is
 *  today + 1; a row dated three days out must not move one day later when the
 *  user picks it. Options that would be a no-op (the row's current date) or
 *  duplicate a previous option (Next Monday can equal Tomorrow or In a week)
 *  are omitted. Invalid/missing today yields no presets. */
export function reschedulePresets(
  item: AgendaItemRecord,
  today: string,
): ReschedulePreset[] {
  if (!isCivilDateValid(today)) return [];
  const candidates: ReschedulePreset[] = [
    { id: 'today', label: 'Today', date: today },
    {
      id: 'tomorrow',
      label: 'Tomorrow',
      date: addCivilDays(today, 1),
    },
    {
      id: 'next-monday',
      label: 'Next Monday',
      date: nextMondayAfter(today),
    },
    {
      id: 'in-a-week',
      label: 'In a week',
      date: addCivilDays(today, 7),
    },
  ];
  const seen = new Set<string>([item.local_date]);
  return candidates.filter((candidate) => {
    if (candidate.date < today || seen.has(candidate.date)) return false;
    seen.add(candidate.date);
    return true;
  });
}

/** The loaded span of the stream: the FIRST page's `from` through the LAST
 *  page's `to`, or null when nothing is loaded. `pages` is in infinite-query
 *  order — previous pages are prepended, so index 0 is always the earliest. */
export function streamBounds(
  pages: AgendaResponse[],
): { from: string; to: string } | null {
  const first = pages[0];
  const last = pages[pages.length - 1];
  if (!first || !last) return null;
  return { from: first.from, to: last.to };
}

/** The next history window to request (ADR 0007): the default read ends at
 *  `today+6` and every history page sits strictly before the anchor, so this
 *  is the ONE read that pages BACK. The window is 7 inclusive days ending the
 *  day before the oldest loaded date, capped so `from` never falls before
 *  `earliest_date`, and `undefined` says there is nothing older to ask for.
 *  A gap between the oldest loaded page and the anchor is filled FIRST: the
 *  window then ends on the anchor's eve.
 *  The result always has `to < anchor_date`: the server no longer rejects a
 *  window that reaches the anchor (it clamps `to` to the anchor's eve), but a
 *  history page there would overlap the default read. */
export function previousHistoryWindow(
  pages: {
    from: string;
    to: string;
    anchor_date: string;
    earliest_date: string;
  }[],
): { from: string; to: string } | undefined {
  const first = pages[0];
  if (!first) return undefined;
  const anchor = first.anchor_date;
  const earliest = first.earliest_date;
  if (
    !isCivilDateValid(anchor) ||
    !isCivilDateValid(earliest) ||
    !isCivilDateValid(first.from)
  ) {
    return undefined;
  }
  const eve = addCivilDays(anchor, -1);
  if (eve < earliest) return undefined;
  let to: string;
  if (!pages.some((page) => page.from <= eve && eve <= page.to)) {
    to = eve;
  } else {
    const oldestFrom = pages.reduce(
      (min, page) => (page.from < min ? page.from : min),
      first.from,
    );
    to = addCivilDays(oldestFrom, -1);
    if (to < earliest) return undefined;
  }
  let from = addCivilDays(to, -6);
  if (from < earliest) from = earliest;
  if (from > to) return undefined;
  return { from, to };
}

/** History's page 0 for an anchor: the 7-day window ending the day before
 *  `anchor`, floored at `earliest` — exactly the window
 *  `previousHistoryWindow` names for a cache that holds nothing but the
 *  anchor day. `undefined` when nothing is older than the anchor (`earliest`
 *  is at or past its eve): there is no page 0, so Home never opens the history
 *  query at all. The window ends on the anchor's eve, so it ABUTS the default
 *  read and `getNextPageParam` only ever steps further back. */
export function historyPageZero(
  anchor: string,
  earliest: string,
): { from: string; to: string } | undefined {
  return previousHistoryWindow([
    { from: anchor, to: anchor, anchor_date: anchor, earliest_date: earliest },
  ]);
}

/** All loaded items bucketed by their civil `local_date`, each bucket in the
 *  order the stream returned it (which is already `sort_order` asc per day). */
export function groupItemsByDate(
  items: AgendaItemRecord[],
): Map<string, AgendaItemRecord[]> {
  const byDate = new Map<string, AgendaItemRecord[]>();
  for (const item of items) {
    const bucket = byDate.get(item.local_date);
    if (bucket) bucket.push(item);
    else byDate.set(item.local_date, [item]);
  }
  return byDate;
}

/** The living task statuses (ADR 0004 § Agenda rules, ADR 0011): terminal
 *  tasks (COMPLETED | DISCARDED) are finished; `WAITING` is living work the
 *  date is holding while an agent runs. */
const AGENDA_LIVING_STATUSES: TaskStatus[] = [
  'OPEN',
  'PLANNED',
  'IN_PROGRESS',
  'WAITING',
];

/** True when the row is part of the Backlog's OPEN stretch (ADR 0007): an
 *  `OPEN` task whose row sits on/past the 10-year line. Those rows are ordered
 *  by the Board's `tasks.sort_order`, not by a day pile, and reorder through
 *  the Board's task `/move`. A malformed/absent `today` claims nothing. */
export function isOpenBacklogTask(
  item: AgendaItemRecord,
  today: string,
): boolean {
  if (!today || !isCivilDateValid(today)) return false;
  if (item.kind !== 'task' || !item.task) return false;
  return (
    item.task.status === 'OPEN' && item.local_date >= addMonthsClamp(today, 120)
  );
}

/** True when the row is part of the **Planned** warehouse (ADR 0010): a
 *  `PLANNED` task whose row sits on/past the 10-year line — the mirror of
 *  `isOpenBacklogTask` one status over. Those rows are ordered by the Board's
 *  `tasks.sort_order` (the Planned column), never by the agenda's
 *  `sort_order`, and a reorder is the Board's same-status task `/move`. A
 *  malformed/absent `today` claims nothing. */
export function isPlannedBacklogTask(
  item: AgendaItemRecord,
  today: string,
): boolean {
  if (!today || !isCivilDateValid(today)) return false;
  if (item.kind !== 'task' || !item.task) return false;
  return (
    item.task.status === 'PLANNED' &&
    item.local_date >= addMonthsClamp(today, 120)
  );
}

/** Splits the server's `backlog` array into its THREE stretches (ADR 0007,
 *  ADR 0010) WITHOUT re-sorting either warehouse: `dated` is `local_date`
 *  before the 10-year line (already `local_date`, `sort_order`, `created_at`
 *  asc), `planned` is the rest that reads as a `PLANNED` task in Planned-column
 *  Board order, and `open` is the rest that reads as an `OPEN` task in
 *  Open-column Board order. Anything left (an `IN_PROGRESS` / terminal /
 *  embedless row at the horizon) is omitted — the server omits it too, and it
 *  is not a reorder target. A malformed `today` yields three empty stretches.
 *  Both warehouses keep the server's encounter order: the read is already in
 *  Board order (ties by task id), and client-sorting would silently disagree
 *  with the Board. */
export function splitBacklogSection(
  items: AgendaItemRecord[],
  today: string,
): {
  dated: AgendaItemRecord[];
  planned: AgendaItemRecord[];
  open: AgendaItemRecord[];
} {
  const horizon =
    today && isCivilDateValid(today) ? addMonthsClamp(today, 120) : '';
  const dated: AgendaItemRecord[] = [];
  const planned: AgendaItemRecord[] = [];
  const open: AgendaItemRecord[] = [];
  for (const item of items) {
    if (horizon && item.local_date < horizon) {
      dated.push(item);
    } else if (isPlannedBacklogTask(item, today)) {
      planned.push(item);
    } else if (isOpenBacklogTask(item, today)) {
      open.push(item);
    }
  }
  return { dated, planned, open };
}

/** The three stretches as the optimistic edits REBUILD a `backlog` array
 *  (ADR 0010): same predicates as `splitBacklogSection`, except that a row
 *  belonging to neither warehouse pile (a stale `IN_PROGRESS` / terminal /
 *  embedless row at the horizon — the server omits those) rides along with the
 *  dated stretch instead of being dropped, so a rebuild never loses a row. */
export function backlogStretchesForRebuild(
  items: AgendaItemRecord[],
  today: string,
): {
  dated: AgendaItemRecord[];
  planned: AgendaItemRecord[];
  open: AgendaItemRecord[];
} {
  const split = splitBacklogSection(items, today);
  const kept = new Set(
    [...split.dated, ...split.planned, ...split.open].map((entry) => entry.id),
  );
  return {
    dated: [...split.dated, ...items.filter((entry) => !kept.has(entry.id))],
    planned: split.planned,
    open: split.open,
  };
}

/** The Planned column's Board order (ADR 0010): `tasks.sort_order` asc, ties
 *  by task id — the same order the read applies server-side. */
function byPlannedBoardOrder(a: AgendaItemRecord, b: AgendaItemRecord): number {
  const rank = (a.task?.sort_order ?? 0) - (b.task?.sort_order ?? 0);
  if (rank !== 0) return rank;
  return (a.task?.id ?? a.id).localeCompare(b.task?.id ?? b.id);
}

/** Work that is still ahead of the user on its day — the amber "N pending"
 *  count and the "Move all to today" gate. Tasks count while living; an
 *  occurrence counts while `pending | in_progress` AND its routine is alive
 *  (a deleted routine's history is read-only). Done / skipped rows, terminal
 *  tasks, `routine_deleted` rows and orphans do not count. */
export function isAgendaItemPending(item: AgendaItemRecord): boolean {
  if (item.kind === 'task' && item.task) {
    return AGENDA_LIVING_STATUSES.includes(item.task.status);
  }
  if (item.kind === 'occurrence' && item.occurrence) {
    if (item.occurrence.routine_deleted) return false;
    return (
      item.occurrence.status === 'pending' ||
      item.occurrence.status === 'in_progress'
    );
  }
  return false;
}

/** Can a drag grab this row? Living work only: open tasks (OPEN | PLANNED |
 *  IN_PROGRESS | WAITING) and waiting/running occurrences of a LIVING routine.
 *  A skipped occurrence is a decline (no grip), terminal tasks and
 *  `routine_deleted` history are not draggable at all, and orphans have
 *  nothing to drag. A `WAITING` row is draggable too (ADR 0011): inside its
 *  day's Waiting section it reorders, and across sections it is the hand-off /
 *  take-back gesture. */
export function isAgendaRowDraggable(item: AgendaItemRecord): boolean {
  const occurrence = item.occurrence;
  if (item.kind === 'occurrence' && occurrence) {
    if (occurrence.routine_deleted) return false;
    return (
      occurrence.status === 'pending' || occurrence.status === 'in_progress'
    );
  }
  return isAgendaItemPending(item);
}

/** Home's agenda cache as the optimistic edits see it (`readAgendaPages` in
 *  `queries/agenda.ts` concatenates the two caches into one page list). Only
 *  `pages` is ever read here; a page's param is its own `from`/`to` window. */
type AgendaStreamData = InfiniteData<
  AgendaResponse,
  { from: string; to: string }
>;

function streamItems(data: AgendaStreamData | undefined): AgendaItemRecord[] {
  return (data?.pages ?? []).flatMap((page) => page.items);
}

/** Every backlog row the cache holds, in page order. */
function backlogItems(data: AgendaStreamData | undefined): AgendaItemRecord[] {
  return (data?.pages ?? []).flatMap((page) => page.backlog ?? []);
}

/** Maps the ONE item with `id` through `fn`, wherever in the stream it sits
 *  (`items` or `backlog`), and leaves it in that same array. Returns the SAME
 *  data reference when the id is missing (or there is no data) so React Query
 *  skips the re-render. */
export function updateStreamItem(
  data: AgendaStreamData | undefined,
  id: string,
  fn: (item: AgendaItemRecord) => AgendaItemRecord,
): AgendaStreamData | undefined {
  if (!data) return data;
  let found = false;
  const pages = data.pages.map((page) => {
    let changed = false;
    const items = page.items.map((item) => {
      if (item.id !== id) return item;
      found = true;
      changed = true;
      return fn(item);
    });
    const backlog = (page.backlog ?? []).map((item) => {
      if (item.id !== id) return item;
      found = true;
      changed = true;
      return fn(item);
    });
    return changed ? { ...page, items, backlog } : page;
  });
  return found ? { ...data, pages } : data;
}

/** The optimistic mirror of the server's cross-day placement
 *  (`reschedule` with a rank — `place_on_date`): the item leaves whichever
 *  array holds it (`items` or `backlog`), every peer ON `toDate` ranked at/after
 *  `rank` shifts up one (the same rule as `applyAgendaMove`), and the item lands
 *  on `rank` in the page whose window covers `toDate`. When NO loaded page
 *  covers `toDate` the item leaves the cache entirely: it cannot live in a page
 *  whose window does not contain its `local_date`, or the day paints it twice
 *  once the server's real page loads.
 *
 *  `planIfOpen` mirrors the one move that changes a task's status: a
 *  **reschedule** (`/reschedule` with a date) turns an `OPEN` task `PLANNED`,
 *  so its callers pass `true`. Move-to-today's sweep and a task's own
 *  placement leave the status alone (ADR 0007 — the sweep is unchanged, an
 *  `OPEN` row stays `OPEN`), so they keep the default and never flash a
 *  Planned card. An already-`PLANNED` task and every occurrence are untouched
 *  either way. Returns the same reference when the id is missing. */
export function moveStreamItem(
  data: AgendaStreamData | undefined,
  id: string,
  toDate: string,
  rank: number,
  planIfOpen = false,
): AgendaStreamData | undefined {
  if (!data) return data;
  const source = data.pages.find(
    (page) =>
      page.items.some((item) => item.id === id) ||
      (page.backlog ?? []).some((item) => item.id === id),
  );
  const item =
    source?.items.find((entry) => entry.id === id) ??
    source?.backlog?.find((entry) => entry.id === id);
  if (!source || !item) return data;
  const moved: AgendaItemRecord = {
    ...item,
    local_date: toDate,
    sort_order: rank,
    task:
      planIfOpen && item.task && item.task.status === 'OPEN'
        ? { ...item.task, status: 'PLANNED' }
        : item.task,
  };
  const target = data.pages.find(
    (page) => page.from <= toDate && toDate <= page.to,
  );
  const pages = data.pages.map((page) => {
    const items = page.items.filter((entry) => entry.id !== id);
    const backlog = (page.backlog ?? []).filter((entry) => entry.id !== id);
    const removed =
      items.length !== page.items.length ||
      backlog.length !== (page.backlog?.length ?? 0);
    if (!target) {
      // Unloaded target day: drop the row (the stream's next read of that
      // window paints the server's copy). Never re-home it in `source`.
      return removed ? { ...page, items, backlog } : page;
    }
    const needsShift = items.some(
      (entry) => entry.local_date === toDate && entry.sort_order >= rank,
    );
    const shifted = needsShift
      ? items.map((entry) =>
          entry.local_date === toDate && entry.sort_order >= rank
            ? { ...entry, sort_order: entry.sort_order + 1 }
            : entry,
        )
      : items;
    if (page !== target) {
      return removed || needsShift
        ? { ...page, items: shifted, backlog }
        : page;
    }
    return { ...page, items: [...shifted, moved], backlog };
  });
  return { ...data, pages };
}

/** The optimistic mirror of a **backlog drop** (`reschedule
 *  { backlog: true }`, ADR 0007): the row leaves every `items` and `backlog`
 *  and lands at the END of the backlog on the page with the greatest `to`
 *  (the default read — the stream no longer pages forward) with
 *  `local_date = backlogDate`. A task that is not `OPEN` flips to `OPEN` with
 *  the appended Board rank (`max + 1` over the `OPEN` tasks already there); an
 *  already-`OPEN` task keeps its rank. Occurrences have no Board rank, so only
 *  they keep their embed untouched. Returns the same reference when the id is
 *  missing (or no page is loaded). */
export function moveItemToBacklog(
  data: AgendaStreamData | undefined,
  id: string,
  backlogDate: string,
): AgendaStreamData | undefined {
  if (!data || data.pages.length === 0) return data;
  const item = [...streamItems(data), ...backlogItems(data)].find(
    (entry) => entry.id === id,
  );
  if (!item) return data;
  const target = data.pages.reduce((best, page) =>
    page.to > best.to ? page : best,
  );
  const openRanks = (target.backlog ?? [])
    .filter((entry) => entry.id !== id && entry.task?.status === 'OPEN')
    .map((entry) => entry.task?.sort_order ?? 0);
  const appendRank = openRanks.reduce(
    (rank, value) => Math.max(rank, value + 1),
    0,
  );
  const moved: AgendaItemRecord = {
    ...item,
    local_date: backlogDate,
    task:
      item.task && item.task.status !== 'OPEN'
        ? { ...item.task, status: 'OPEN', sort_order: appendRank }
        : item.task,
  };
  const pages = data.pages.map((page) => {
    const items = page.items.filter((entry) => entry.id !== id);
    const backlog = (page.backlog ?? []).filter((entry) => entry.id !== id);
    if (page !== target) {
      return items.length !== page.items.length ||
        backlog.length !== (page.backlog?.length ?? 0)
        ? { ...page, items, backlog }
        : page;
    }
    return { ...page, items, backlog: [...backlog, moved] };
  });
  return { ...data, pages };
}

/** The optimistic mirror of `reschedule { unschedule: true }` (ADR 0010): a
 *  dated `PLANNED` row leaves its day and parks in the **Planned** warehouse
 *  without a status change, a task log or a Board re-rank — so `task` (status
 *  AND `sort_order`) is kept byte-for-byte and only `local_date` moves to
 *  `sentinelDate`. The row is removed from every page's `items` and `backlog`
 *  and re-inserted into the backlog of the page with the greatest `to` (the
 *  default read, same target as `moveItemToBacklog`) as
 *  `dated ++ planned ++ open`: the dated stretch keeps encounter order, the
 *  Planned rows — the mover included — are re-laid in Board order
 *  (`tasks.sort_order`, ties by task id) because the row is appended at
 *  `max + 1` server-side, and the OPEN stretch keeps its own order after them.
 *  `today` is needed to tell a dated `PLANNED` row (still `dated`) from a
 *  warehoused one. Returns the same reference when the id is missing (or no
 *  page is loaded). */
export function unscheduleItem(
  data: AgendaStreamData | undefined,
  id: string,
  sentinelDate: string,
  today: string,
): AgendaStreamData | undefined {
  if (!data || data.pages.length === 0) return data;
  const item = [...streamItems(data), ...backlogItems(data)].find(
    (entry) => entry.id === id,
  );
  if (!item) return data;
  const target = data.pages.reduce((best, page) =>
    page.to > best.to ? page : best,
  );
  const moved: AgendaItemRecord = { ...item, local_date: sentinelDate };
  const stretches = backlogStretchesForRebuild(
    (target.backlog ?? []).filter((entry) => entry.id !== id),
    today,
  );
  const planned = [...stretches.planned, moved].sort(byPlannedBoardOrder);
  const pages = data.pages.map((page) => {
    const items = page.items.filter((entry) => entry.id !== id);
    const backlog = (page.backlog ?? []).filter((entry) => entry.id !== id);
    if (page !== target) {
      return items.length !== page.items.length ||
        backlog.length !== (page.backlog?.length ?? 0)
        ? { ...page, items, backlog }
        : page;
    }
    return {
      ...page,
      items,
      backlog: [...stretches.dated, ...planned, ...stretches.open],
    };
  });
  return { ...data, pages };
}

/** The optimistic mirror of a **plan** (`POST /api/tasks/:id/move {
 *  status: 'PLANNED' }` with no rank, ADR 0010): an `OPEN` task becomes
 *  `PLANNED` with the appended Planned-column rank (`max + 1` over every other
 *  `PLANNED` embed in the loaded cache, else 0).
 *
 *  The plan is a status-only transition, so the row's `local_date` and agenda
 *  `sort_order` are the user's scheduling intent and are left byte-for-byte
 *  alone:
 *  - a **dated** row (`local_date` below the 10-year horizon) stays exactly
 *    where it is — in its page's `items` (or the dated backlog stretch) — and
 *    only its task embed flips status and takes the appended Planned rank;
 *  - an **undated/warehoused** row keeps its own date (sentinel and
 *    exact-horizon parking are both valid) and is re-inserted into the END of
 *    the Planned stretch of the greatest-`to` page (dated ++ planned ++ open),
 *    because its Board rank space just changed.
 *
 *  A malformed/absent `today` cannot prove the row is warehoused, so the row
 *  stays where it is. Anything that is not an `OPEN` task (an already-planned
 *  row, an occurrence, an orphan) returns the same reference, as does a missing
 *  id. */
export function planOpenItem(
  data: AgendaStreamData | undefined,
  id: string,
  today: string,
): AgendaStreamData | undefined {
  if (!data || data.pages.length === 0) return data;
  const item = [...streamItems(data), ...backlogItems(data)].find(
    (entry) => entry.id === id,
  );
  if (!item || !item.task || item.task.status !== 'OPEN') return data;
  const horizon =
    today && isCivilDateValid(today) ? addMonthsClamp(today, 120) : '';
  const plannedRanks = [...streamItems(data), ...backlogItems(data)]
    .filter((entry) => entry.id !== id && entry.task?.status === 'PLANNED')
    .map((entry) => entry.task?.sort_order ?? 0);
  const appendRank = plannedRanks.reduce(
    (rank, value) => Math.max(rank, value + 1),
    0,
  );
  const moved: AgendaItemRecord = {
    ...item,
    task: { ...item.task, status: 'PLANNED', sort_order: appendRank },
  };
  if (!horizon || item.local_date < horizon) {
    // Dated (or unknowable): only the embed changes, the row stays put.
    return updateStreamItem(data, id, () => moved);
  }
  const target = data.pages.reduce((best, page) =>
    page.to > best.to ? page : best,
  );
  const stretches = backlogStretchesForRebuild(
    (target.backlog ?? []).filter((entry) => entry.id !== id),
    today,
  );
  const pages = data.pages.map((page) => {
    const items = page.items.filter((entry) => entry.id !== id);
    const backlog = (page.backlog ?? []).filter((entry) => entry.id !== id);
    if (page !== target) {
      return items.length !== page.items.length ||
        backlog.length !== (page.backlog?.length ?? 0)
        ? { ...page, items, backlog }
        : page;
    }
    return {
      ...page,
      items,
      backlog: [
        ...stretches.dated,
        ...stretches.planned,
        moved,
        ...stretches.open,
      ],
    };
  });
  return { ...data, pages };
}

/** The absolute rank `POST /api/agenda/items/:id/move { sort_order }` should
 *  carry for one up/down step. The server shifts every peer at/after the
 *  target rank up one and lands the item on it, so:
 *  - up: the item takes the above row's rank (that row shifts up);
 *  - down: the item lands just AFTER the below row (`below.sort_order + 1`).
 *  Returns `null` when the item is missing or already at the edge. */
export function agendaMoveTarget(
  items: AgendaItemRecord[],
  itemId: string,
  direction: 'up' | 'down',
): number | null {
  const idx = items.findIndex((item) => item.id === itemId);
  if (idx < 0) return null;
  const neighborIdx = idx + (direction === 'up' ? -1 : 1);
  if (neighborIdx < 0 || neighborIdx >= items.length) return null;
  return direction === 'up'
    ? items[neighborIdx].sort_order
    : items[neighborIdx].sort_order + 1;
}

/** The absolute rank `POST /api/agenda/items/:id/move { sort_order }` should
 *  carry to land the item at `fromIndex` on the slot now held by `toIndex`
 *  (the dnd-kit drop). Same server contract as `agendaMoveTarget`:
 *  - dropped above (`toIndex < fromIndex`): the item takes the target row's
 *    rank (that row shifts up);
 *  - dropped below (`toIndex > fromIndex`): the item lands just AFTER the
 *    target row (`items[toIndex].sort_order + 1`).
 *  Returns `null` when either index is out of range or they are equal. */
export function agendaMoveTargetAt(
  items: AgendaItemRecord[],
  fromIndex: number,
  toIndex: number,
): number | null {
  if (
    fromIndex < 0 ||
    toIndex < 0 ||
    fromIndex >= items.length ||
    toIndex >= items.length ||
    fromIndex === toIndex
  ) {
    return null;
  }
  return toIndex < fromIndex
    ? items[toIndex].sort_order
    : items[toIndex].sort_order + 1;
}

/** The absolute Board rank `POST /api/tasks/:id/move { status, sort_order }`
 *  should carry to land an OPEN **or PLANNED** warehouse row at `fromIndex` on
 *  the slot now held by `toIndex` (the math is status-keyed: the caller passes
 *  the stretch for the status it is reordering). The rank is the task's own
 *  `tasks.sort_order` (ADR 0007 — Home and the Board share one Open order;
 *  ADR 0010 — and one Planned order), never the agenda row's, and it is the
 *  HOVERED row's rank in BOTH directions: a same-status `/move` is
 *  `reorder_in_place` (packages/api-core/src/tasks.rs), which shifts the
 *  hovered row away from the slot and lands the mover on it — the `+ 1` of the
 *  agenda's "land after the row" rule would put the mover past the hovered
 *  card. `null` when either index is out of range, they are equal, or either
 *  row has no task embed. */
export function openTaskMoveTargetAt(
  items: AgendaItemRecord[],
  fromIndex: number,
  toIndex: number,
): number | null {
  if (
    fromIndex < 0 ||
    toIndex < 0 ||
    fromIndex >= items.length ||
    toIndex >= items.length ||
    fromIndex === toIndex
  ) {
    return null;
  }
  const from = items[fromIndex].task;
  const to = items[toIndex].task;
  if (!from || !to) return null;
  return to.sort_order;
}

/** The optimistic mirror of the Board's **same-status** reorder
 *  (`reorder_in_place`, packages/api-core/src/tasks.rs) over a pile of tasks:
 *  a drop sends the hovered card's rank, the mover lands on it, and every
 *  OTHER task in the mover's status shifts away from the vacated slot —
 *  `[new, old)` up by one when the mover drops above (`new < old`), `(old,
 *  new]` down by one when it drops below (`new > old`). Tasks in any other
 *  status keep their ranks. The math is status-keyed, so it serves both
 *  warehouses: `OPEN` (ADR 0007) and `PLANNED` (ADR 0010 — the same Planned
 *  column the Board shows). `new === old` writes nothing, and a missing mover
 *  returns the input array unchanged. */
export function applyOpenTaskReorder(
  tasks: TaskRecord[],
  moverId: string,
  newSortOrder: number,
): TaskRecord[] {
  const mover = tasks.find((entry) => entry.id === moverId);
  if (!mover || mover.sort_order === newSortOrder) return tasks;
  const old = mover.sort_order;
  return tasks.map((entry) => {
    if (entry.id === moverId) return { ...entry, sort_order: newSortOrder };
    if (entry.status !== mover.status) return entry;
    if (newSortOrder < old) {
      return entry.sort_order >= newSortOrder && entry.sort_order < old
        ? { ...entry, sort_order: entry.sort_order + 1 }
        : entry;
    }
    return entry.sort_order > old && entry.sort_order <= newSortOrder
      ? { ...entry, sort_order: entry.sort_order - 1 }
      : entry;
  });
}

/** Replaces one task inside the shared `['tasks']` cache by its id with the
 *  server's own row — status AND `sort_order`, so Board/Lists follow a write
 *  that moved the task to another column. Rows of other ids (and their object
 *  references) are untouched, and a `fresh` of `null`/`undefined` (the task has
 *  no row any more) or an id the list does not hold returns the SAME array, so
 *  React Query can skip the re-render. */
export function replaceCachedTask(
  tasks: TaskRecord[],
  fresh: TaskRecord | null | undefined,
): TaskRecord[] {
  if (!fresh) return tasks;
  const id = fresh.id;
  if (!tasks.some((entry) => entry.id === id)) return tasks;
  return tasks.map((entry) => (entry.id === id ? fresh : entry));
}

/** The absolute rank a cross-day drop should send when it lands OVER one of
 *  the target day's living rows: that row's own `sort_order`, never its list
 *  index. The server's `place_on_date` shifts every peer ranked at/after the
 *  number up one and lands the item on it, so an index would land the item in
 *  the wrong slot whenever the pile has rank gaps or parked (Completed) rows —
 *  those keep their ranks in the same number space.
 *  `null` (a drop on the day container itself) or an id that is not in
 *  `living` returns `undefined`: the caller omits `sort_order` and the server
 *  appends at `max + 1`. */
export function crossDaySortOrder(
  living: AgendaItemRecord[],
  overItemId: string | null,
): number | undefined {
  if (!overItemId) return undefined;
  return living.find((entry) => entry.id === overItemId)?.sort_order;
}

/** Optimistic mirror of the server's `shift_sort_order` + `set_sort_order`:
 *  the moved item lands on `target`, every other peer at/after `target`
 *  shifts up one. Returns a new array sorted by rank — identical ranks to
 *  what the server stores, so the next move target stays exact. */
export function applyAgendaMove(
  items: AgendaItemRecord[],
  itemId: string,
  target: number,
): AgendaItemRecord[] {
  const item = items.find((entry) => entry.id === itemId);
  if (!item || item.sort_order === target) return items;
  return items
    .map((entry) => {
      if (entry.id === itemId) return { ...entry, sort_order: target };
      if (entry.sort_order >= target) {
        return { ...entry, sort_order: entry.sort_order + 1 };
      }
      return entry;
    })
    .sort((a, b) => a.sort_order - b.sort_order);
}

/** Reschedule visibility (ADR 0004 amendment, ADR 0007, ADR 0011):
 *  occurrences move only while `pending | skipped` (in_progress/done → API
 *  400), tasks while living EXCEPT a running one — an `IN_PROGRESS` task
 *  cannot change day (the API 400s `cannot reschedule an in_progress task`;
 *  pause first), so the control hides. A `WAITING` row CAN change day and
 *  stays waiting (ADR 0011 § Reschedule — the API allows it, unlike
 *  `IN_PROGRESS`). COMPLETED/DISCARDED slots are hidden so Home never invites
 *  moving a finished card (the API would allow it). A `routine_deleted` row is
 *  read-only history: never reschedulable. */
export function canReschedule(item: AgendaItemRecord): boolean {
  if (item.kind === 'occurrence' && item.occurrence) {
    if (item.occurrence.routine_deleted) return false;
    return (
      item.occurrence.status === 'pending' ||
      item.occurrence.status === 'skipped'
    );
  }
  if (item.kind === 'task' && item.task) {
    return (
      item.task.status !== 'COMPLETED' &&
      item.task.status !== 'DISCARDED' &&
      item.task.status !== 'IN_PROGRESS'
    );
  }
  return false;
}

/** Backlog as a destination (ADR 0007, ADR 0010, ADR 0011): tasks only, and
 *  only the ones the drop can actually move. An `OPEN` task already parked on
 *  the backlog sentinel is never offered the drop again (it is already there),
 *  but a `PLANNED` row at/past the horizon still is — that drop UNPLANS it
 *  (`{ backlog: true }` flips it to `OPEN` and appends the Open column), so it
 *  is a real transition even though the row barely moves. An `OPEN` task on a
 *  near date can still be dropped (the row moves; no status change), while a
 *  `PLANNED` row is the real transition. A `WAITING` task is refused: the API
 *  400s the backlog drop for it — reaching the warehouse means moving it to
 *  Open or Planned first (ADR 0011 § Reschedule and unschedule). Occurrences
 *  have no backlog, and terminal cards are never invited back into it. */
export function canMoveToBacklog(
  item: AgendaItemRecord,
  today: string,
): boolean {
  if (!isCivilDateValid(today)) return false;
  if (item.kind !== 'task' || !item.task) return false;
  if (
    item.task.status === 'IN_PROGRESS' ||
    item.task.status === 'WAITING' ||
    item.task.status === 'COMPLETED' ||
    item.task.status === 'DISCARDED'
  ) {
    return false;
  }
  return !isOpenBacklogTask(item, today);
}

/** Unschedule as a destination (ADR 0010): a **dated** `PLANNED` task — its row
 *  sits before the 10-year line, past dates included — can be parked in the
 *  Planned warehouse without touching its status, its Board rank or the logs.
 *  A row already at/after the horizon is the warehouse itself (the server
 *  answers 200 as a no-op), and everything the API 400s stays hidden: an
 *  `OPEN` row (use Backlog / plan instead), a `WAITING` task (move it to Open
 *  or Planned first — ADR 0011 § Reschedule and unschedule), a running task, a
 *  terminal card, an occurrence, an orphan. A malformed/absent `today` offers
 *  nothing. */
export function canUnschedule(item: AgendaItemRecord, today: string): boolean {
  if (!today || !isCivilDateValid(today)) return false;
  if (item.kind !== 'task' || !item.task) return false;
  return (
    item.task.status === 'PLANNED' &&
    item.local_date < addMonthsClamp(today, 120)
  );
}

/** True when the row belongs under the Completed dump: a task embed in a
 *  terminal status (COMPLETED | DISCARDED) or a `done` occurrence. Orphans
 *  (embedless rows) are never parked — the dump must never hide one. */
export function isAgendaItemParked(item: AgendaItemRecord): boolean {
  if (item.kind === 'task' && item.task) {
    return item.task.status === 'COMPLETED' || item.task.status === 'DISCARDED';
  }
  if (item.kind === 'occurrence' && item.occurrence) {
    return item.occurrence.status === 'done';
  }
  return false;
}

/** The embed's `updated_at` in ms since epoch (the Completed dump sort key).
 *  Missing/invalid timestamps count as 0 — the oldest possible slot. */
function embedUpdatedAtMs(item: AgendaItemRecord): number {
  const raw = item.occurrence?.updated_at ?? item.task?.updated_at;
  if (!raw) return 0;
  const ms = Date.parse(raw);
  return Number.isFinite(ms) ? ms : 0;
}

/** Split one date's pile. `living` is sort_order asc; `completed` is newest
 *  embed updated_at first (id asc on ties). Does not mutate the input. */
export function partitionAgendaItems(items: AgendaItemRecord[]): {
  living: AgendaItemRecord[];
  completed: AgendaItemRecord[];
} {
  const living: AgendaItemRecord[] = [];
  const completed: AgendaItemRecord[] = [];
  for (const entry of items) {
    (isAgendaItemParked(entry) ? completed : living).push(entry);
  }
  living.sort((a, b) => a.sort_order - b.sort_order);
  completed.sort((a, b) => {
    const byTime = embedUpdatedAtMs(b) - embedUpdatedAtMs(a);
    return byTime !== 0 ? byTime : a.id.localeCompare(b.id);
  });
  return { living, completed };
}

/** Splits one date's pile into the `WAITING` rows its **Waiting section**
 *  renders and everything else (ADR 0011 § Agenda): the section owns that
 *  day's living waiting tasks, the main list owns the rest, so a waiting row
 *  is never painted twice. Occurrences never wait. Both piles keep the input
 *  order (the caller has already partitioned / sorted); does not mutate the
 *  input. */
export function splitWaitingRows(items: AgendaItemRecord[]): {
  waiting: AgendaItemRecord[];
  rest: AgendaItemRecord[];
} {
  const waiting: AgendaItemRecord[] = [];
  const rest: AgendaItemRecord[] = [];
  for (const entry of items) {
    if (entry.kind === 'task' && entry.task?.status === 'WAITING') {
      waiting.push(entry);
    } else {
      rest.push(entry);
    }
  }
  return { waiting, rest };
}

/** The collapsed Waiting section's whole header copy (ADR 0011 § Agenda):
 *  the count is the summary, and clicking the row reveals the tasks. Kept
 *  pure so the pluralization is testable without mounting the page. */
export function waitingSectionLabel(count: number): string {
  if (count <= 0) return 'No tasks waiting';
  return `${count} task${count === 1 ? '' : 's'} in Waiting`;
}

/** Does this day paint a Waiting target? A day with waiting rows always does
 *  (collapsed behind its count). An empty day only shows the dashed drop
 *  target while a task row is being dragged, and never on a past day — the
 *  API rejects a reschedule there. */
export function shouldShowWaitingSection(
  waitingCount: number,
  isPast: boolean,
  dragging: boolean,
): boolean {
  return waitingCount > 0 || (dragging && !isPast);
}

/** What one dnd-kit drop should do — the pure decision HomePage's
 *  `handleDragEnd` acts on. `items` is `streamItemsOf` (page `items` plus
 *  `backlog`), and `streamItemIds` names the rows painted in a day stream
 *  (page `items`) rather than the Backlog's dated stretch. */
export type AgendaDropDecision =
  | { kind: 'none' }
  | { kind: 'sameDay'; target: number }
  | { kind: 'crossDay'; date: string; sortOrder?: number }
  | {
      kind: 'warehouseReorder';
      status: 'OPEN' | 'PLANNED';
      toIndex: number;
      target: number;
    }
  | { kind: 'plan' }
  | { kind: 'unschedule' }
  | { kind: 'backlogDrop' }
  | { kind: 'handOffToWaiting'; date: string }
  | { kind: 'waitingToWarehouse'; status: 'OPEN' | 'PLANNED' }
  | { kind: 'waitingToDate'; date: string; sortOrder?: number };

export interface ResolveAgendaDropInput {
  dragged: AgendaItemRecord;
  overId: string;
  overItem: AgendaItemRecord | null;
  /** All loaded rows: every page's `items` then its `backlog`. */
  items: AgendaItemRecord[];
  /** Ids painted in `page.items` (the day stream). A row in `page.backlog`
   *  is NOT in a Waiting section even when its status is WAITING. */
  streamItemIds: ReadonlySet<string>;
  today: string;
}

/** dnd-kit drop resolution, kept pure so `node:test` can cover the matrix
 *  (HomePage only carries the decision out). The semantics are the ADR 0007 /
 *  ADR 0010 warehouse rules plus ADR 0011's Waiting section:
 *  - a normal row dropped into a Waiting section is handed off to WAITING
 *    (and rescheduled when the section is another date);
 *  - a waiting row dropped out of its section becomes a dated `PLANNED` row;
 *  - a waiting row dropped in another day's Waiting section only reschedules;
 *  - a waiting row reordered inside its section uses the waiting rows' agenda
 *    ranks;
 *  - waiting → Planned/Backlog changes status through the Board's `/move`,
 *    followed by the explicit date action that shelves the row (the status
 *    move alone preserves a dated row);
 *  - past days never accept a drop, but same-day reorder inside a past day
 *    still works (the agenda `/move` has no date floor). */
export function resolveAgendaDrop({
  dragged,
  overId,
  overItem,
  items,
  streamItemIds,
  today,
}: ResolveAgendaDropInput): AgendaDropDecision {
  if (!today) return { kind: 'none' };
  const livingOf = (date: string) =>
    partitionAgendaItems(items.filter((entry) => entry.local_date === date))
      .living;
  const waitingRowsOf = (date: string) =>
    splitWaitingRows(livingOf(date)).waiting;
  const stream = (id: string) => streamItemIds.has(id);
  const draggedIsWaitingInSection =
    dragged.task?.status === 'WAITING' && stream(dragged.id);
  const backlog = dedupeAgendaItems(items.filter((entry) => !stream(entry.id)));
  const warehouseRowsOf = (status: 'OPEN' | 'PLANNED') => {
    const split = splitBacklogSection(backlog, today);
    return status === 'OPEN' ? split.open : split.planned;
  };
  // The list dnd-kit measured: a stream day's main pile excludes Waiting; a
  // dated Backlog group is one mixed list. A stale/missing id yields -1 and
  // the rank helpers return null/undefined.
  const sortableSameDay = () => {
    const living = livingOf(dragged.local_date);
    const sortable = stream(dragged.id)
      ? splitWaitingRows(living).rest
      : living;
    return sortable.filter(isAgendaRowDraggable);
  };
  const crossDayRank = (date: string, overItemId: string) => {
    const living = livingOf(date);
    const sortable =
      stream(dragged.id) && stream(overItemId)
        ? splitWaitingRows(living).rest
        : living;
    return crossDaySortOrder(sortable, overItemId);
  };

  const draggedIsOpen = isOpenBacklogTask(dragged, today);
  const draggedIsPlanned = isPlannedBacklogTask(dragged, today);
  const plannedTarget =
    overId === PLANNED_DROPPABLE_ID ||
    (overItem !== null && isPlannedBacklogTask(overItem, today));
  const openTarget =
    overId === BACKLOG_DROPPABLE_ID ||
    (overItem !== null && isOpenBacklogTask(overItem, today));
  // A Waiting target is only the section itself or a WAITING row painted in
  // the day stream. The same status in the dated Backlog stretch is an
  // ordinary backlog row, not a second hand-off target.
  const waitingTargetDate = overId.startsWith(WAITING_DAY_ID_PREFIX)
    ? overId.slice(WAITING_DAY_ID_PREFIX.length)
    : overItem?.task?.status === 'WAITING' && stream(overItem.id)
      ? overItem.local_date
      : null;

  // 1. Waiting section.
  if (waitingTargetDate) {
    const sameDayWaitingReorder =
      draggedIsWaitingInSection && waitingTargetDate === dragged.local_date;
    if (waitingTargetDate < today && !sameDayWaitingReorder) {
      return { kind: 'none' };
    }
    if (dragged.kind !== 'task' || !dragged.task) return { kind: 'none' };
    if (
      dragged.task.status === 'COMPLETED' ||
      dragged.task.status === 'DISCARDED'
    ) {
      return { kind: 'none' };
    }
    if (draggedIsWaitingInSection) {
      if (waitingTargetDate === dragged.local_date) {
        const waiting =
          waitingRowsOf(waitingTargetDate).filter(isAgendaRowDraggable);
        const target = agendaMoveTargetAt(
          waiting,
          waiting.findIndex((entry) => entry.id === dragged.id),
          overItem && overItem.task?.status === 'WAITING'
            ? waiting.findIndex((entry) => entry.id === overItem.id)
            : waiting.length - 1,
        );
        return target === null ? { kind: 'none' } : { kind: 'sameDay', target };
      }
      return {
        kind: 'crossDay',
        date: waitingTargetDate,
        sortOrder:
          overItem?.task?.status === 'WAITING'
            ? crossDaySortOrder(waitingRowsOf(waitingTargetDate), overItem.id)
            : undefined,
      };
    }
    if (dragged.task.status === 'WAITING') {
      // A WAITING row from the Backlog's dated stretch keeps its status and
      // merely reschedules into the section it was dropped on.
      return {
        kind: 'crossDay',
        date: waitingTargetDate,
        sortOrder:
          overItem?.task?.status === 'WAITING'
            ? crossDaySortOrder(waitingRowsOf(waitingTargetDate), overItem.id)
            : undefined,
      };
    }
    return { kind: 'handOffToWaiting', date: waitingTargetDate };
  }

  // 2. Planned warehouse.
  if (plannedTarget) {
    if (dragged.kind !== 'task' || !dragged.task) return { kind: 'none' };
    if (dragged.task.status === 'WAITING') {
      return { kind: 'waitingToWarehouse', status: 'PLANNED' };
    }
    if (
      dragged.task.status === 'IN_PROGRESS' ||
      dragged.task.status === 'COMPLETED' ||
      dragged.task.status === 'DISCARDED'
    ) {
      return { kind: 'none' };
    }
    if (draggedIsPlanned) {
      const planned = warehouseRowsOf('PLANNED');
      const toIndex = overItem
        ? planned.findIndex((entry) => entry.id === overItem.id)
        : planned.length - 1;
      const target = openTaskMoveTargetAt(
        planned,
        planned.findIndex((entry) => entry.id === dragged.id),
        toIndex,
      );
      return target === null
        ? { kind: 'none' }
        : { kind: 'warehouseReorder', status: 'PLANNED', toIndex, target };
    }
    if (dragged.task.status === 'OPEN') return { kind: 'plan' };
    if (dragged.task.status === 'PLANNED') return { kind: 'unschedule' };
    return { kind: 'none' };
  }

  // 3. Backlog.
  if (openTarget) {
    if (dragged.kind !== 'task' || !dragged.task) return { kind: 'none' };
    if (dragged.task.status === 'WAITING') {
      return { kind: 'waitingToWarehouse', status: 'OPEN' };
    }
    if (
      dragged.task.status === 'IN_PROGRESS' ||
      dragged.task.status === 'COMPLETED' ||
      dragged.task.status === 'DISCARDED'
    ) {
      return { kind: 'none' };
    }
    if (draggedIsOpen) {
      if (!overItem) return { kind: 'none' };
      const open = warehouseRowsOf('OPEN');
      const toIndex = open.findIndex((entry) => entry.id === overItem.id);
      const target = openTaskMoveTargetAt(
        open,
        open.findIndex((entry) => entry.id === dragged.id),
        toIndex,
      );
      return target === null
        ? { kind: 'none' }
        : { kind: 'warehouseReorder', status: 'OPEN', toIndex, target };
    }
    if (dragged.task.status === 'OPEN' || dragged.task.status === 'PLANNED') {
      return { kind: 'backlogDrop' };
    }
    return { kind: 'none' };
  }

  // 4. A day's own container. A day container is always part of the day
  //    stream, so every WAITING row dropped here leaves Waiting as a dated
  //    PLANNED row — including one that was living in the dated Backlog.
  if (overId.startsWith(DAY_ID_PREFIX)) {
    const day = overId.slice(DAY_ID_PREFIX.length);
    if (day < today) return { kind: 'none' };
    if (dragged.task?.status === 'WAITING') {
      return { kind: 'waitingToDate', date: day };
    }
    if (day === dragged.local_date) {
      const sortable = sortableSameDay();
      const target = agendaMoveTargetAt(
        sortable,
        sortable.findIndex((entry) => entry.id === dragged.id),
        sortable.length - 1,
      );
      return target === null ? { kind: 'none' } : { kind: 'sameDay', target };
    }
    if (dragged.task?.status === 'IN_PROGRESS') return { kind: 'none' };
    return { kind: 'crossDay', date: day };
  }

  // 5. Another row. A WAITING row dropped on a normal stream row leaves
  //    Waiting as a dated PLANNED row. A WAITING row that already lives in
  //    the dated Backlog only takes that route when the target is a stream
  //    row; inside the dated stretch it is an ordinary same-group reorder or
  //    cross-group reschedule (both keep WAITING).
  if (!overItem) return { kind: 'none' };
  if (
    dragged.task?.status === 'WAITING' &&
    (draggedIsWaitingInSection || stream(overItem.id))
  ) {
    if (overItem.local_date < today) return { kind: 'none' };
    return {
      kind: 'waitingToDate',
      date: overItem.local_date,
      sortOrder: crossDaySortOrder(livingOf(overItem.local_date), overId),
    };
  }
  if (overItem.local_date === dragged.local_date) {
    const sortable = sortableSameDay();
    const target = agendaMoveTargetAt(
      sortable,
      sortable.findIndex((entry) => entry.id === dragged.id),
      sortable.findIndex((entry) => entry.id === overId),
    );
    return target === null ? { kind: 'none' } : { kind: 'sameDay', target };
  }
  if (overItem.local_date < today) return { kind: 'none' };
  if (dragged.task?.status === 'IN_PROGRESS') return { kind: 'none' };
  return {
    kind: 'crossDay',
    date: overItem.local_date,
    sortOrder: crossDayRank(overItem.local_date, overId),
  };
}

/** The append rank on `date`'s pile — the server's `max + 1`, else 0. Used by
 *  HomePage for the optimistic mirror of dating an undated task on today and of
 *  a reschedule that omits `sort_order`. */
export function appendRankOnStream(
  data: AgendaStreamData | undefined,
  date: string,
): number {
  return streamItems(data).reduce(
    (rank, entry) =>
      entry.local_date === date ? Math.max(rank, entry.sort_order + 1) : rank,
    0,
  );
}

/** The optimistic mirror of the server's **date an undated task on today**
 *  rule (`date_undated_task_on_today`): the status flip happens either way (the
 *  embed is mapped through `fn`), but the row only travels to `today` — appended
 *  at the pile's end — when its date is **undated/warehoused**
 *  (`local_date >= today+10y`). A **dated** row is the user's scheduling
 *  intent: a status-only transition never moves it, so it keeps its date and
 *  rank. A row already on `today` keeps its rank too (the server's placement is
 *  a no-op there), and a missing/invalid `today` cannot prove the row is
 *  undated, so it is left where it is. Returns the same reference when `data`
 *  is missing or no row carries `id`. */
export function placeUndatedTaskOnToday(
  data: AgendaStreamData | undefined,
  id: string,
  today: string,
  fn: (item: AgendaItemRecord) => AgendaItemRecord,
): AgendaStreamData | undefined {
  const updated = updateStreamItem(data, id, fn);
  if (!updated || !today || !isCivilDateValid(today)) return updated;
  const row = streamItemsOf(updated).find((entry) => entry.id === id);
  if (!row || row.local_date < addMonthsClamp(today, 120)) return updated;
  return moveStreamItem(updated, id, today, appendRankOnStream(updated, today));
}

/** The server's hand-off placement as a cache repair: the row carries the
 *  fresh task embed and, when it was **undated** (at/after the 10-year
 *  horizon), travels to `today` appended. A dated row stays on its own day —
 *  the hand-off transition preserves the date (ADR 0011 rule 2 amended by the
 *  status/date decoupling) — and a malformed/absent `today` leaves it there
 *  too. The second-write failure path uses this on the PRE-hand-off snapshot so
 *  the cache never keeps a date the server refused. */
export function restoreHandedOffRow(
  data: AgendaStreamData | undefined,
  id: string,
  today: string,
  task: TaskRecord,
): AgendaStreamData | undefined {
  return placeUndatedTaskOnToday(data, id, today, (entry) =>
    entry.task ? { ...entry, task } : entry,
  );
}

/** Runs one agenda drop's write sequence: `first` always runs; `second` (when
 *  present) runs only after `first` resolves. A first failure rejects the
 *  whole move; a second failure keeps the first write's result and hands it to
 *  `onSecondFailure` so the caller can repaint the server's actual state. */
export type AgendaWriteFlowResult = 'ok' | 'first-failed' | 'second-failed';

export interface AgendaWriteFlow<TFirst, TSecond> {
  first: () => Promise<TFirst>;
  /** Omitted for a one-write move (hand-off straight onto today). */
  second?: () => Promise<TSecond>;
  onFirstSuccess: (first: TFirst) => void;
  onSecondSuccess: (second: TSecond) => void;
  onFirstFailure: (error: unknown) => void;
  onSecondFailure: (error: unknown, first: TFirst) => void;
}

export async function runAgendaWriteFlow<TFirst, TSecond = never>({
  first,
  second,
  onFirstSuccess,
  onSecondSuccess,
  onFirstFailure,
  onSecondFailure,
}: AgendaWriteFlow<TFirst, TSecond>): Promise<AgendaWriteFlowResult> {
  let firstResult: TFirst;
  try {
    firstResult = await first();
  } catch (error) {
    onFirstFailure(error);
    return 'first-failed';
  }
  onFirstSuccess(firstResult);
  if (!second) return 'ok';
  try {
    const secondResult = await second();
    onSecondSuccess(secondResult);
    return 'ok';
  } catch (error) {
    onSecondFailure(error, firstResult);
    return 'second-failed';
  }
}

/** Every loaded item the stream cache holds, in page order — each page's
 *  `items` then its `backlog` (ADR 0007: a backlog row is draggable too).
 *  HomePage's lookups (a drag source, a drag target, a task's one membership
 *  row) read this instead of the render-time list so an async handler never
 *  works on a stale snapshot. The day spine still comes from `page.items`
 *  only. */
export function streamItemsOf(
  data: AgendaStreamData | undefined,
): AgendaItemRecord[] {
  return (data?.pages ?? []).flatMap((page) => [
    ...page.items,
    ...(page.backlog ?? []),
  ]);
}

/** One row per id, keeping the FIRST occurrence (page order). A window read
 *  can only return each row once, but an optimistic paint that lands on a day
 *  no page covers, or a re-read racing a write, can leave the same id in two
 *  pages — grouping that would paint the day twice (duplicate React keys and
 *  duplicate sortable ids). Home flattens through this before grouping. */
export function dedupeAgendaItems(
  items: AgendaItemRecord[],
): AgendaItemRecord[] {
  const seen = new Set<string>();
  return items.filter((item) => {
    if (seen.has(item.id)) return false;
    seen.add(item.id);
    return true;
  });
}
