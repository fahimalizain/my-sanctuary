// Pure helpers for Home's agenda stream (ADR 0004 amendment — slice 5): the
// day label, the reorder rank math, the living / Completed dump split, the
// date-window walk, and the optimistic edits over React Query's infinite
// `{ pages }` cache. Kept React-free so `node:test` can cover them
// (HomePage.tsx is presentational glue on top).

import type { InfiniteData } from '@tanstack/react-query';
import type { AgendaItemRecord, AgendaResponse, TaskStatus } from '../../types';
import { addCivilDays, isCivilDateValid } from '../routines/rrule-preview';

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

/** Day-header title for one civil date: "Today" / "Tomorrow" / "Yesterday"
 *  relative to `today`, otherwise "Tue 25 Aug" (weekday, day, month — parsed
 *  with the same UTC-carrier trick as `rrule-preview`). A date in ANOTHER year
 *  also carries its year ("Fri 1 Jan 2027") so a long stream's history is
 *  never ambiguous — relative labels never do. Malformed input returns the
 *  input unchanged. */
export function agendaDateLabel(date: string, today: string): string {
  if (date === today) return 'Today';
  if (date === addCivilDays(today, 1)) return 'Tomorrow';
  if (date === addCivilDays(today, -1)) return 'Yesterday';
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

/** The living task statuses (ADR 0004 § Agenda rules): terminal tasks
 *  (COMPLETED | DISCARDED) are finished. */
const AGENDA_LIVING_STATUSES: TaskStatus[] = ['OPEN', 'PLANNED', 'IN_PROGRESS'];

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

/** Home's stream exactly as React Query caches it (see `queries/agenda.ts` —
 *  the type lives here too so the helpers stay dependency-light). */
type AgendaStreamData = InfiniteData<
  AgendaResponse,
  { from: string; to: string } | undefined
>;

function streamItems(data: AgendaStreamData | undefined): AgendaItemRecord[] {
  return (data?.pages ?? []).flatMap((page) => page.items);
}

/** Maps the ONE item with `id` through `fn`, wherever in the stream it sits,
 *  and leaves it in that same page. Returns the SAME data reference when the
 *  id is missing (or there is no data) so React Query skips the re-render. */
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
    return changed ? { ...page, items } : page;
  });
  return found ? { ...data, pages } : data;
}

/** The optimistic mirror of the server's cross-day placement
 *  (`reschedule` with a rank — `place_on_date`): the item leaves whichever
 *  page holds it, every peer ON `toDate` ranked at/after `rank` shifts up one
 *  (the same rule as `applyAgendaMove`), and the item lands on `rank` in the
 *  page whose window covers `toDate`. When NO loaded page covers `toDate` the
 *  item leaves the cache entirely: it cannot live in a page whose window does
 *  not contain its `local_date`, or the day paints it twice once the server's
 *  real page loads. Returns the same reference when the id is missing. */
export function moveStreamItem(
  data: AgendaStreamData | undefined,
  id: string,
  toDate: string,
  rank: number,
): AgendaStreamData | undefined {
  if (!data) return data;
  const source = data.pages.find((page) =>
    page.items.some((item) => item.id === id),
  );
  const item = source?.items.find((entry) => entry.id === id);
  if (!source || !item) return data;
  const moved: AgendaItemRecord = {
    ...item,
    local_date: toDate,
    sort_order: rank,
  };
  const target = data.pages.find(
    (page) => page.from <= toDate && toDate <= page.to,
  );
  const pages = data.pages.map((page) => {
    const items = page.items.filter((entry) => entry.id !== id);
    if (!target) {
      // Unloaded target day: drop the row (the stream's next read of that
      // window paints the server's copy). Never re-home it in `source`.
      return items.length === page.items.length ? page : { ...page, items };
    }
    const shifted = items.map((entry) =>
      entry.local_date === toDate && entry.sort_order >= rank
        ? { ...entry, sort_order: entry.sort_order + 1 }
        : entry,
    );
    if (page !== target) {
      return shifted.length === page.items.length
        ? page
        : { ...page, items: shifted };
    }
    return { ...page, items: [...shifted, moved] };
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

/** Reschedule visibility (ADR 0004 amendment): occurrences move only while
 *  `pending | skipped` (in_progress/done → API 400), tasks only while living
 *  — COMPLETED/DISCARDED slots are hidden so Home never invites moving a
 *  finished card (the API would allow it). A `routine_deleted` row is
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
    return item.task.status !== 'COMPLETED' && item.task.status !== 'DISCARDED';
  }
  return false;
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

/** The append rank on `date`'s pile — the server's `max + 1`, else 0. Used by
 *  HomePage for the optimistic mirror of `place_task_on_today` and of a
 *  reschedule that omits `sort_order`. */
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

/** Every loaded item of the stream, in page order — HomePage's lookups (a
 *  drag target, a task's one membership row) read this instead of the
 *  render-time list so an async handler never works on a stale snapshot. */
export function streamItemsOf(
  data: AgendaStreamData | undefined,
): AgendaItemRecord[] {
  return streamItems(data);
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
