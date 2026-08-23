// Pure helpers for the Home agenda (ADR 0004 § Surfaces — Home). Date label,
// add-task picker filter, and the reorder rank math live here so they can be
// unit-tested without a DOM; `readError` is the page's local copy of the
// server error envelope (Categories / Routines / board-model each keep their
// own — this slice does not refactor them to share).

import type { AgendaItemRecord, TaskRecord, TaskStatus } from '../../types';
import { addCivilDays } from '../routines/rrule-preview';

// The server's error envelope is `{"error": "message"}`; fall back to a
// generic message when the body is not JSON.
export async function readError(res: Response): Promise<string> {
  try {
    const data: unknown = await res.json();
    if (
      data &&
      typeof data === 'object' &&
      'error' in data &&
      typeof (data as { error: unknown }).error === 'string'
    ) {
      return (data as { error: string }).error;
    }
  } catch {
    // Not JSON — fall through to the generic message.
  }
  return `Request failed with status ${res.status}`;
}

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

/** Header title for the selected civil date: "Today" / "Tomorrow" /
 *  "Yesterday" relative to `today`, otherwise "Tue 25 Aug" (weekday, day,
 *  month — parsed with the same UTC-carrier trick as `rrule-preview`).
 *  Malformed input returns the input unchanged. */
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
  return `${WEEKDAY_SHORT[carrier.getUTCDay()]} ${Number(d)} ${MONTH_SHORT[Number(mo) - 1]}`;
}

// v1 pickable statuses (ADR 0004 § Agenda rules — living OPEN | PLANNED |
// IN_PROGRESS only; terminal tasks 400 on add).
const AGENDA_PICKABLE_STATUSES: TaskStatus[] = [
  'OPEN',
  'PLANNED',
  'IN_PROGRESS',
];

/** The add-task picker's pool: living OPEN/PLANNED/IN_PROGRESS tasks that
 *  are not already on the selected date (`onDateTaskIds` = the ref_ids of
 *  this date's task-kind agenda items). */
export function filterAgendaPickerTasks(
  tasks: TaskRecord[],
  onDateTaskIds: Set<string>,
): TaskRecord[] {
  return tasks.filter(
    (task) =>
      AGENDA_PICKABLE_STATUSES.includes(task.status) &&
      !onDateTaskIds.has(task.id),
  );
}

/** Free-text picker search over the stored title and the computed display
 *  title (case-insensitive substring; blank query matches everything). */
export function agendaTaskMatches(task: TaskRecord, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return (
    task.title.toLowerCase().includes(q) ||
    task.display_title.toLowerCase().includes(q)
  );
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
 *  finished card (the API would allow it). */
export function canReschedule(item: AgendaItemRecord): boolean {
  if (item.kind === 'occurrence' && item.occurrence) {
    return (
      item.occurrence.status === 'pending' ||
      item.occurrence.status === 'skipped'
    );
  }
  if (item.kind === 'task' && item.task) {
    return (
      item.task.status !== 'COMPLETED' && item.task.status !== 'DISCARDED'
    );
  }
  return false;
}
