// Home — the agenda as an infinite stream of days (ADR 0004 amendment, slice
// 5). One vertically scrolling run of civil days inside its own scroll
// container: opening the page lands on the server's `anchor_date` (the oldest
// past day that still has something pending), scroll UP for history, DOWN for
// the future. Each day keeps the two piles (ADR 0004 amendment — living rows
// ranked by `sort_order`, then the muted Completed dump), plus a sticky
// header; a past day with pending work shows an amber count and a one-tap
// "Move all to today". Dragging a living row onto today or a later day
// reschedules it (`/reschedule` with the target rank); a same-day drop is the
// old `/move` reorder. The date picker, the add-task picker and
// remove-from-day are gone — tasks are born on the Board, and a task's row
// follows the task's own lifecycle. Built on the existing task/occurrence
// actions (complete, skip, reopen, start, pause, rename, TaskModal, error
// banner, optimistic `['tasks']` patches) and reschedule/complete art.

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from 'react';
import { Link } from '@tanstack/react-router';
import {
  DndContext,
  DragOverlay,
  MouseSensor,
  TouchSensor,
  closestCenter,
  useDroppable,
  useSensor,
  useSensors,
  type DragEndEvent,
} from '@dnd-kit/core';
import {
  SortableContext,
  verticalListSortingStrategy,
} from '@dnd-kit/sortable';
import { Loader2, Repeat } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from '@/components/ui/dialog';
import { FocusTimer } from '@/app/components/FocusTimer';
import { QuotesSection } from '@/app/components/QuotesSection';
import { TaskModal } from '@/app/components/TaskModal';
import { quotes } from '@/app/mock-data';
import { moveDayToToday } from '@/lib/api';
import {
  useAgendaStream,
  useMoveAgendaItem,
  useRescheduleAgendaItem,
  type AgendaStreamData,
} from '@/app/queries/agenda';
import { queryKeys } from '@/app/queries/keys';
import {
  useCompleteOccurrence,
  usePauseOccurrence,
  useReopenOccurrence,
  useSkipOccurrence,
  useStartOccurrence,
  useUpdateOccurrence,
} from '@/app/queries/occurrences';
import {
  setTasksCache,
  useDeleteTask,
  useMoveTask,
  useRunTaskAction,
  useUpdateTask,
} from '@/app/queries/tasks';
import { queryClient } from '@/lib/queryClient';
import { AgendaItemRow, SortableAgendaItemRow } from './AgendaItemRow';
import {
  agendaDateLabel,
  agendaMoveTargetAt,
  appendRankOnStream,
  applyAgendaMove,
  eachCivilDate,
  groupItemsByDate,
  isAgendaItemPending,
  moveStreamItem,
  partitionAgendaItems,
  streamBounds,
  streamItemsOf,
  updateStreamItem,
} from './agenda-helpers';
import type {
  AgendaItemRecord,
  MoveTaskInput,
  OccurrenceStatus,
  RescheduleAgendaItemInput,
  TaskDifficulty,
  TaskPriority,
  TaskRecord,
  TaskStatus,
  UpdateTaskInput,
} from '@/app/types';

/** Every day's drop container id (`day-2026-08-23`) — also the day section's
 *  DOM id, which is how the initial scroll and the Today button find a day. */
const DAY_ID_PREFIX = 'day-';
/** Consecutive auto-loads of the future allowed while the bottom sentinel
 *  stays in view (the future is unbounded — a scroll re-arms this). */
const MAX_BOTTOM_AUTOLOADS = 3;
/** Hard cap on the Today button's crawl to an unloaded today. */
const MAX_TODAY_FETCHES = 30;

function dayElementId(date: string): string {
  return `${DAY_ID_PREFIX}${date}`;
}

/** Home's slim, compact copy of the FocusTimer for below `lg` — same
 *  component, constrained by classes only (heading to screen-readers, digits
 *  and buttons shrunk, session line dropped). */
const COMPACT_TIMER_CLASSES =
  'p-2.5 [&_h2]:sr-only [&>div:nth-child(2)]:mb-2 [&>div:nth-child(2)>div:first-child]:text-2xl [&>div:nth-child(2)>p]:hidden [&>div:nth-child(3)]:mb-0 [&>div:last-child]:hidden';

interface AgendaRowHandlers {
  onReschedule: (item: AgendaItemRecord, date: string) => void;
  onCompleteTask: (item: AgendaItemRecord) => void;
  onStartTask: (item: AgendaItemRecord) => void;
  onPauseTask: (item: AgendaItemRecord) => void;
  onOpenTask: (task: TaskRecord) => void;
  onCompleteOccurrence: (item: AgendaItemRecord) => void;
  onSkipOccurrence: (item: AgendaItemRecord) => void;
  onStartOccurrence: (item: AgendaItemRecord) => void;
  onPauseOccurrence: (item: AgendaItemRecord) => void;
  onRenameOccurrence: (item: AgendaItemRecord) => void;
}

/** Can a drag grab this row? Living work only: open tasks (OPEN | PLANNED |
 *  IN_PROGRESS) and waiting/running occurrences of a LIVING routine. A
 *  skipped occurrence is a decline (no grip), terminal tasks and
 *  `routine_deleted` history are not draggable at all, and orphans have
 *  nothing to drag. */
function isAgendaRowDraggable(item: AgendaItemRecord): boolean {
  const occurrence = item.occurrence;
  if (item.kind === 'occurrence' && occurrence) {
    if (occurrence.routine_deleted) return false;
    return (
      occurrence.status === 'pending' || occurrence.status === 'in_progress'
    );
  }
  return isAgendaItemPending(item);
}

/** A past day's leftovers that `move-to-today` will actually sweep: waiting
 *  tasks and `pending` occurrences. An `in_progress` OCCURRENCE stays put
 *  (its timer is still running — the API keeps it too); an `in_progress` task
 *  moves, exactly like the API. */
function canMoveToToday(item: AgendaItemRecord): boolean {
  return isAgendaItemPending(item) && item.occurrence?.status !== 'in_progress';
}

/** One day of the stream: sticky header (with the past-day catch-up), the
 *  living pile as its own droppable + sortable list (past days are not drop
 *  targets), then the Completed dump — never draggable, never a drop
 *  target. */
function AgendaDay({
  day,
  serverToday,
  items,
  handlers,
  onMoveDayToToday,
}: {
  day: string;
  serverToday: string;
  items: AgendaItemRecord[];
  handlers: AgendaRowHandlers;
  onMoveDayToToday: (day: string) => void;
}) {
  const { living, completed } = partitionAgendaItems(items);
  const pendingCount = items.filter(isAgendaItemPending).length;
  const isPast = !!serverToday && day < serverToday;
  const draggable = living.filter(isAgendaRowDraggable);
  const { setNodeRef, isOver } = useDroppable({
    id: dayElementId(day),
    data: { day },
    // The API rejects a reschedule into the past — a past day never accepts
    // a drop.
    disabled: isPast,
  });
  const empty = items.length === 0;

  return (
    <section id={dayElementId(day)}>
      <header
        className={
          empty
            ? 'sticky top-0 z-10 flex items-center gap-2 border-b border-border/50 bg-cream/95 px-1 py-1.5 backdrop-blur'
            : 'sticky top-0 z-10 flex items-center gap-2 border-b border-border/60 bg-cream/95 px-1 py-2 backdrop-blur'
        }
      >
        <h2
          className={
            empty
              ? 'font-heading text-xs font-medium text-muted-foreground'
              : 'font-heading text-sm font-semibold text-foreground'
          }
        >
          {agendaDateLabel(day, serverToday)}
        </h2>
        {isPast && pendingCount > 0 && (
          <>
            <span className="flex-shrink-0 rounded-full bg-amber-400/20 px-1.5 py-0.5 text-[10px] font-semibold text-amber-700">
              {pendingCount} pending
            </span>
            <button
              type="button"
              onClick={() => onMoveDayToToday(day)}
              aria-label={`Move all pending items from ${day} to today`}
              className="ml-auto flex-shrink-0 rounded-full bg-amber-400/15 px-2 py-1 text-[11px] font-medium text-amber-700 transition-colors hover:bg-amber-400/25"
            >
              Move all to today
            </button>
          </>
        )}
      </header>

      <SortableContext
        items={draggable.map((entry) => entry.id)}
        strategy={verticalListSortingStrategy}
      >
        <div
          ref={setNodeRef}
          className={
            isOver
              ? 'min-h-10 rounded-lg bg-primary/5 px-1 py-2 ring-1 ring-primary/20 transition-colors'
              : 'min-h-10 px-1 py-2 transition-colors'
          }
        >
          {living.length === 0 ? (
            <p className="px-1 py-1.5 text-sm italic text-muted-foreground/70">
              Nothing planned
            </p>
          ) : (
            <div className="space-y-2">
              {living.map((entry) =>
                isAgendaRowDraggable(entry) ? (
                  <SortableAgendaItemRow
                    key={entry.id}
                    item={entry}
                    serverToday={serverToday}
                    {...handlers}
                  />
                ) : (
                  <AgendaItemRow
                    key={entry.id}
                    item={entry}
                    isDragging={false}
                    showHandle={false}
                    serverToday={serverToday}
                    {...handlers}
                  />
                ),
              )}
            </div>
          )}
        </div>
      </SortableContext>

      {completed.length > 0 && (
        <section className="space-y-2 px-1 pb-2">
          <h3 className="px-1 text-xs font-medium tracking-wide text-muted-foreground">
            Completed
          </h3>
          <div className="space-y-2">
            {completed.map((entry) => (
              <AgendaItemRow
                key={entry.id}
                item={entry}
                isDragging={false}
                showHandle={false}
                serverToday={serverToday}
                {...handlers}
              />
            ))}
          </div>
        </section>
      )}
    </section>
  );
}

export function HomePage() {
  // The stream: page 0 is the server's anchor week (the GET omits `from`/
  // `to` entirely, so the browser never computes today), `pages` grows down
  // for the future and up for history.
  const stream = useAgendaStream();
  const pages = useMemo(() => stream.data?.pages ?? [], [stream.data]);
  const firstPage = pages[0];
  // The server's civil today (primary calendar time zone) — refreshed from
  // every read; anchors the day headers, the Play gate, the reschedule
  // floor and the Today button.
  const serverToday = firstPage?.today ?? '';
  const anchorDate = firstPage?.anchor_date ?? '';
  const bounds = useMemo(() => streamBounds(pages), [pages]);
  const days = useMemo(
    () => (bounds ? eachCivilDate(bounds.from, bounds.to) : []),
    [bounds],
  );
  const items = useMemo(() => pages.flatMap((page) => page.items), [pages]);
  const byDate = useMemo(() => groupItemsByDate(items), [items]);

  // Action failures (move/complete/skip/etc.): a banner above the still-
  // visible stream — rows are never unmounted by an action error.
  const [actionError, setActionError] = useState<string | null>(null);
  // The row currently dragged — feeds the DragOverlay title chip.
  const [activeDrag, setActiveDrag] = useState<AgendaItemRecord | null>(null);

  const [taskModal, setTaskModal] = useState<TaskRecord | null>(null);
  // The renamed occurrence's whole agenda row: the item id is what the
  // optimistic merge needs (an occurrence id alone cannot address a row).
  const [renaming, setRenaming] = useState<AgendaItemRecord | null>(null);
  const [renameTitle, setRenameTitle] = useState('');
  const [renameError, setRenameError] = useState<string | null>(null);
  const [renameSaving, setRenameSaving] = useState(false);

  // ──────────────────────────────────────────
  // Cache plumbing
  // ──────────────────────────────────────────

  /** The stream as it is RIGHT NOW (never the render-time snapshot): a drag
   *  can land after a refetch resolved, and the optimistic paint must build
   *  on what is actually cached. */
  const readStream = useCallback(
    (): AgendaStreamData | undefined =>
      queryClient.getQueryData<AgendaStreamData>(queryKeys.agenda.stream()),
    [],
  );

  const patchStream = useCallback(
    (
      updater: (
        current: AgendaStreamData | undefined,
      ) => AgendaStreamData | undefined,
    ) => {
      queryClient.setQueryData<AgendaStreamData>(
        queryKeys.agenda.stream(),
        updater,
      );
    },
    [],
  );

  /** Optimistic mirror of the server's `place_task_on_today` (ADR 0004
   *  amendment): a task's single row travels to civil today on a real
   *  lifecycle transition (start / complete / discard / reopen), appended at
   *  the pile's end; a row already there keeps its rank. `fn` paints the
   *  embed flip first (so `partitionAgendaItems` parks a completed row in the
   *  Completed dump). */
  const placeTaskOnToday = useCallback(
    (
      current: AgendaStreamData | undefined,
      itemId: string,
      fn: (item: AgendaItemRecord) => AgendaItemRecord,
    ): AgendaStreamData | undefined => {
      const next = updateStreamItem(current, itemId, fn);
      const row = streamItemsOf(next).find((entry) => entry.id === itemId);
      if (!row || !serverToday || row.local_date === serverToday) return next;
      return moveStreamItem(
        next,
        itemId,
        serverToday,
        appendRankOnStream(next, serverToday),
      );
    },
    [serverToday],
  );

  const mergeRow = useCallback(
    (itemId: string, updater: (item: AgendaItemRecord) => AgendaItemRecord) => {
      patchStream((current) => updateStreamItem(current, itemId, updater));
    },
    [patchStream],
  );

  // ──────────────────────────────────────────
  // Reorder / reschedule
  // ──────────────────────────────────────────

  const moveAgendaItemMutation = useMoveAgendaItem();
  const rescheduleAgendaItemMutation = useRescheduleAgendaItem();

  /** dnd-kit drop resolution. Same-day → the old `/move` reorder; across days
   *  → `/reschedule` with the target day's rank. Past days are never drop
   *  targets (the API 400s), and an `in_progress` occurrence may only reorder
   *  inside its own day. */
  const handleDragEnd = (event: DragEndEvent) => {
    setActiveDrag(null);
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const activeId = String(active.id);
    const overId = String(over.id);
    const itemsNow = streamItemsOf(readStream());
    const dragged = itemsNow.find((entry) => entry.id === activeId);
    if (!dragged) return;

    // The on-screen sortable list holds the DRAGGABLE rows only — dnd-kit
    // resolves a same-day drop index against that, never the full pile
    // (parked and read-only rows are not in it).
    const sortableSameDay = () =>
      partitionAgendaItems(
        dayItemsOf(itemsNow, dragged.local_date),
      ).living.filter(isAgendaRowDraggable);
    // A cross-day rank is "the index in the target day's living list" — the
    // pile as the user reads it (read-only rows included), end of the list
    // for a drop on the day container itself.
    const crossDayRank = (day: string, overItemId?: string) => {
      const living = partitionAgendaItems(dayItemsOf(itemsNow, day)).living;
      const index = overItemId
        ? living.findIndex((entry) => entry.id === overItemId)
        : living.length;
      return index < 0 ? living.length : index;
    };

    // A drop on a day's own container means "the end of that day's pile".
    if (overId.startsWith(DAY_ID_PREFIX)) {
      const day = overId.slice(DAY_ID_PREFIX.length);
      if (day === dragged.local_date) {
        const sortable = sortableSameDay();
        const target = agendaMoveTargetAt(
          sortable,
          sortable.findIndex((entry) => entry.id === dragged.id),
          sortable.length - 1,
        );
        if (target !== null) void handleSameDayMove(dragged, target);
        return;
      }
      if (day < serverToday) return;
      void handleCrossDayMove(dragged, day, crossDayRank(day));
      return;
    }

    const overItem = itemsNow.find((entry) => entry.id === overId);
    if (!overItem) return;
    if (overItem.local_date === dragged.local_date) {
      const sortable = sortableSameDay();
      const target = agendaMoveTargetAt(
        sortable,
        sortable.findIndex((entry) => entry.id === dragged.id),
        sortable.findIndex((entry) => entry.id === overId),
      );
      if (target === null) return;
      void handleSameDayMove(dragged, target);
      return;
    }
    if (overItem.local_date < serverToday) return;
    void handleCrossDayMove(
      dragged,
      overItem.local_date,
      crossDayRank(overItem.local_date, overId),
    );
  };

  /** Same-day reorder → `POST /api/agenda/items/:id/move { sort_order }`.
   *  The optimistic paint is `applyAgendaMove` over the day's WHOLE pile
   *  (parked ranks shift too, exactly like the server); a failure restores
   *  the snapshot and banners. */
  const handleSameDayMove = async (item: AgendaItemRecord, target: number) => {
    const snapshot = readStream();
    const day = item.local_date;
    const dayItems = dayItemsOf(streamItemsOf(snapshot), day);
    const moved = applyAgendaMove(dayItems, item.id, target);
    // Rank gaps make `applyAgendaMove` a no-op when the row already holds the
    // target rank — nothing to POST either.
    if (moved === dayItems) return;
    patchStream((current) =>
      moved.reduce(
        (acc, entry) => updateStreamItem(acc, entry.id, () => entry),
        current,
      ),
    );
    setActionError(null);
    try {
      const data = await moveAgendaItemMutation.mutateAsync({
        id: item.id,
        input: { sort_order: target },
      });
      // Merge the authoritative row (fresh embeds); ranks already match.
      mergeRow(data.item.id, () => data.item);
    } catch (err) {
      patchStream(() => snapshot);
      setActionError(err instanceof Error ? err.message : 'Move failed');
    }
  };

  /** Move a row onto another day (drag across days, or the row's own
   *  reschedule popover). `sortOrder` is the rank the drop wants (the target
   *  day's living index); omitted — the popover path — appends at the pile's
   *  end server-side, exactly like the old "Tomorrow". A failure restores the
   *  snapshot and banners. */
  const handleCrossDayMove = async (
    item: AgendaItemRecord,
    targetDate: string,
    sortOrder?: number,
  ) => {
    if (!targetDate || targetDate === item.local_date) return;
    if (!serverToday || targetDate < serverToday) return;
    // `in_progress` occurrences are reorder-only until they are paused.
    if (item.occurrence?.status === 'in_progress') return;
    const snapshot = readStream();
    const input: RescheduleAgendaItemInput =
      sortOrder === undefined
        ? { date: targetDate }
        : { date: targetDate, sort_order: sortOrder };
    patchStream((current) =>
      moveStreamItem(
        current,
        item.id,
        targetDate,
        sortOrder ?? appendRankOnStream(current, targetDate),
      ),
    );
    setActionError(null);
    try {
      const data = await rescheduleAgendaItemMutation.mutateAsync({
        id: item.id,
        input,
      });
      mergeRow(data.item.id, () => data.item);
    } catch (err) {
      patchStream(() => snapshot);
      setActionError(err instanceof Error ? err.message : 'Move failed');
    }
  };

  /** "Move all to today" on a past day: the server sweeps the row set
   *  `POST /api/agenda/days/:date/move-to-today` sweeps, appended to today's
   *  pile in their original order. Painted optimistically, reverted on
   *  failure, and re-read on settle (the sweep has server-side side effects a
   *  local mirror cannot guess). */
  const handleMoveDayToToday = async (day: string) => {
    const snapshot = readStream();
    const movable = dayItemsOf(streamItemsOf(snapshot), day).filter(
      canMoveToToday,
    );
    if (movable.length === 0) return;
    patchStream((current) => {
      let next = current;
      let rank = appendRankOnStream(current, serverToday);
      for (const entry of movable) {
        next = moveStreamItem(next, entry.id, serverToday, rank);
        rank += 1;
      }
      return next;
    });
    setActionError(null);
    try {
      await moveDayToToday(day);
    } catch (err) {
      patchStream(() => snapshot);
      setActionError(
        err instanceof Error ? err.message : 'Could not move the day',
      );
    } finally {
      void queryClient.invalidateQueries({ queryKey: queryKeys.agenda.all });
    }
  };

  // ──────────────────────────────────────────
  // Task verbs (complete / reopen / start / pause)
  // ──────────────────────────────────────────

  const runTaskActionMutation = useRunTaskAction();
  const updateTaskMutation = useUpdateTask();
  const deleteTaskMutation = useDeleteTask();
  const moveTaskMutation = useMoveTask();

  /** Task → `/complete` (parks the row under Completed right away, with a
   *  fresh `updated_at` stamp so the dump's newest-first sort jumps it to the
   *  top) or, for a checked-off row, back to PLANNED through the Board's
   *  `/move` (ADR 0004 amendment — uncheck is the Board reopen, no new
   *  endpoint). Both travel onto civil today server-side — the paint mirrors
   *  that. Unchecking never stamps: living sorts by `sort_order`. */
  const handleCompleteTask = async (item: AgendaItemRecord) => {
    const task = item.task;
    if (!task) return;
    const snapshot = readStream();
    // Uncheck: COMPLETED → PLANNED (living again — Play/Reschedule return
    // because the row is no longer a finished card).
    if (task.status === 'COMPLETED') {
      patchStream((current) =>
        placeTaskOnToday(current, item.id, (entry) =>
          entry.task
            ? { ...entry, task: { ...entry.task, status: 'PLANNED' } }
            : entry,
        ),
      );
      setActionError(null);
      try {
        const data = await moveTaskMutation.mutateAsync({
          id: task.id,
          input: { status: 'PLANNED' } satisfies MoveTaskInput,
        });
        mergeRow(item.id, (entry) =>
          entry.task ? { ...entry, task: data.task } : entry,
        );
        // Patch the shared tasks cache so Board/Lists see the fresh row.
        setTasksCache((prev) =>
          prev.map((entry) => (entry.id === data.task.id ? data.task : entry)),
        );
      } catch (err) {
        patchStream(() => snapshot);
        setActionError(err instanceof Error ? err.message : 'Uncheck failed');
      }
      return;
    }
    patchStream((current) =>
      placeTaskOnToday(current, item.id, (entry) =>
        entry.task
          ? {
              ...entry,
              task: {
                ...entry.task,
                status: 'COMPLETED',
                // Fresh stamp → the Completed dump sorts this row first.
                updated_at: new Date().toISOString(),
              },
            }
          : entry,
      ),
    );
    setActionError(null);
    try {
      const data = await runTaskActionMutation.mutateAsync({
        id: task.id,
        action: 'complete',
      });
      mergeRow(item.id, (entry) =>
        entry.task ? { ...entry, task: data.task } : entry,
      );
      // Patch the shared tasks cache so Board/Lists see the fresh row.
      setTasksCache((prev) =>
        prev.map((entry) => (entry.id === data.task.id ? data.task : entry)),
      );
    } catch (err) {
      patchStream(() => snapshot);
      setActionError(err instanceof Error ? err.message : 'Complete failed');
    }
  };

  /** Task → the existing `/start` (Board → In Progress). Its row travels onto
   *  civil today server-side; the FocusTimer is a separate surface and this
   *  just flips the card. */
  const handleStartTask = async (item: AgendaItemRecord) => {
    const task = item.task;
    if (!task) return;
    const snapshot = readStream();
    patchStream((current) =>
      placeTaskOnToday(current, item.id, (entry) =>
        entry.task
          ? { ...entry, task: { ...entry.task, status: 'IN_PROGRESS' } }
          : entry,
      ),
    );
    setActionError(null);
    try {
      const data = await runTaskActionMutation.mutateAsync({
        id: task.id,
        action: 'start',
      });
      mergeRow(item.id, (entry) =>
        entry.task ? { ...entry, task: data.task } : entry,
      );
      // Patch the shared tasks cache so Board/Lists see the fresh row.
      setTasksCache((prev) =>
        prev.map((entry) => (entry.id === data.task.id ? data.task : entry)),
      );
    } catch (err) {
      patchStream(() => snapshot);
      setActionError(err instanceof Error ? err.message : 'Start failed');
    }
  };

  /** Task → `/pause` (ADR 0002: IN_PROGRESS → PLANNED, event end patched
   *  closed). Pause never moves the row's day; Play returns once the row is
   *  planned again. Occurrence pause is a separate verb. */
  const handlePauseTask = async (item: AgendaItemRecord) => {
    const task = item.task;
    if (!task) return;
    setActionError(null);
    try {
      const data = await runTaskActionMutation.mutateAsync({
        id: task.id,
        action: 'pause',
      });
      mergeRow(item.id, (entry) =>
        entry.task ? { ...entry, task: data.task } : entry,
      );
      setTasksCache((prev) =>
        prev.map((entry) => (entry.id === data.task.id ? data.task : entry)),
      );
    } catch (err) {
      setActionError(err instanceof Error ? err.message : 'Pause failed');
    }
  };

  // ──────────────────────────────────────────
  // Occurrence verbs (complete / skip / reopen / start / pause)
  // ──────────────────────────────────────────

  const completeOccurrenceMutation = useCompleteOccurrence();
  const skipOccurrenceMutation = useSkipOccurrence();
  const reopenOccurrenceMutation = useReopenOccurrence();
  const startOccurrenceMutation = useStartOccurrence();
  const pauseOccurrenceMutation = usePauseOccurrence();
  const updateOccurrenceMutation = useUpdateOccurrence();

  /** Occurrence complete/skip — one shared optimistic path over the two
   *  verbs (the verb matrix is server-side; the UI just flips the chip).
   *  Complete ALSO stamps a fresh `updated_at` on the embed so the row
   *  parks at the top of the Completed dump; skip stays living (sorted by
   *  `sort_order`), so its stamp is untouched. An occurrence row never
   *  changes its day. */
  const setOccurrenceStatus = async (
    item: AgendaItemRecord,
    status: OccurrenceStatus,
    verb: 'complete' | 'skip',
  ) => {
    const occurrence = item.occurrence;
    if (!occurrence) return;
    const snapshot = readStream();
    patchStream((current) =>
      updateStreamItem(current, item.id, (entry) =>
        entry.occurrence
          ? {
              ...entry,
              occurrence: {
                ...entry.occurrence,
                status,
                updated_at:
                  verb === 'complete'
                    ? new Date().toISOString()
                    : entry.occurrence.updated_at,
              },
            }
          : entry,
      ),
    );
    setActionError(null);
    try {
      const data =
        verb === 'complete'
          ? await completeOccurrenceMutation.mutateAsync({ id: occurrence.id })
          : await skipOccurrenceMutation.mutateAsync({ id: occurrence.id });
      mergeRow(item.id, (entry) =>
        entry.occurrence ? { ...entry, occurrence: data.occurrence } : entry,
      );
    } catch (err) {
      patchStream(() => snapshot);
      setActionError(err instanceof Error ? err.message : 'Update failed');
    }
  };

  /** Occurrence reopen — the uncheck/unskip path (ADR 0004 amendment):
   *  done/skipped → pending with the chip ids cleared (`calendar_id` +
   *  `google_event_id` → null, matching the server's reopen). Same
   *  optimistic shape as setOccurrenceStatus: paint pending immediately,
   *  merge the authoritative occurrence on success, snapshot-rollback +
   *  banner on failure. */
  const handleReopenOccurrence = async (item: AgendaItemRecord) => {
    const occurrence = item.occurrence;
    if (!occurrence) return;
    const snapshot = readStream();
    patchStream((current) =>
      updateStreamItem(current, item.id, (entry) =>
        entry.occurrence
          ? {
              ...entry,
              occurrence: {
                ...entry.occurrence,
                status: 'pending',
                calendar_id: null,
                google_event_id: null,
              },
            }
          : entry,
      ),
    );
    setActionError(null);
    try {
      const data = await reopenOccurrenceMutation.mutateAsync({
        id: occurrence.id,
      });
      mergeRow(item.id, (entry) =>
        entry.occurrence ? { ...entry, occurrence: data.occurrence } : entry,
      );
    } catch (err) {
      patchStream(() => snapshot);
      setActionError(err instanceof Error ? err.message : 'Update failed');
    }
  };

  /** Occurrence start: creates the one-shot Google log and flips the chip to
   *  in_progress. Today-only on the server (the row only offers Play on
   *  today). Optimistic → the chip reads In progress immediately; a 401/400
   *  rolls back with a banner. */
  const handleStartOccurrence = async (item: AgendaItemRecord) => {
    const occurrence = item.occurrence;
    if (!occurrence) return;
    const snapshot = readStream();
    patchStream((current) =>
      updateStreamItem(current, item.id, (entry) =>
        entry.occurrence
          ? {
              ...entry,
              occurrence: { ...entry.occurrence, status: 'in_progress' },
            }
          : entry,
      ),
    );
    setActionError(null);
    try {
      const data = await startOccurrenceMutation.mutateAsync({
        id: occurrence.id,
      });
      mergeRow(item.id, (entry) =>
        entry.occurrence ? { ...entry, occurrence: data.occurrence } : entry,
      );
    } catch (err) {
      patchStream(() => snapshot);
      setActionError(err instanceof Error ? err.message : 'Start failed');
    }
  };

  /** Occurrence pause (ADR 0004 amendment): close the living chip and
   *  return to pending with ids cleared. Optimistic → Play returns
   *  immediately; a 401/400 rolls back with a banner. */
  const handlePauseOccurrence = async (item: AgendaItemRecord) => {
    const occurrence = item.occurrence;
    if (!occurrence) return;
    const snapshot = readStream();
    patchStream((current) =>
      updateStreamItem(current, item.id, (entry) =>
        entry.occurrence
          ? {
              ...entry,
              occurrence: {
                ...entry.occurrence,
                status: 'pending',
                calendar_id: null,
                google_event_id: null,
              },
            }
          : entry,
      ),
    );
    setActionError(null);
    try {
      const data = await pauseOccurrenceMutation.mutateAsync({
        id: occurrence.id,
      });
      mergeRow(item.id, (entry) =>
        entry.occurrence ? { ...entry, occurrence: data.occurrence } : entry,
      );
    } catch (err) {
      patchStream(() => snapshot);
      setActionError(err instanceof Error ? err.message : 'Pause failed');
    }
  };

  // ──────────────────────────────────────────
  // TaskModal (existing component — PATCH / DELETE / move)
  // ──────────────────────────────────────────

  /** Merge a fresh task embed into its one membership row (the id is stable:
   *  a task owns exactly one row). */
  const mergeTask = (task: TaskRecord) => {
    const row = streamItemsOf(readStream()).find(
      (entry) => entry.task?.id === task.id,
    );
    if (!row) return;
    mergeRow(row.id, (entry) => (entry.task ? { ...entry, task } : entry));
  };

  const handleTaskModalSubmit = async (values: {
    title: string;
    description: string;
    durationMinutes: number;
    priority: TaskPriority;
    difficulty: TaskDifficulty;
  }): Promise<string | null> => {
    if (!taskModal) return null;
    setActionError(null);
    try {
      // Merge the fresh embed straight into the rows (same refresh the
      // agenda reload would give, without the spinner flash).
      const data = await updateTaskMutation.mutateAsync({
        id: taskModal.id,
        input: {
          title: values.title,
          description: values.description,
          duration_minutes: values.durationMinutes,
          priority: values.priority,
          difficulty: values.difficulty,
        } satisfies UpdateTaskInput,
      });
      mergeTask(data.task);
      // Patch the shared tasks cache so Board/Lists see the fresh row.
      setTasksCache((prev) =>
        prev.map((entry) => (entry.id === data.task.id ? data.task : entry)),
      );
      return null; // the modal closes itself on success
    } catch (err) {
      return err instanceof Error ? err.message : 'Save failed';
    }
  };

  const handleTaskModalDelete = async (
    taskId: string,
  ): Promise<string | null> => {
    setActionError(null);
    try {
      await deleteTaskMutation.mutateAsync(taskId);
    } catch (err) {
      return err instanceof Error ? err.message : 'Delete failed';
    }
    // The membership row is an orphan after the task dies — the next read
    // omits it server-side; drop it locally now.
    patchStream((current) =>
      current
        ? {
            ...current,
            pages: current.pages.map((page) =>
              page.items.some((entry) => entry.task?.id === taskId)
                ? {
                    ...page,
                    items: page.items.filter(
                      (entry) => entry.task?.id !== taskId,
                    ),
                  }
                : page,
            ),
          }
        : current,
    );
    // Patch the shared tasks cache so Board/Lists drop the task too.
    setTasksCache((prev) => prev.filter((entry) => entry.id !== taskId));
    return null;
  };

  const handleTaskModalMove = async (
    taskId: string,
    status: TaskStatus,
  ): Promise<string | null> => {
    setActionError(null);
    try {
      const data = await moveTaskMutation.mutateAsync({
        id: taskId,
        input: { status } satisfies MoveTaskInput,
      });
      mergeTask(data.task);
      // Patch the shared tasks cache so Board/Lists see the fresh row.
      setTasksCache((prev) =>
        prev.map((entry) => (entry.id === data.task.id ? data.task : entry)),
      );
      // The modal's status pills read `task.status` from its props — keep the
      // edited copy fresh so the selection follows the server.
      setTaskModal((prev) => (prev && prev.id === taskId ? data.task : prev));
      return null;
    } catch (err) {
      return err instanceof Error ? err.message : 'Move failed';
    }
  };

  // ──────────────────────────────────────────
  // Occurrence rename (day-level override; empty clears to inherit)
  // ──────────────────────────────────────────

  const openRename = (item: AgendaItemRecord) => {
    setRenaming(item);
    setRenameTitle(item.occurrence?.title ?? '');
    setRenameError(null);
  };

  const closeRename = () => {
    setRenaming(null);
    setRenameError(null);
  };

  const handleRenameSave = async () => {
    const item = renaming;
    const occurrence = item?.occurrence;
    if (!item || !occurrence) return;
    setRenameSaving(true);
    setRenameError(null);
    try {
      // Empty/whitespace clears the override — the day inherits again.
      const data = await updateOccurrenceMutation.mutateAsync({
        id: occurrence.id,
        input: { title: renameTitle.trim() },
      });
      mergeRow(item.id, (entry) =>
        entry.occurrence ? { ...entry, occurrence: data.occurrence } : entry,
      );
    } catch (err) {
      setRenameSaving(false);
      setRenameError(err instanceof Error ? err.message : 'Save failed');
      return;
    }
    setRenameSaving(false);
    setRenaming(null);
  };

  // ──────────────────────────────────────────
  // Stream scrolling: history/future sentinels, anchor landing, Today
  // ──────────────────────────────────────────

  const scrollRef = useRef<HTMLDivElement>(null);
  const topSentinelRef = useRef<HTMLDivElement>(null);
  const bottomSentinelRef = useRef<HTMLDivElement>(null);
  // Last known intersection of each sentinel — the observer only fires on
  // CHANGES, so a moored sentinel needs these to be re-checked after every
  // fetch settles (see `maybeFetchMore`).
  const topVisibleRef = useRef(false);
  const bottomVisibleRef = useRef(false);
  // History loads wait for a deliberate upward scroll (or the Today button):
  // a mount-time auto-prepend would fetch a week nobody asked for.
  const historyIntentRef = useRef(false);
  // Consecutive future auto-loads while the bottom sentinel stays in view.
  const bottomAutoloadsRef = useRef(0);
  // Scroll geometry captured before a prepend, so the extra height can be
  // added to `scrollTop` afterwards — the anchor must not jump.
  const prependRef = useRef<{
    scrollHeight: number;
    firstFrom: string | null;
    pageCount: number;
  } | null>(null);
  const landedOnAnchorRef = useRef(false);
  // Last observed scroll offset — only an upward move counts as history
  // intent (see the scroll listener below).
  const lastScrollTopRef = useRef(0);
  const [todayVisible, setTodayVisible] = useState(false);

  const hasPreviousPage = stream.hasPreviousPage;
  const hasNextPage = stream.hasNextPage;
  const isFetchingPreviousPage = stream.isFetchingPreviousPage;
  const isFetchingNextPage = stream.isFetchingNextPage;
  const fetchPreviousPage = stream.fetchPreviousPage;
  const fetchNextPage = stream.fetchNextPage;
  const pageCount = pages.length;

  const rememberPrepend = useCallback(() => {
    const root = scrollRef.current;
    if (!root) return;
    const data = readStream();
    prependRef.current = {
      scrollHeight: root.scrollHeight,
      firstFrom: data?.pages[0]?.from ?? null,
      pageCount: data?.pages.length ?? 0,
    };
  }, [readStream]);

  /** Fetch the next page in whichever direction the viewport is asking for.
   *  Called from the sentinel observer and, because an observer only fires on
   *  intersection CHANGES, re-checked after each settle. */
  const maybeFetchMore = useCallback(() => {
    if (
      topVisibleRef.current &&
      historyIntentRef.current &&
      hasPreviousPage &&
      !isFetchingPreviousPage
    ) {
      rememberPrepend();
      void fetchPreviousPage();
      return;
    }
    if (
      bottomVisibleRef.current &&
      hasNextPage &&
      !isFetchingNextPage &&
      bottomAutoloadsRef.current < MAX_BOTTOM_AUTOLOADS
    ) {
      bottomAutoloadsRef.current += 1;
      void fetchNextPage();
    }
  }, [
    hasPreviousPage,
    hasNextPage,
    isFetchingPreviousPage,
    isFetchingNextPage,
    fetchPreviousPage,
    fetchNextPage,
    rememberPrepend,
  ]);

  // A scroll counts as history intent only when it moved UP: history loads
  // wait for a deliberate gesture, so landing on the anchor (whose page starts
  // right under the top sentinel) never fetches a week nobody asked for. The
  // same scroll re-arms the future auto-load budget.
  useEffect(() => {
    const root = scrollRef.current;
    if (!root) return;
    lastScrollTopRef.current = root.scrollTop;
    const onScroll = () => {
      const top = root.scrollTop;
      if (top < lastScrollTopRef.current - 4) historyIntentRef.current = true;
      lastScrollTopRef.current = top;
      bottomAutoloadsRef.current = 0;
    };
    root.addEventListener('scroll', onScroll, { passive: true });
    return () => root.removeEventListener('scroll', onScroll);
  }, []);

  // Sentinels inside the stream's own scroll container. The observer is
  // rebuilt whenever a fetch settles so a still-visible sentinel gets a fresh
  // initial callback.
  useEffect(() => {
    const root = scrollRef.current;
    const top = topSentinelRef.current;
    const bottom = bottomSentinelRef.current;
    if (!root || !top || !bottom || pageCount === 0) return;
    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (entry.target === top) {
            topVisibleRef.current = entry.isIntersecting;
          } else if (entry.target === bottom) {
            bottomVisibleRef.current = entry.isIntersecting;
            if (!entry.isIntersecting) bottomAutoloadsRef.current = 0;
          }
        }
        maybeFetchMore();
      },
      { root, rootMargin: '240px 0px' },
    );
    observer.observe(top);
    observer.observe(bottom);
    return () => observer.disconnect();
  }, [
    pageCount,
    hasPreviousPage,
    hasNextPage,
    isFetchingPreviousPage,
    isFetchingNextPage,
    maybeFetchMore,
  ]);

  // Land on `anchor_date` once, aligned to the top of the stream. Later
  // prepends must NOT re-run this (the height compensation below keeps the
  // anchor exactly where the user left it).
  useLayoutEffect(() => {
    if (landedOnAnchorRef.current || !anchorDate) return;
    const root = scrollRef.current;
    const target = root?.querySelector<HTMLElement>(
      `#${dayElementId(anchorDate)}`,
    );
    if (!target) return;
    landedOnAnchorRef.current = true;
    target.scrollIntoView({ block: 'start' });
  }, [anchorDate, days]);

  // A prepend adds height ABOVE the viewport: add exactly that much to
  // `scrollTop` so the day under the user's eyes does not jump.
  useLayoutEffect(() => {
    const root = scrollRef.current;
    const recorded = prependRef.current;
    if (!root || !recorded) return;
    prependRef.current = null;
    const firstFrom = pages[0]?.from ?? null;
    if (
      pages.length <= recorded.pageCount ||
      firstFrom === recorded.firstFrom
    ) {
      return; // an append (or a failure) — the viewport is already right
    }
    const delta = root.scrollHeight - recorded.scrollHeight;
    if (delta > 0) root.scrollTop += delta;
  }, [pages]);

  // Is today's header on screen? Its absence (today not loaded yet) counts as
  // "not visible", so the Today button shows.
  useEffect(() => {
    const root = scrollRef.current;
    const target = serverToday
      ? root?.querySelector<HTMLElement>(`#${dayElementId(serverToday)}`)
      : null;
    if (!root || !target) {
      setTodayVisible(false);
      return;
    }
    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (entry.target === target) setTodayVisible(entry.isIntersecting);
        }
      },
      { root },
    );
    observer.observe(target);
    return () => observer.disconnect();
  }, [serverToday, pageCount, days]);

  /** The floating Today button: walk the stream until today is loaded (capped
   *  — a crawl, not a loop), then land on its header. */
  const scrollToToday = async () => {
    const root = scrollRef.current;
    if (!root || !serverToday) return;
    historyIntentRef.current = true;
    for (let step = 0; step < MAX_TODAY_FETCHES; step += 1) {
      if (document.getElementById(dayElementId(serverToday))) break;
      const data = readStream();
      const loaded = data?.pages ?? [];
      const first = loaded[0];
      const last = loaded[loaded.length - 1];
      if (!first || !last) return;
      if (serverToday < first.from) {
        // The back-scroll floor is the server's oldest row.
        if (first.from <= first.earliest_date) break;
        rememberPrepend();
        await fetchPreviousPage();
      } else if (serverToday > last.to) {
        await fetchNextPage();
      } else {
        break; // inside the loaded span but not painted (should not happen)
      }
      // A failed / no-op fetch makes no new page — stop rather than spin.
      if ((readStream()?.pages.length ?? 0) <= loaded.length) break;
    }
    document.getElementById(dayElementId(serverToday))?.scrollIntoView({
      block: 'start',
    });
  };

  // ──────────────────────────────────────────
  // Render
  // ──────────────────────────────────────────

  const loadError =
    stream.error instanceof Error
      ? stream.error.message
      : stream.error
        ? 'Failed to load agenda'
        : null;
  const hasPages = pages.length > 0;
  // A later read failed (a page fetch or a background refetch): a one-line
  // notice above the still-mounted days — never a wipe.
  const pageFetchError =
    stream.isFetchNextPageError ||
    stream.isFetchPreviousPageError ||
    (hasPages && !!loadError);

  // Drag sensors: a vertical list with a dedicated grip means a short move
  // activates — Mouse 8px + Touch 8px, still NO 250ms hold (a long-press
  // would fight the stream's own scroll gesture). PointerSensor is gone:
  // Chrome DevTools device mode and many phones speak touch events, not
  // pointer. The listeners live only on the grip, so row taps never reach a
  // sensor. `autoScroll` is what lets a drag walk the days inside the
  // stream's scroll container.
  const sensors = useSensors(
    useSensor(MouseSensor, { activationConstraint: { distance: 8 } }),
    useSensor(TouchSensor, { activationConstraint: { distance: 8 } }),
  );

  const handlers: AgendaRowHandlers = {
    onReschedule: (entry, date) => void handleCrossDayMove(entry, date),
    onCompleteTask: (entry) => void handleCompleteTask(entry),
    onStartTask: (entry) => void handleStartTask(entry),
    onPauseTask: (entry) => void handlePauseTask(entry),
    onOpenTask: (task) => setTaskModal(task),
    onCompleteOccurrence: (entry) =>
      entry.occurrence?.status === 'done'
        ? void handleReopenOccurrence(entry)
        : void setOccurrenceStatus(entry, 'done', 'complete'),
    onSkipOccurrence: (entry) =>
      entry.occurrence?.status === 'skipped'
        ? void handleReopenOccurrence(entry)
        : void setOccurrenceStatus(entry, 'skipped', 'skip'),
    onStartOccurrence: (entry) => void handleStartOccurrence(entry),
    onPauseOccurrence: (entry) => void handlePauseOccurrence(entry),
    onRenameOccurrence: (entry) => openRename(entry),
  };

  return (
    <div className="min-h-screen bg-cream">
      <div className="mx-auto max-w-7xl px-6 pt-8 pb-28">
        <div className="grid grid-cols-1 gap-8 lg:grid-cols-3">
          {/* Main column — the day stream */}
          <div className="min-w-0 lg:col-span-2">
            <header className="mb-4 flex items-center justify-between gap-4">
              <h1 className="font-heading text-3xl font-bold text-foreground">
                Agenda
              </h1>
              <Link
                to="/routines"
                className="inline-flex items-center gap-1.5 text-sm font-medium text-primary hover:underline"
              >
                <Repeat className="h-4 w-4" />
                Routines
              </Link>
            </header>

            {/* Below lg the focus timer rides above the stream as a compact
                sticky strip — same component, constrained by classes. */}
            <div className="sticky top-2 z-20 mb-3 rounded-xl border border-border bg-card/95 shadow-sm backdrop-blur lg:hidden">
              <FocusTimer className={COMPACT_TIMER_CLASSES} />
            </div>

            {/* Action error banner (move/complete/skip failures) — the stream
                stays mounted */}
            {actionError && (
              <div className="mb-3 flex items-center justify-between gap-4 rounded-xl bg-destructive/10 px-4 py-3 text-destructive">
                <p className="text-sm">{actionError}</p>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setActionError(null)}
                >
                  Dismiss
                </Button>
              </div>
            )}

            {/* A later page failed: a one-line notice, never a wipe */}
            {pageFetchError && (
              <div className="mb-3 flex items-center justify-between gap-3 rounded-lg bg-destructive/10 px-3 py-2 text-destructive">
                <p className="text-xs">Could not load more days.</p>
                <button
                  type="button"
                  onClick={() => {
                    if (stream.isFetchPreviousPageError) {
                      rememberPrepend();
                      void stream.fetchPreviousPage();
                    } else if (stream.isFetchNextPageError) {
                      void stream.fetchNextPage();
                    } else {
                      void stream.refetch();
                    }
                  }}
                  className="text-xs font-medium underline"
                >
                  Retry
                </button>
              </div>
            )}

            {/* The stream: its own scroll container, so the page itself never
                grows without bound and dnd-kit can auto-scroll a drag. */}
            <div
              ref={scrollRef}
              className="relative h-[calc(100dvh-17rem)] min-h-[20rem] overflow-y-auto overscroll-contain rounded-xl border border-border/60 bg-cream lg:h-[calc(100dvh-12rem)]"
            >
              {/* First load — no pages yet */}
              {!hasPages && stream.isLoading && (
                <div className="flex items-center justify-center gap-2 py-24 text-muted-foreground">
                  <Loader2 className="h-5 w-5 animate-spin" />
                  Loading agenda…
                </div>
              )}

              {/* First fetch failed and there is nothing to show */}
              {!hasPages && !stream.isLoading && loadError && (
                <div className="m-2 flex items-center justify-between gap-4 rounded-xl bg-destructive/10 px-4 py-3 text-destructive">
                  <p className="text-sm">{loadError}</p>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => void stream.refetch()}
                  >
                    Retry
                  </Button>
                </div>
              )}

              {hasPages && (
                <>
                  <div
                    ref={topSentinelRef}
                    aria-hidden
                    className="h-1 w-full"
                  />
                  <DndContext
                    sensors={sensors}
                    collisionDetection={closestCenter}
                    autoScroll
                    onDragStart={(event) =>
                      setActiveDrag(
                        streamItemsOf(readStream()).find(
                          (entry) => entry.id === event.active.id,
                        ) ?? null,
                      )
                    }
                    onDragEnd={handleDragEnd}
                    onDragCancel={() => setActiveDrag(null)}
                  >
                    {days.map((day) => (
                      <AgendaDay
                        key={day}
                        day={day}
                        serverToday={serverToday}
                        items={byDate.get(day) ?? []}
                        handlers={handlers}
                        onMoveDayToToday={(target) =>
                          void handleMoveDayToToday(target)
                        }
                      />
                    ))}

                    {/* Floating title chip while dragging — the source row
                        stays in place, dimmed (opacity-40 on its wrapper) */}
                    <DragOverlay>
                      {activeDrag && (
                        <div className="cursor-grabbing rounded-lg border border-border bg-background px-3 py-2 shadow-xl ring-1 ring-border/60">
                          <span className="text-sm font-medium text-foreground">
                            {activeDrag.kind === 'occurrence' &&
                            activeDrag.occurrence
                              ? activeDrag.occurrence.resolved_title
                              : (activeDrag.task?.display_title ?? '')}
                          </span>
                        </div>
                      )}
                    </DragOverlay>
                  </DndContext>
                  <div
                    ref={bottomSentinelRef}
                    aria-hidden
                    className="h-8 w-full"
                  />
                </>
              )}
            </div>
          </div>

          {/* Right panel — Focus & Quotes (unchanged, lg and up only: below
              lg the compact timer sits above the stream instead) */}
          <div className="hidden space-y-6 lg:block">
            <FocusTimer />
            <QuotesSection quotes={quotes} />
          </div>
        </div>
      </div>

      {/* Today: jump back from anywhere in the stream (hidden while today's
          header is on screen) */}
      {serverToday && !todayVisible && (
        <button
          type="button"
          onClick={() => void scrollToToday()}
          className="fixed bottom-24 left-1/2 z-40 -translate-x-1/2 rounded-full border border-border bg-card px-4 py-2 text-sm font-medium text-foreground shadow-lg transition-colors hover:bg-muted lg:left-auto lg:right-8 lg:translate-x-0"
        >
          Today
        </button>
      )}

      {/* Task edit modal — the existing TaskModal (edit/delete/move) */}
      <TaskModal
        open={taskModal !== null}
        onOpenChange={(open) => {
          if (!open) setTaskModal(null);
        }}
        task={taskModal ?? undefined}
        onSubmit={handleTaskModalSubmit}
        onDelete={handleTaskModalDelete}
        onMove={handleTaskModalMove}
      />

      {/* Occurrence rename dialog — a day-level override; empty clears */}
      <Dialog
        open={renaming !== null}
        onOpenChange={(open) => {
          if (!open) closeRename();
        }}
      >
        <DialogContent className="flex max-h-[90dvh] flex-col gap-0 overflow-hidden p-0 sm:max-w-[480px] bg-card border-border">
          <DialogHeader className="shrink-0 px-6 pt-6 pb-4">
            <DialogTitle className="text-foreground">
              Rename for this day
            </DialogTitle>
            <DialogDescription>
              Changes this occurrence only — never the routine. Leave empty to
              inherit the routine title again.
            </DialogDescription>
          </DialogHeader>
          <hr className="shrink-0 border-border" />
          <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-6 py-4">
            <input
              type="text"
              value={renameTitle}
              onChange={(e) => setRenameTitle(e.target.value)}
              placeholder={
                renaming?.occurrence
                  ? renaming.occurrence.title == null
                    ? `Inherit: ${renaming.occurrence.resolved_title}`
                    : 'Day override — empty to inherit'
                  : undefined
              }
              className="w-full px-4 py-3 rounded-xl border border-input bg-background text-foreground placeholder:text-muted-foreground/60 focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all"
            />
            {renameError && (
              <p className="mt-3 text-sm text-destructive">{renameError}</p>
            )}
          </div>
          <div className="flex shrink-0 justify-end gap-3 border-t border-border px-6 py-4">
            <Button
              variant="outline"
              onClick={closeRename}
              disabled={renameSaving}
              className="border-input text-foreground hover:bg-muted"
            >
              Cancel
            </Button>
            <Button
              onClick={() => void handleRenameSave()}
              disabled={renameSaving}
              className="bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
            >
              {renameSaving && (
                <Loader2 className="h-4 w-4 mr-2 animate-spin" />
              )}
              Save
            </Button>
          </div>
        </DialogContent>
      </Dialog>
    </div>
  );
}

/** Every loaded row of one civil day, in stream order. */
function dayItemsOf(
  items: AgendaItemRecord[],
  day: string,
): AgendaItemRecord[] {
  return items.filter((entry) => entry.local_date === day);
}
