import type {
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
 *    old last-visible card is pushed to #20 and off the board). */
export function resolveSortOrder(
  remaining: TaskRecord[],
  destIndex: number,
  destStatus: TaskStatus,
): number {
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

/** destIndex from destShown (includes the mover) → index into `remaining`
 *  (destShown minus the mover) so resolveSortOrder reads the hovered card,
 *  not the one after it. When the mover sat *before* the hover target,
 *  removing it shifts every later index down by one. */
export function destIndexInRemaining(
  destIndexInShown: number,
  oldIndexInShown: number, // -1 if the mover is not in the dest column
): number {
  if (oldIndexInShown !== -1 && oldIndexInShown < destIndexInShown) {
    return destIndexInShown - 1;
  }
  return destIndexInShown;
}

/** Optimistically applies a `/move` to the local task list — mirrors the
 *  server exactly: `reorder_in_place` for a same-column drop
 *  (packages/api-core/src/tasks.rs 1883–1915), `place_at` for a cross-column
 *  one (1863–1881). The success merge only replaces the mover
 *  (`response.task`), so siblings must already carry the post-shift ranks or
 *  the board stays wrong until the next refresh.
 *
 *  - Same column, `destSortOrder < old`: dest peers (not the mover) with
 *    `destSortOrder <= rank < old` shift +1.
 *  - Same column, `destSortOrder > old`: dest peers with
 *    `old < rank <= destSortOrder` shift −1.
 *  - Same column, `destSortOrder === old`: no sibling changes.
 *  - Cross column: dest peers with `rank >= destSortOrder` shift +1; the
 *    source column is NOT compacted (gaps are fine, like `place_at`).
 *
 *  Input objects are never mutated; shifted rows and the mover are spread
 *  into new objects. Returns `tasks` unchanged when the mover is missing. */
export function applyOptimisticMove(
  tasks: TaskRecord[],
  taskId: string,
  destStatus: TaskStatus,
  destSortOrder: number,
): TaskRecord[] {
  const mover = tasks.find((task) => task.id === taskId);
  if (!mover) return tasks;

  const old = mover.sort_order;
  if (mover.status === destStatus && destSortOrder !== old) {
    if (destSortOrder < old) {
      // new < old: peers with `new <= rank < old` shift up one.
      tasks = tasks.map((task) =>
        task.status === destStatus &&
        task.id !== taskId &&
        task.sort_order >= destSortOrder &&
        task.sort_order < old
          ? { ...task, sort_order: task.sort_order + 1 }
          : task,
      );
    } else {
      // new > old: peers with `old < rank <= new` shift down one.
      tasks = tasks.map((task) =>
        task.status === destStatus &&
        task.id !== taskId &&
        task.sort_order > old &&
        task.sort_order <= destSortOrder
          ? { ...task, sort_order: task.sort_order - 1 }
          : task,
      );
    }
  } else if (mover.status !== destStatus) {
    // Cross-column: dest peers with `rank >= insert` shift up one.
    tasks = tasks.map((task) =>
      task.status === destStatus &&
      task.id !== taskId &&
      task.sort_order >= destSortOrder
        ? { ...task, sort_order: task.sort_order + 1 }
        : task,
    );
  }

  return tasks.map((task) =>
    task.id === taskId
      ? { ...task, status: destStatus, sort_order: destSortOrder }
      : task,
  );
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
