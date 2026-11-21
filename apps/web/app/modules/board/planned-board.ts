// Pure model for the Board's date-grouped Planned column (ADR 0014).
// React-free so `node:test` can cover the grouping and the drop matrix:
// - dated groups mirror Home's day piles (`agenda_sort_order`);
// - the single Undated group mirrors the Board's flat Planned column
//   (`tasks.sort_order`, with `created_at` breaking rank ties);
// - drops resolve to one of the writes ADR 0014 § DnD matrix names.
// The server owns "today" and the 10-year horizon; this module only groups the
// rows it is given and never compares a date to a browser clock.

import type {
  AgendaItemRecord,
  PlannedBoardRow,
  TaskRecord,
  TaskStatus,
} from '@/app/types';

/** Every group's droppable id starts with this prefix, so a group id can
 *  never collide with a task id (mirrors `column:` on the flat Board). */
export const PLANNED_GROUP_PREFIX = 'planned-group:';
/** The single Undated group's key (never a civil date, never the sentinel
 *  date itself — the API would reject a reschedule that implies a real day). */
export const PLANNED_UNDATED_GROUP_KEY = 'undated';

export function plannedGroupId(key: string): string {
  return `${PLANNED_GROUP_PREFIX}${key}`;
}

/** The group key (`YYYY-MM-DD` or `undated`) from a droppable id, or null when
 *  the id is not a Planned group id. */
export function plannedGroupKeyFromId(id: string): string | null {
  if (!id.startsWith(PLANNED_GROUP_PREFIX)) return null;
  const key = id.slice(PLANNED_GROUP_PREFIX.length);
  return key.length > 0 ? key : null;
}

export interface PlannedBoardGroup {
  id: string;
  /** `YYYY-MM-DD` or `undated`. */
  key: string;
  /** The civil date, or null for the Undated group. */
  date: string | null;
  undated: boolean;
  /** Displayed/filtered order — the visible cards, never the raw pile. */
  taskIds: string[];
}

/** The rows indexed by `task_id` — the map every lookup and drop resolver
 *  takes. A duplicate task id keeps the first row (the server promises one). */
export function plannedRowsByTaskId(
  rows: PlannedBoardRow[],
): Map<string, PlannedBoardRow> {
  const byTask = new Map<string, PlannedBoardRow>();
  for (const row of rows) {
    if (!byTask.has(row.task_id)) byTask.set(row.task_id, row);
  }
  return byTask;
}

/** The task's row in the Planned Board read, if it has one. */
export function findPlannedRow(
  rows: PlannedBoardRow[],
  taskId: string,
): PlannedBoardRow | undefined {
  return rows.find((row) => row.task_id === taskId);
}

interface DatedEntry {
  task: TaskRecord;
  row: PlannedBoardRow;
}

/** Dated members: `agenda_sort_order` asc, ties by task id. A row with no
 *  agenda rank (which a dated row should never have) sinks to the end. */
function byAgendaRank(a: DatedEntry, b: DatedEntry): number {
  const aRank = a.row.agenda_sort_order;
  const bRank = b.row.agenda_sort_order;
  if (aRank !== bRank) {
    if (aRank === null) return 1;
    if (bRank === null) return -1;
    return aRank - bRank;
  }
  return a.task.id.localeCompare(b.task.id);
}

/** Builds the date groups from the Planned Board read, in display order.
 *
 *  - Only living `PLANNED` tasks that `isVisible` accepts are considered.
 *  - A missing row, `row.undated === true`, or a row without a date joins the
 *    single Undated group (the server also treats those as undated).
 *  - Dated groups sort by `local_date` ascending; their members by
 *    `agenda_sort_order` asc, ties by task id.
 *  - Undated sorts by the task's own `sort_order` asc, ties by `created_at`
 *    asc then task id — the Board's flat Planned order. The row's
 *    `board_sort_order` may stay in the payload for the optimistic helpers,
 *    but it never decides display order. Undated always comes last.
 *  - Empty DATED groups are omitted (a filter never leaves an empty day), but
 *    the Undated group is ALWAYS present, even with no cards: it is the
 *    column body's drop target and must never disappear. Inputs are never
 *    mutated. */
export function buildPlannedGroups(
  tasks: TaskRecord[],
  rows: PlannedBoardRow[],
  isVisible: (task: TaskRecord) => boolean,
): PlannedBoardGroup[] {
  const rowsByTask = plannedRowsByTaskId(rows);
  const dated = new Map<string, DatedEntry[]>();
  const undated: TaskRecord[] = [];

  for (const task of tasks) {
    if (task.status !== 'PLANNED' || !isVisible(task)) continue;
    const row = rowsByTask.get(task.id);
    if (!row || row.undated || row.local_date === null) {
      undated.push(task);
      continue;
    }
    const bucket = dated.get(row.local_date);
    if (bucket) bucket.push({ task, row });
    else dated.set(row.local_date, [{ task, row }]);
  }

  const groups: PlannedBoardGroup[] = [...dated.entries()]
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    .map(([date, entries]) => ({
      id: plannedGroupId(date),
      key: date,
      date,
      undated: false,
      taskIds: entries.sort(byAgendaRank).map((entry) => entry.task.id),
    }));

  undated.sort(
    (a, b) =>
      a.sort_order - b.sort_order ||
      a.created_at.localeCompare(b.created_at) ||
      a.id.localeCompare(b.id),
  );
  groups.push({
    id: plannedGroupId(PLANNED_UNDATED_GROUP_KEY),
    key: PLANNED_UNDATED_GROUP_KEY,
    date: null,
    undated: true,
    taskIds: undated.map((task) => task.id),
  });

  return groups;
}

/** The group that currently holds a task, or null when it is not displayed
 *  (not PLANNED, filtered out, or unknown). */
export function plannedGroupOfTask(
  groups: PlannedBoardGroup[],
  taskId: string,
): PlannedBoardGroup | null {
  for (const group of groups) {
    if (group.taskIds.includes(taskId)) return group;
  }
  return null;
}

/** The group's cards while a drag hides its own card. The committed grouping
 *  keeps the active card, so the group (and its droppable body) survives even
 *  when that card was its only member; the renderer hides just the card.
 *  Returns the same array reference when the id is absent. */
export function visiblePlannedTaskIds(
  group: PlannedBoardGroup,
  hiddenTaskId: string | null,
): string[] {
  if (hiddenTaskId === null) return group.taskIds;
  const next = group.taskIds.filter((id) => id !== hiddenTaskId);
  return next.length === group.taskIds.length ? group.taskIds : next;
}

/** Maps a placeholder index computed against the COMMITTED group onto the
 *  visible list, clamped to `[0, visible length]`; `null` stays `null`.
 *
 *  The committed hover index ALREADY names the visible insertion slot: the
 *  rank math lands a downward same-group move AFTER the hovered card, and
 *  removing the hidden mover from before that card shifts the card up into
 *  exactly the same index. Upward / foreign drops hover the card whose slot
 *  they insert before, so no shift is needed there either. */
export function visiblePlannedPreviewIndex(
  group: PlannedBoardGroup,
  hiddenTaskId: string | null,
  previewIndex: number | null,
): number | null {
  if (previewIndex === null) return null;
  const visibleLength = visiblePlannedTaskIds(group, hiddenTaskId).length;
  return Math.min(Math.max(previewIndex, 0), visibleLength);
}

function tasksOfGroup(
  group: PlannedBoardGroup | null,
  tasks: ReadonlyMap<string, TaskRecord>,
): TaskRecord[] {
  if (!group) return [];
  const found: TaskRecord[] = [];
  for (const id of group.taskIds) {
    const task = tasks.get(id);
    if (task) found.push(task);
  }
  return found;
}

/** The absolute agenda rank a drop inside a **dated** group should send
 *  (`POST /api/agenda/items/:id/move { sort_order }`), mirroring Home's
 *  `agendaMoveTargetAt`:
 *  - dropped below the target (`moving` index < target index): the target's
 *    `agenda_sort_order + 1`;
 *  - dropped above it, or arriving from another group: the target's own rank.
 *
 *  `undefined` means "no rank": the group body (`targetTaskId` null), the
 *  mover dropped on itself, a missing rank, or an Undated group (which uses
 *  the Board rank instead). */
export function plannedAgendaRankForDrop(
  group: PlannedBoardGroup,
  rows: ReadonlyMap<string, PlannedBoardRow>,
  targetTaskId: string | null,
  movingTaskId: string | null,
): number | undefined {
  if (group.undated || targetTaskId === null) return undefined;
  if (movingTaskId === targetTaskId) return undefined;
  const targetRank = rows.get(targetTaskId)?.agenda_sort_order;
  if (targetRank === null || targetRank === undefined) return undefined;
  if (movingTaskId === null) return targetRank;
  const fromIndex = group.taskIds.indexOf(movingTaskId);
  const toIndex = group.taskIds.indexOf(targetTaskId);
  if (fromIndex !== -1 && toIndex !== -1) {
    if (fromIndex === toIndex) return undefined;
    return fromIndex < toIndex ? targetRank + 1 : targetRank;
  }
  // Cross-group/unknown mover: insert before the hovered card.
  return targetRank;
}

/** The absolute Board rank a drop inside the **Undated** group should send
 *  (`POST /api/tasks/:id/move { status: 'PLANNED', sort_order }`), mirroring
 *  the Board's `resolveSortOrder`: the hovered card's own rank in both
 *  directions, or the last visible peer's rank + 1 (0 on an empty pile) for a
 *  group-body drop. The mover is excluded from the append target, like the
 *  Board's `remaining`. */
export function plannedBoardRankForDrop(
  tasksInGroup: TaskRecord[],
  targetTaskId: string | null,
  movingTaskId: string | null,
): number {
  if (targetTaskId !== null) {
    const target = tasksInGroup.find((task) => task.id === targetTaskId);
    if (target) return target.sort_order;
  }
  const peers =
    movingTaskId === null
      ? tasksInGroup
      : tasksInGroup.filter((task) => task.id !== movingTaskId);
  if (peers.length === 0) return 0;
  return peers[peers.length - 1].sort_order + 1;
}

export type PlannedDropTarget =
  | { kind: 'card'; taskId: string }
  | { kind: 'group'; groupId: string }
  /** The Planned column's own body. Only a non-PLANNED card joins Undated
   *  here; a PLANNED card has no date write to make (the explicit Undated
   *  group is its unschedule target). */
  | { kind: 'column' };

/** What one Planned-column drop should do. The caller performs the writes:
 *  `reorderDated` is the agenda `/move`, `moveToDate`/`unschedule` the agenda
 *  `/reschedule`, and the join/reorder Undated decisions the Board task
 *  `/move`. `joinDated` and `joinUndated` are the FIRST write of a
 *  cross-column drop (ADR 0014 § Two-write drops). */
export type PlannedDropDecision =
  | { kind: 'none' }
  | {
      kind: 'reorderDated';
      group: PlannedBoardGroup;
      itemId: string;
      targetRank: number;
    }
  | {
      kind: 'moveToDate';
      group: PlannedBoardGroup;
      itemId: string;
      targetRank?: number;
    }
  | { kind: 'unschedule'; itemId: string }
  | { kind: 'reorderUndated'; taskId: string; targetRank: number }
  | { kind: 'joinDated'; group: PlannedBoardGroup; targetRank?: number }
  | { kind: 'joinUndated'; targetRank: number }
  /** A PLANNED card dropped on another status's card: status-only move. The
   *  caller knows the destination status from the over target. */
  | { kind: 'leavePlanned' };

export interface ResolvePlannedDropInput {
  activeTaskId: string;
  activeStatus: TaskStatus;
  /** The active task's group, or null when it is not displayed (non-PLANNED,
   *  filtered out, or unknown). */
  activeGroup: PlannedBoardGroup | null;
  over: PlannedDropTarget;
  groups: PlannedBoardGroup[];
  rows: ReadonlyMap<string, PlannedBoardRow>;
  tasks: ReadonlyMap<string, TaskRecord>;
  /** `YYYY-MM-DD` — the read's server-computed civil today. A dated target
   *  before it is inert; an empty value only occurs before the read resolves. */
  today: string;
}

/** True when a dated target is in the past. `YYYY-MM-DD` sorts
 *  chronologically, so the string compare is the civil compare. */
function isPastDate(date: string | null, today: string): boolean {
  return date !== null && today !== '' && date < today;
}

/** Resolves one drop on the date-grouped Planned column (ADR 0014 § DnD
 *  matrix), kept pure so the page only carries the decision out:
 *
 *  - PLANNED, same dated group card → `reorderDated`;
 *  - PLANNED, another dated group card → `moveToDate` at the hovered rank;
 *  - PLANNED, another dated group body → `moveToDate` appended;
 *  - PLANNED, Undated card/group → `unschedule` (or a same-group
 *    `reorderUndated`);
 *  - PLANNED, the column body → `none` (its tiny padding must never
 *    unschedule; the explicit Undated group is the visible target);
 *  - PLANNED, another status card → `leavePlanned`;
 *  - non-PLANNED, a dated card/group → `joinDated`;
 *  - non-PLANNED, Undated card/group/column body → `joinUndated`;
 *  - anything else → `none`.
 *
 *  A date group before `today` is readable but inert for DATE-CHANGING drops
 *  (every reschedule there would 400); a same-group reorder is an agenda
 *  `/move` and stays allowed even on a past day. Dropping on the active
 *  card/its own group body is a no-op. A rowless PLANNED task has no `item_id`,
 *  so date writes are refused rather than invented. */
export function resolvePlannedDrop({
  activeTaskId,
  activeStatus,
  activeGroup,
  over,
  groups,
  rows,
  tasks,
  today,
}: ResolvePlannedDropInput): PlannedDropDecision {
  const activeIsPlanned = activeStatus === 'PLANNED';
  const itemId = rows.get(activeTaskId)?.item_id ?? null;
  const undatedGroup = groups.find((group) => group.undated) ?? null;

  if (over.kind === 'card') {
    const target = tasks.get(over.taskId);
    if (!target || target.id === activeTaskId) return { kind: 'none' };

    if (!activeIsPlanned) {
      const targetGroup = plannedGroupOfTask(groups, target.id);
      if (!targetGroup) return { kind: 'none' };
      if (targetGroup.undated) {
        return {
          kind: 'joinUndated',
          targetRank: plannedBoardRankForDrop(
            tasksOfGroup(targetGroup, tasks),
            target.id,
            activeTaskId,
          ),
        };
      }
      if (isPastDate(targetGroup.date, today)) return { kind: 'none' };
      const targetRank = plannedAgendaRankForDrop(
        targetGroup,
        rows,
        target.id,
        activeTaskId,
      );
      return targetRank === undefined
        ? { kind: 'joinDated', group: targetGroup }
        : { kind: 'joinDated', group: targetGroup, targetRank };
    }

    // PLANNED active: another status's card leaves the column.
    if (target.status !== 'PLANNED') return { kind: 'leavePlanned' };
    const targetGroup = plannedGroupOfTask(groups, target.id);
    if (!targetGroup) return { kind: 'none' };

    if (targetGroup.undated) {
      if (activeGroup?.undated) {
        return {
          kind: 'reorderUndated',
          taskId: activeTaskId,
          targetRank: plannedBoardRankForDrop(
            tasksOfGroup(targetGroup, tasks),
            target.id,
            activeTaskId,
          ),
        };
      }
      return itemId ? { kind: 'unschedule', itemId } : { kind: 'none' };
    }

    // Same-group first: a reorder is the agenda `/move`, which has no date
    // floor. A PLANNED card may therefore reorder inside its OWN past group;
    // only a date-CHANGING drop into a past day is inert.
    if (activeGroup?.id === targetGroup.id) {
      if (!itemId) return { kind: 'none' };
      const targetRank = plannedAgendaRankForDrop(
        targetGroup,
        rows,
        target.id,
        activeTaskId,
      );
      return targetRank === undefined
        ? { kind: 'none' }
        : {
            kind: 'reorderDated',
            group: targetGroup,
            itemId,
            targetRank,
          };
    }

    if (isPastDate(targetGroup.date, today)) return { kind: 'none' };
    if (!itemId) return { kind: 'none' };
    const targetRank = plannedAgendaRankForDrop(
      targetGroup,
      rows,
      target.id,
      activeTaskId,
    );
    return targetRank === undefined
      ? { kind: 'moveToDate', group: targetGroup, itemId }
      : { kind: 'moveToDate', group: targetGroup, itemId, targetRank };
  }

  if (over.kind === 'group') {
    if (plannedGroupKeyFromId(over.groupId) === null) return { kind: 'none' };
    const group = groups.find((entry) => entry.id === over.groupId);
    if (!group) return { kind: 'none' };
    if (isPastDate(group.date, today)) return { kind: 'none' };

    if (group.undated) {
      if (activeIsPlanned) {
        if (activeGroup?.undated) return { kind: 'none' };
        return itemId ? { kind: 'unschedule', itemId } : { kind: 'none' };
      }
      return {
        kind: 'joinUndated',
        targetRank: plannedBoardRankForDrop(
          tasksOfGroup(group, tasks),
          null,
          activeTaskId,
        ),
      };
    }

    if (!activeIsPlanned) {
      return { kind: 'joinDated', group };
    }
    if (activeGroup?.id === group.id) return { kind: 'none' };
    return itemId ? { kind: 'moveToDate', group, itemId } : { kind: 'none' };
  }

  // Column body. For a PLANNED card this is the tiny scroll padding around
  // the groups, not a scheduling target: only the explicit Undated group
  // unschedules. A non-PLANNED card joins the Undated warehouse as before.
  if (activeIsPlanned) return { kind: 'none' };
  return {
    kind: 'joinUndated',
    targetRank: plannedBoardRankForDrop(
      tasksOfGroup(undatedGroup, tasks),
      null,
      activeTaskId,
    ),
  };
}

// ──────────────────────────────────────────
// Optimistic cache helpers — immutable edits over the read's `rows`.
// Every helper returns the SAME array when the write changes nothing, so
// React Query can skip the re-render.
// ──────────────────────────────────────────

/** Mirrors the agenda `/move` within one dated group: every peer on `date`
 *  ranked at/after `targetRank` shifts up one, then the mover lands on
 *  `targetRank`. Rows on other dates, undated rows and rows without an agenda
 *  rank are untouched. A missing mover returns the input unchanged. */
export function applyPlannedAgendaMove(
  rows: PlannedBoardRow[],
  date: string,
  itemId: string,
  targetRank: number,
): PlannedBoardRow[] {
  const moverIndex = rows.findIndex(
    (row) =>
      row.item_id === itemId &&
      !row.undated &&
      row.local_date === date &&
      row.agenda_sort_order !== null,
  );
  if (moverIndex === -1) return rows;

  let changed = false;
  const next = rows.map((row, index) => {
    if (index === moverIndex) {
      if (row.agenda_sort_order === targetRank) return row;
      changed = true;
      return { ...row, agenda_sort_order: targetRank };
    }
    if (
      !row.undated &&
      row.local_date === date &&
      row.item_id !== itemId &&
      row.agenda_sort_order !== null &&
      row.agenda_sort_order >= targetRank
    ) {
      changed = true;
      return { ...row, agenda_sort_order: row.agenda_sort_order + 1 };
    }
    return row;
  });
  return changed ? next : rows;
}

/** Mirrors the Board's same-status reorder (`reorder_in_place`,
 *  packages/api-core/src/tasks.rs) over the WHOLE Planned column rank space:
 *  `tasks.sort_order` is shared by dated and undated rows.
 *
 *  - `new < old`: peers with `new <= rank < old` shift +1;
 *  - `new > old`: peers with `old < rank <= new` shift −1;
 *  - the mover lands on `new`.
 *
 *  Only the Undated group displays this rank, but the server still stores it
 *  on dated rows. Equal ranks / a missing mover return the input unchanged. */
export function applyPlannedBoardReorder(
  rows: PlannedBoardRow[],
  taskId: string,
  targetRank: number,
): PlannedBoardRow[] {
  const moverIndex = rows.findIndex((row) => row.task_id === taskId);
  if (moverIndex === -1) return rows;
  const old = rows[moverIndex].board_sort_order;
  if (old === targetRank) return rows;

  return rows.map((row, index) => {
    if (index === moverIndex) {
      return { ...row, board_sort_order: targetRank };
    }
    if (targetRank < old) {
      return row.board_sort_order >= targetRank && row.board_sort_order < old
        ? { ...row, board_sort_order: row.board_sort_order + 1 }
        : row;
    }
    return row.board_sort_order > old && row.board_sort_order <= targetRank
      ? { ...row, board_sort_order: row.board_sort_order - 1 }
      : row;
  });
}

/** Mirrors `reschedule { date, sort_order? }` for one row: it leaves its old
 *  date grouping (`local_date` becomes `date`, `undated` false) while keeping
 *  its Board rank and ids. With a `targetRank`, target-date peers at/after it
 *  shift up one; without one, the row appends at `max target-date agenda rank
 *  + 1` (0 on an empty day). A missing item id returns the input unchanged. */
export function movePlannedRowToDate(
  rows: PlannedBoardRow[],
  itemId: string,
  date: string,
  targetRank?: number,
): PlannedBoardRow[] {
  const moverIndex = rows.findIndex((row) => row.item_id === itemId);
  if (moverIndex === -1) return rows;

  let rank: number;
  if (targetRank === undefined) {
    let max: number | null = null;
    for (const row of rows) {
      if (
        row.undated ||
        row.local_date !== date ||
        row.agenda_sort_order === null
      ) {
        continue;
      }
      max =
        max === null
          ? row.agenda_sort_order
          : Math.max(max, row.agenda_sort_order);
    }
    rank = max === null ? 0 : max + 1;
  } else {
    rank = targetRank;
  }

  let changed = false;
  const next = rows.map((row, index) => {
    if (index === moverIndex) {
      if (
        row.local_date === date &&
        !row.undated &&
        row.agenda_sort_order === rank
      ) {
        return row;
      }
      changed = true;
      return {
        ...row,
        local_date: date,
        undated: false,
        agenda_sort_order: rank,
      };
    }
    if (
      targetRank !== undefined &&
      !row.undated &&
      row.local_date === date &&
      row.agenda_sort_order !== null &&
      row.agenda_sort_order >= targetRank
    ) {
      changed = true;
      return { ...row, agenda_sort_order: row.agenda_sort_order + 1 };
    }
    return row;
  });
  return changed ? next : rows;
}

/** Mirrors `reschedule { unschedule: true }` optimistically: the row parks in
 *  Undated without an agenda rank. `local_date` is kept because the server's
 *  sentinel date is unknown to the client, and `board_sort_order` still decides
 *  its place in the Undated group. */
export function markPlannedRowUndated(
  rows: PlannedBoardRow[],
  itemId: string,
): PlannedBoardRow[] {
  const moverIndex = rows.findIndex((row) => row.item_id === itemId);
  if (moverIndex === -1) return rows;
  const mover = rows[moverIndex];
  if (mover.undated && mover.agenda_sort_order === null) return rows;
  const next = rows.slice();
  next[moverIndex] = {
    ...mover,
    undated: true,
    agenda_sort_order: null,
  };
  return next;
}

/** Drops a task's row (a status-only move out of Planned). Missing ids return
 *  the input unchanged. */
export function removePlannedTask(
  rows: PlannedBoardRow[],
  taskId: string,
): PlannedBoardRow[] {
  const next = rows.filter((row) => row.task_id !== taskId);
  return next.length === rows.length ? rows : next;
}

function samePlannedRow(a: PlannedBoardRow, b: PlannedBoardRow): boolean {
  return (
    a.task_id === b.task_id &&
    a.item_id === b.item_id &&
    a.local_date === b.local_date &&
    a.agenda_sort_order === b.agenda_sort_order &&
    a.board_sort_order === b.board_sort_order &&
    a.undated === b.undated
  );
}

/** Replaces the row with the same `task_id`, or appends it when missing. An
 *  identical row keeps the input array reference. */
export function upsertPlannedRow(
  rows: PlannedBoardRow[],
  row: PlannedBoardRow,
): PlannedBoardRow[] {
  const index = rows.findIndex((entry) => entry.task_id === row.task_id);
  if (index === -1) return [...rows, row];
  if (rows[index] === row || samePlannedRow(rows[index], row)) return rows;
  const next = rows.slice();
  next[index] = row;
  return next;
}

/** Builds a dated row from the server's reschedule/refresh result. The caller
 *  only uses it for dated outcomes — an unschedule must call
 *  `markPlannedRowUndated` (or keep the server's own `undated` flag). The
 *  Board rank is the task's own `sort_order` when present, else the caller's
 *  fallback, else 0. */
export function plannedRowFromItem(
  item: AgendaItemRecord,
  boardSortOrder?: number,
): PlannedBoardRow {
  return {
    task_id: item.ref_id,
    item_id: item.id,
    local_date: item.local_date,
    agenda_sort_order: item.sort_order,
    board_sort_order: item.task?.sort_order ?? boardSortOrder ?? 0,
    undated: false,
  };
}
