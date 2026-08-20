import type {
  MoveTaskError,
  TaskDifficulty,
  TaskPriority,
  TaskRecord,
  TaskStatus,
} from '@/app/types';

/** The /board search params — locked by ADR 0002 § Filters. All fields are
 *  optional; missing params mean "all". `category` is a comma-separated list
 *  of category ids; unknown ids are ignored by the page. */
export type BoardSearch = {
  priority?: TaskPriority;
  difficulty?: TaskDifficulty;
  category?: string; // comma-separated category ids
};

// The server's error envelope is `{"error": "message"}`; fall back to a
// generic message when the body is not JSON. (Copied locally — ListsPage has
// its own copy and this slice does not refactor it to share.)
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

// Move failures can carry a `displaced` task (ADR 0002 § Move API): when the
// start fails AFTER a successful displace, the parked task stays and the
// client snaps only the moved card back. readError cannot express that, so
// the move flow parses the full body instead.
export async function readMoveError(res: Response): Promise<MoveTaskError> {
  try {
    const data: unknown = await res.json();
    if (
      data &&
      typeof data === 'object' &&
      'error' in data &&
      typeof (data as { error: unknown }).error === 'string'
    ) {
      return {
        error: (data as { error: string }).error,
        displaced: (data as Partial<MoveTaskError>).displaced,
      };
    }
  } catch {
    // Not JSON — fall through to the generic message.
  }
  return { error: `Request failed with status ${res.status}` };
}

export interface BoardColumn {
  title: string;
  status: TaskStatus;
  /** 2px accent strip on the column header only — the rest of the column
   *  stays neutral (Categories style). */
  accent: string;
}

// Column order and the status each one renders (ADR 0002 § Status model).
export const COLUMNS: BoardColumn[] = [
  { title: 'Backlog', status: 'OPEN', accent: 'bg-muted-foreground/20' },
  { title: 'Planned', status: 'PLANNED', accent: 'bg-sky-500' },
  { title: 'In Progress', status: 'IN_PROGRESS', accent: 'bg-emerald-500' },
  { title: 'Done', status: 'COMPLETED', accent: 'bg-sky-400' },
  { title: 'Discarded', status: 'DISCARDED', accent: 'bg-rose-400' },
];

// Done / Discarded render at most this many filtered matches (ADR 0002
// § Done/Discarded cap). Backlog / Planned / In Progress show all matches.
export const TERMINAL_COLUMN_CAP = 20;

// Column droppables are registered under `column:<status>` so they can never
// collide with a task UUID (ADR 0002 § DnD): `over.id` is either a task id or
// a prefixed column id, never a bare status string.
export const COLUMN_ID_PREFIX = 'column:';

/** Parses `destIndex` (a view index into the column's FILTERED, CAPPED
 *  display list, ADR 0002 § Filters) into the absolute `sort_order` to send.
 *  `remaining` is the dest column's displayed tasks minus the dragged id.
 *
 *  - Empty dest, or insert at the very top → the first visible card's rank
 *    (or 0). Prepend-compatible: the dropped card sorts first.
 *  - Middle → the hovered visible card's own rank (insert before it).
 *  - End → last visible rank + 1. Done/Discarded clamp at the rank of the
 *    20th visible match so the drop never lands past the capped window (the
 *    old last-visible card is pushed to #20 and off the board).
 *  - In Progress is a singleton: the rank is always 0. */
export function resolveSortOrder(
  remaining: TaskRecord[],
  destIndex: number,
  destStatus: TaskStatus,
): number {
  if (destStatus === 'IN_PROGRESS') return 0;

  if (remaining.length === 0 || destIndex <= 0) {
    return remaining[0]?.sort_order ?? 0;
  }

  if (destIndex >= remaining.length) {
    // Drop at the end of a capped terminal column: take the rank of the
    // last visible card — the dropped card lands inside 0..19 and the old
    // #20 slides off the board (ADR 0002 § Done/Discarded cap).
    if (
      (destStatus === 'COMPLETED' || destStatus === 'DISCARDED') &&
      remaining.length >= TERMINAL_COLUMN_CAP
    ) {
      return remaining[TERMINAL_COLUMN_CAP - 1].sort_order;
    }
    return remaining[remaining.length - 1].sort_order + 1;
  }

  return remaining[destIndex].sort_order;
}

/** Mirrors the Rust `default_move_rank` (ADR 0002 § Move API): the column
 *  default for a no-drop `/move`. The max is computed over ALL tasks of the
 *  target status (the server queries the whole column, not the filtered
 *  view), excluding `excludeId` so a mover's leftover rank never inflates
 *  its own append target.
 *
 *  - `OPEN` → `max + 1` (or 0 on an empty pile)
 *  - `PLANNED` from `IN_PROGRESS` (pause) → 0 (prepend)
 *  - `PLANNED` otherwise → `max + 1` (or 0)
 *  - `COMPLETED` / `DISCARDED` / `IN_PROGRESS` → 0 (prepend) */
export function defaultMoveRank(
  from: TaskStatus,
  to: TaskStatus,
  tasks: TaskRecord[],
  excludeId: string,
): number {
  const max = tasks.reduce<number | null>((highest, task) => {
    if (task.status !== to || task.id === excludeId) return highest;
    return highest === null
      ? task.sort_order
      : Math.max(highest, task.sort_order);
  }, null);
  const append = (maxOrNull: number | null) =>
    maxOrNull === null ? 0 : maxOrNull + 1;
  switch (to) {
    case 'OPEN':
      return append(max);
    case 'PLANNED':
      return from === 'IN_PROGRESS' ? 0 : append(max);
    case 'COMPLETED':
    case 'DISCARDED':
    case 'IN_PROGRESS':
      return 0;
  }
}
