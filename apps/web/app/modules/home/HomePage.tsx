// Home — the agenda as an infinite run of days, then TWO warehouse sections,
// Planned above Backlog (ADR 0004 amendment, slice 5; ADR 0007, slice 6; ADR
// 0009, slice 7). One vertically scrolling run of civil days inside its own
// scroll container: opening the page lands on the server's `anchor_date` (the
// oldest past day that still has something pending), scroll UP for history,
// DOWN to the end of today+6. There is NO future page — each loaded page paints
// its own `from…to`, so a history page and a later anchor never get gap-filled
// into a wall of empty days. The cache itself is two reads
// (`queries/agenda.ts`): the default window (`anchor_date … today+6` plus the
// backlog) is a plain query, and history is a second, lazy query keyed by the
// anchor it walks behind — so a refetch can never drop today. Each day keeps
// the two piles (living rows ranked by `sort_order`, then the muted Completed
// dump), plus a sticky header; a past day with pending work shows an amber
// count and a one-tap "Move all to today". Under the last day sit the two
// warehouses: **Planned** (the `PLANNED` rows at the sentinel, in the Planned
// column's Board order) and then the **Backlog** (the dated future stretch
// grouped by day, then the `OPEN` rows at the sentinel in the Open column's
// Board order). Dragging a row onto today or a later stream day reschedules it
// (`/reschedule` with the target rank — an `OPEN` task becomes `PLANNED`); a
// same-day drop is the old `/move` reorder. Dragging into Backlog drops a row
// (`{ backlog: true }` → `OPEN` on today+50y, still the unplan), dragging into
// Planned plans an `OPEN` row (`/move { status: 'PLANNED' }` with no rank — a
// dated row keeps its day, an undated row keeps its sentinel), and dragging a
// dated `PLANNED` row there unschedules it (`{ unschedule: true }`: status, log
// and Board rank all untouched). Dragging inside one warehouse is that
// column's Board task `/move`, so Home and Board share one order per column;
// occurrences and running tasks are never dropped there. The old header date
// picker, the add-task picker and remove-from-day are gone — tasks are born on
// the Board. A task's status is workflow and its row's `local_date` is
// scheduling intent: a status-only move never re-dates a dated row, and only
// an undated (warehoused) row is dated today. A row's calendar menu offers
// today-relative presets, a month picker and (for tasks) Unschedule / Backlog.
// Built on the existing task/occurrence
// actions (complete, skip, reopen, start, pause, rename, TaskModal, error
// banner, move notice, optimistic `['tasks']` patches) and reschedule/complete
// art.

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
  useSensor,
  useSensors,
  type DragEndEvent,
} from '@dnd-kit/core';
import { arrayMove } from '@dnd-kit/sortable';
import { Check, Loader2, Repeat, X } from 'lucide-react';
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
import { quotes } from '@/app/data/quotes';
import { moveDayToToday } from '@/lib/api';
import {
  readAgendaPages,
  useAgendaCurrent,
  useAgendaHistory,
  useMoveAgendaItem,
  useRescheduleAgendaItem,
  writeAgendaPages,
  type AgendaPages,
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
import { agendaCollisionDetection } from './agenda-collision';
import { AgendaDay } from './AgendaDay';
import { backlogDayElementId, dayElementId } from './agenda-dom';
import {
  canMoveToToday,
  type ActionNotice,
  type AgendaRowHandlers,
  type RowBusySlot,
} from './agenda-row-contract';
import { BacklogSection } from './BacklogSection';
import { PlannedSection } from './PlannedSection';
import { addCivilDays } from '../routines/rrule-preview';
import {
  addMonthsClamp,
  agendaDateLabel,
  agendaShortDateLabel,
  appendRankOnStream,
  applyAgendaMove,
  applyOpenTaskReorder,
  BACKLOG_DROPPABLE_ID,
  canUnschedule,
  dedupeAgendaItems,
  backlogStretchesForRebuild,
  eachCivilDate,
  groupItemsByDate,
  historyPageZero,
  moveItemToBacklog,
  moveStreamItem,
  planOpenItem,
  planWaitingToDateSecondWrite,
  PLANNED_DROPPABLE_ID,
  placeUndatedTaskOnToday,
  replaceCachedTask,
  resolveAgendaDrop,
  restoreHandedOffRow,
  runAgendaWriteFlow,
  splitBacklogSection,
  streamItemsOf,
  unscheduleItem,
  updateStreamItem,
} from './agenda-helpers';
import type {
  AgendaItemRecord,
  AgendaResponse,
  MoveTaskInput,
  OccurrenceStatus,
  RescheduleAgendaItemInput,
  TaskDifficulty,
  TaskPriority,
  TaskRecord,
  TaskStatus,
  UpdateTaskInput,
} from '@/app/types';

/** The default read's own `anchor_date` as the cache holds it right now — the
 *  key that names Home's history cache and the split in `writeAgendaPages`.
 *  Empty until the default read resolves (nothing to read or write then). */
function currentAnchor(): string {
  return (
    queryClient.getQueryData<AgendaResponse>(queryKeys.agenda.current())
      ?.anchor_date ?? ''
  );
}

export function HomePage() {
  // The two agenda reads (ADR 0004 amendment, ADR 0007): the default window —
  // `anchor_date … today+6` plus the backlog — is a plain query, so an
  // invalidation always refetches today; history is a second, lazy query keyed
  // by the anchor it walks behind.
  const current = useAgendaCurrent();
  const currentPage = current.data;
  // The server's civil today (primary calendar time zone) — refreshed from
  // every read; anchors the day headers, the Play gate, the reschedule floor
  // and the Today button.
  const serverToday = currentPage?.today ?? '';
  const anchorDate = currentPage?.anchor_date ?? '';
  // The server's oldest row — the floor of the history walk (`earliest_date`),
  // and what decides whether history has anything to load at all.
  const earliestDate = currentPage?.earliest_date ?? '';
  // History never fetches on mount: the user has to reach the top of the
  // stream first (see `maybeFetchPreviousPage`). State, not a ref — flipping it
  // is what re-renders the hook into an enabled query.
  const [historyEnabled, setHistoryEnabled] = useState(false);
  const history = useAgendaHistory(anchorDate, earliestDate, historyEnabled);
  // The history pages in query order (page 0 is the anchor's eve week, older
  // weeks are appended) — the span the top-edge gesture grows.
  const historyPages = useMemo(() => history.data?.pages ?? [], [history.data]);
  // The oldest history week loaded, or null while none is. A top-edge fetch
  // PREPENDS in the rendered spine exactly when this moves back.
  const historyOldestFrom = useMemo(
    () =>
      historyPages.reduce<string | null>(
        (oldest, page) =>
          oldest === null || page.from < oldest ? page.from : oldest,
        null,
      ),
    [historyPages],
  );
  // The rendered day spine: history weeks (sorted by `from`) then the default
  // window's own `from…to`, deduped (the first page to claim a date wins).
  // Deliberately NOT one `eachCivilDate(first.from, last.to)` run — that would
  // fill the hole between a history page and a later anchor with empty days.
  // Backlog rows never enter it, so it can never walk out to today+50y.
  const pages = useMemo(() => {
    const ordered = [...historyPages].sort((a, b) =>
      a.from < b.from ? -1 : a.from > b.from ? 1 : 0,
    );
    return currentPage ? [...ordered, currentPage] : ordered;
  }, [historyPages, currentPage]);
  const days = useMemo(() => {
    const seen = new Set<string>();
    const spine: string[] = [];
    for (const page of pages) {
      for (const day of eachCivilDate(page.from, page.to)) {
        if (seen.has(day)) continue;
        seen.add(day);
        spine.push(day);
      }
    }
    return spine;
  }, [pages]);
  const items = useMemo(
    // `dedupeAgendaItems` is a backstop against a row stranded in a page whose
    // window does not hold its `local_date` (see `moveStreamItem`) — grouping
    // a duplicate id would paint its day twice with duplicate keys.
    () => dedupeAgendaItems(pages.flatMap((page) => page.items)),
    [pages],
  );
  const byDate = useMemo(() => groupItemsByDate(items), [items]);
  // The two warehouses (ADR 0007, ADR 0010): the DEFAULT read's `backlog`
  // array (a history read always answers with an empty one), split into the
  // dated stretch (grouped by day), the Planned stretch and the OPEN stretch —
  // both warehouse stretches in their column's Board order, as served.
  const backlogItems = useMemo(
    () => dedupeAgendaItems(currentPage?.backlog ?? []),
    [currentPage],
  );
  const backlogSplit = useMemo(
    () => splitBacklogSection(backlogItems, serverToday),
    [backlogItems, serverToday],
  );

  // Action failures (move/complete/skip/etc.): a banner above the still-
  // visible stream — rows are never unmounted by an action error.
  const [actionError, setActionError] = useState<string | null>(null);
  // A successful move's transient confirmation (with Show); replaces any
  // previous notice, auto-dismisses, and never survives a failure.
  const [actionNotice, setActionNotice] = useState<ActionNotice | null>(null);
  // The row to ring briefly after Show has scrolled to it.
  const [flashItemId, setFlashItemId] = useState<string | null>(null);
  // The notice Show is currently resolving — consumed by the layout effect
  // below once the target row's Waiting section has expanded.
  const [pendingShow, setPendingShow] = useState<ActionNotice | null>(null);
  // The row currently dragged — feeds the DragOverlay title chip.
  const [activeDrag, setActiveDrag] = useState<AgendaItemRecord | null>(null);
  // Row actions in flight: the render-time map behind `AgendaItemRow`'s
  // spinner + disabled controls …
  const [rowBusy, setRowBusy] = useState<Record<string, RowBusySlot>>({});
  // … and its SYNCHRONOUS twin. A second click can land before React paints
  // the disabled button, so the lock is taken in the ref first and only then
  // mirrored into state.
  const rowBusyRef = useRef(new Set<string>());

  useEffect(() => {
    if (!actionNotice) return;
    const timer = window.setTimeout(() => setActionNotice(null), 6000);
    return () => window.clearTimeout(timer);
  }, [actionNotice]);

  useEffect(() => {
    if (!flashItemId) return;
    const timer = window.setTimeout(() => setFlashItemId(null), 1800);
    return () => window.clearTimeout(timer);
  }, [flashItemId]);

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

  /** Home's whole agenda cache as it is RIGHT NOW (never the render-time
   *  snapshot): a drag can land after a refetch resolved, and the optimistic
   *  paint must build on what is actually cached. `anchor` is the default
   *  read's own `anchor_date` — without a default read there is nothing to
   *  read or write. */
  const readStream = useCallback((): AgendaPages | undefined => {
    const anchor = currentAnchor();
    if (!anchor) return undefined;
    return readAgendaPages(anchor);
  }, []);

  const patchStream = useCallback(
    (
      updater: (current: AgendaPages | undefined) => AgendaPages | undefined,
    ) => {
      const anchor = currentAnchor();
      if (!anchor) return;
      writeAgendaPages(anchor, updater(readAgendaPages(anchor)));
    },
    [],
  );

  const mergeRow = useCallback(
    (itemId: string, updater: (item: AgendaItemRecord) => AgendaItemRecord) => {
      patchStream((current) => updateStreamItem(current, itemId, updater));
    },
    [patchStream],
  );

  // ──────────────────────────────────────────
  // Row action locks (spinner + one mutation per row)
  // ──────────────────────────────────────────

  /** Take the row's lock for `slot`, or return false when another action on
   *  that row is still in flight. Called SYNCHRONOUSLY before the optimistic
   *  paint (and before any `await`), so a double click never fires two
   *  requests and the row can paint its spinner. */
  const beginRowAction = useCallback(
    (itemId: string, slot: RowBusySlot): boolean => {
      if (rowBusyRef.current.has(itemId)) return false;
      rowBusyRef.current.add(itemId);
      setRowBusy((prev) => ({ ...prev, [itemId]: slot }));
      return true;
    },
    [],
  );

  /** Release the row's lock — always from the handler's `finally`, so the
   *  spinner clears on success AND failure. */
  const endRowAction = useCallback((itemId: string) => {
    rowBusyRef.current.delete(itemId);
    setRowBusy((prev) => {
      if (!(itemId in prev)) return prev;
      const next = { ...prev };
      delete next[itemId];
      return next;
    });
  }, []);

  const pendingOf = useCallback(
    (itemId: string): RowBusySlot | null => rowBusy[itemId] ?? null,
    [rowBusy],
  );

  // ──────────────────────────────────────────
  // Reorder / reschedule
  // ──────────────────────────────────────────

  const moveAgendaItemMutation = useMoveAgendaItem();
  const rescheduleAgendaItemMutation = useRescheduleAgendaItem();

  /** One warehouse's rows as the cache holds them right now (server order,
   *  which is that Board column's `tasks.sort_order` order). */
  const warehouseRowsOf = (
    data: AgendaPages | undefined,
    status: 'OPEN' | 'PLANNED',
  ) => {
    const split = splitBacklogSection(
      dedupeAgendaItems(
        (data?.pages ?? []).flatMap((page) => page.backlog ?? []),
      ),
      serverToday,
    );
    return status === 'OPEN' ? split.open : split.planned;
  };

  /** dnd-kit drop resolution. Same-day → the old `/move` reorder; across days
   *  → `/reschedule` with the target day's rank; the Waiting section → a
   *  hand-off, a take-back (onto a date) or a waiting in-section reorder; the
   *  Planned warehouse (its section or a Planned row) → plan / unschedule /
   *  the Planned column's Board reorder; the Backlog (its section or an OPEN
   *  row) → a backlog drop or the Open column's Board reorder. Past days are
   *  never drop targets (the API 400s), an `in_progress` occurrence may only
   *  reorder inside its own day, and terminal cards and occurrences never
   *  enter a warehouse. */
  const handleDragEnd = (event: DragEndEvent) => {
    setActiveDrag(null);
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const activeId = String(active.id);
    const overId = String(over.id);
    const dataNow = readStream();
    const itemsNow = streamItemsOf(dataNow);
    const dragged = itemsNow.find((entry) => entry.id === activeId);
    if (!dragged) return;
    // Rows in `page.items` are painted in the day stream; rows in
    // `page.backlog` are the dated warehouse stretch (or a warehouse pile).
    // The Waiting SECTION only owns the former.
    const streamItemIds = new Set(
      (dataNow?.pages ?? []).flatMap((page) =>
        page.items.map((entry) => entry.id),
      ),
    );
    const overItem = itemsNow.find((entry) => entry.id === overId) ?? null;
    const decision = resolveAgendaDrop({
      dragged,
      overId,
      overItem,
      items: itemsNow,
      streamItemIds,
      today: serverToday,
    });

    switch (decision.kind) {
      case 'none':
        return;
      case 'sameDay':
        void handleSameDayMove(dragged, decision.target);
        return;
      case 'crossDay':
        void handleCrossDayMove(
          dragged,
          decision.date,
          'grip',
          decision.sortOrder,
        );
        return;
      case 'warehouseReorder':
        void handleWarehouseReorder(
          dragged,
          decision.status,
          warehouseRowsOf(readStream(), decision.status),
          decision.toIndex,
          decision.target,
        );
        return;
      case 'plan':
        void handlePlanDrop(dragged);
        return;
      case 'unschedule':
        void handleUnschedule(dragged, 'grip');
        return;
      case 'backlogDrop':
        void handleBacklogDrop(dragged, 'grip');
        return;
      case 'handOffToWaiting':
        void handleHandOffToWaiting(dragged, decision.date);
        return;
      case 'waitingToWarehouse':
        void handleWaitingToWarehouse(dragged, decision.status);
        return;
      case 'waitingToDate':
        void handleWaitingToDate(dragged, decision.date, decision.sortOrder);
        return;
    }
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
    if (!beginRowAction(item.id, 'grip')) return;
    try {
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
        // The Board's Planned read shows the same agenda rank. The mutation
        // stays neutral so Home itself does not refetch after its own move.
        void queryClient.invalidateQueries({
          queryKey: queryKeys.agenda.boardPlanned(),
        });
      } catch (err) {
        patchStream(() => snapshot);
        setActionError(err instanceof Error ? err.message : 'Move failed');
      }
    } finally {
      endRowAction(item.id);
    }
  };

  /** Confirmation for a successful move. A target past the default window's
   *  last day (today+6) lives in the Backlog's dated stretch; saying so
   *  prevents "it disappeared" when the row leaves the main stream. */
  const showMoveNotice = (moved: AgendaItemRecord) => {
    const date = moved.local_date;
    const inDatedBacklog = !!serverToday && date > addCivilDays(serverToday, 6);
    const relative = agendaDateLabel(date, serverToday);
    const absolute = agendaShortDateLabel(date, serverToday);
    const dateLabel =
      relative === absolute ? absolute : `${relative} · ${absolute}`;
    setActionNotice({
      message: `Moved to ${dateLabel}${inDatedBacklog ? ' · Backlog' : ''}`,
      itemId: moved.id,
      date,
    });
  };

  /** Move a row onto another day (drag across days, or the row's own
   *  reschedule popover). `sortOrder` is the absolute rank the drop wants —
   *  the target row's own `sort_order` (`crossDaySortOrder`); omitted — the
   *  popover path, or a drop on the day container — appends at the pile's
   *  end server-side (`max + 1`), exactly like a popover pick. A failure
   *  restores the snapshot and banners. Landing a task that was still `OPEN`
   *  plans it server-side — this is the ONLY move that does, so it is the one
   *  caller that asks `moveStreamItem` to paint `PLANNED`. The server also
   *  appends the task to the Planned column, so the shared `['tasks']` cache
   *  takes its fresh row (status AND Board rank) — the same patch a backlog
   *  drop sends. */
  const handleCrossDayMove = async (
    item: AgendaItemRecord,
    targetDate: string,
    slot: 'calendar' | 'grip',
    sortOrder?: number,
  ) => {
    if (!targetDate || targetDate === item.local_date) return;
    if (!serverToday || targetDate < serverToday) return;
    // `in_progress` occurrences are reorder-only until they are paused; an
    // `IN_PROGRESS` task cannot change day at all (the API 400s — pause
    // first). A `WAITING` task CAN: the API keeps it waiting and only the row
    // travels (ADR 0011).
    if (item.occurrence?.status === 'in_progress') return;
    if (item.task?.status === 'IN_PROGRESS') return;
    if (!beginRowAction(item.id, slot)) return;
    try {
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
          true,
        ),
      );
      setActionError(null);
      try {
        const data = await rescheduleAgendaItemMutation.mutateAsync({
          id: item.id,
          input,
        });
        mergeRow(data.item.id, () => data.item);
        // Patch the shared tasks cache so Board/Lists see the flipped status
        // and the new Planned rank.
        setTasksCache((prev) => replaceCachedTask(prev, data.item.task));
        showMoveNotice(data.item);
      } catch (err) {
        patchStream(() => snapshot);
        setActionNotice(null);
        setActionError(err instanceof Error ? err.message : 'Move failed');
      }
    } finally {
      endRowAction(item.id);
    }
  };

  /** Hand a living task off to WAITING by dropping it into a day's Waiting
   *  section (ADR 0011 § Where the agenda row goes). A status-only transition
   *  never moves a dated row: it preserves the day the task already sits on,
   *  and only dates an undated row on civil today. A drop on another date is
   *  therefore a second, reschedule write that preserves the section the user
   *  aimed at. The optimistic paint goes straight to the FINAL day/status. A
   *  first-write failure restores the snapshot; a second-write failure repaints
   *  the state the server actually holds (`restoreHandedOffRow`: the dated row
   *  stayed on its old day, an undated one landed on today) and banners. */
  const handleHandOffToWaiting = async (
    item: AgendaItemRecord,
    targetDate: string,
  ) => {
    const task = item.task;
    if (!task || !serverToday || !targetDate) return;
    if (targetDate < serverToday) return;
    if (task.status === 'WAITING') return;
    if (task.status === 'COMPLETED' || task.status === 'DISCARDED') return;
    if (!beginRowAction(item.id, 'grip')) return;
    try {
      const snapshot = readStream();
      // Where the first write leaves the row: a dated row keeps its own day,
      // an undated (or missing) row lands on today.
      const sourceRow = streamItemsOf(snapshot).find(
        (entry) => entry.id === item.id,
      );
      const horizon = addMonthsClamp(serverToday, 120);
      const placedDate =
        sourceRow && sourceRow.local_date < horizon
          ? sourceRow.local_date
          : serverToday;
      patchStream((current) => {
        const handed = placeUndatedTaskOnToday(
          current,
          item.id,
          serverToday,
          (entry) =>
            entry.task
              ? { ...entry, task: { ...entry.task, status: 'WAITING' } }
              : entry,
        );
        const row = streamItemsOf(handed).find((entry) => entry.id === item.id);
        if (!row || row.local_date === targetDate) return handed;
        return moveStreamItem(
          handed,
          item.id,
          targetDate,
          appendRankOnStream(handed, targetDate),
        );
      });
      setActionError(null);
      const result = await runAgendaWriteFlow({
        first: () =>
          moveTaskMutation.mutateAsync({
            id: task.id,
            input: { status: 'WAITING' } satisfies MoveTaskInput,
          }),
        second:
          targetDate === placedDate
            ? undefined
            : () =>
                rescheduleAgendaItemMutation.mutateAsync({
                  id: item.id,
                  input: { date: targetDate },
                }),
        onFirstSuccess: (data) => {
          mergeRow(item.id, (entry) =>
            entry.task ? { ...entry, task: data.task } : entry,
          );
          setTasksCache((prev) => replaceCachedTask(prev, data.task));
        },
        onSecondSuccess: (data) => {
          mergeRow(data.item.id, () => data.item);
          setTasksCache((prev) => replaceCachedTask(prev, data.item.task));
          setActionNotice({
            message: `Handed off · ${agendaDateLabel(targetDate, serverToday)}`,
            itemId: data.item.id,
            date: targetDate,
          });
        },
        onFirstFailure: (err) => {
          patchStream(() => snapshot);
          setActionNotice(null);
          setActionError(
            err instanceof Error ? err.message : 'Hand-off failed',
          );
        },
        onSecondFailure: (err, first) => {
          // The hand-off itself succeeded; only the date move failed.
          // `restoreHandedOffRow` repaints the server's state — a dated row
          // stayed on its old day, an undated one landed on today — before
          // asking the stream to re-read.
          patchStream(() =>
            restoreHandedOffRow(snapshot, item.id, serverToday, first.task),
          );
          setActionNotice(null);
          setActionError(
            `Handed off, but could not move it to ${agendaShortDateLabel(
              targetDate,
              serverToday,
            )}: ${err instanceof Error ? err.message : 'Move failed'}`,
          );
          void queryClient.invalidateQueries({
            queryKey: queryKeys.agenda.all,
          });
        },
      });
      if (result === 'ok' && targetDate === placedDate) {
        setActionNotice({
          message:
            targetDate === serverToday
              ? 'Handed off · Waiting'
              : `Handed off · ${agendaDateLabel(targetDate, serverToday)}`,
          itemId: item.id,
          date: targetDate,
        });
      }
    } finally {
      endRowAction(item.id);
    }
  };

  /** Shelve a WAITING row into a warehouse (ADR 0013): `WAITING` →
   *  `OPEN` / `PLANNED` through the Board's `/move`, then an explicit date
   *  action parks the row in that warehouse — `{ backlog: true }` for OPEN and
   *  `{ unschedule: true }` for PLANNED. The status move alone preserves a
   *  dated row by design (status and date are decoupled), so the second write
   *  is what actually shelves it. The optimistic paint goes straight to the
   *  final SHELVED state with `moveItemToBacklog` / `unscheduleItem`, then
   *  merges the authoritative row. A first-write failure restores the
   *  snapshot; a second-write failure repaints the server's real state — the
   *  status move succeeded, but the row kept its old date — with `first.task`
   *  and banners. */
  const handleWaitingToWarehouse = async (
    item: AgendaItemRecord,
    status: 'OPEN' | 'PLANNED',
  ) => {
    const task = item.task;
    if (!task || task.status !== 'WAITING' || !serverToday) return;
    if (!beginRowAction(item.id, 'grip')) return;
    try {
      const snapshot = readStream();
      const sentinel = addMonthsClamp(serverToday, 600);
      patchStream((current) => {
        if (!current) return current;
        if (status === 'OPEN') {
          return moveItemToBacklog(current, item.id, sentinel);
        }
        // A WAITING → PLANNED transition appends the Planned column (ADR
        // 0013); the explicit `{ unschedule: true }` write is what parks the
        // row on the sentinel.
        const appendRank = streamItemsOf(current)
          .filter((entry) => entry.id !== item.id)
          .flatMap((entry) =>
            entry.task?.status === 'PLANNED' ? [entry.task.sort_order] : [],
          )
          .reduce((rank, value) => Math.max(rank, value + 1), 0);
        const planned = updateStreamItem(current, item.id, (entry) =>
          entry.task
            ? {
                ...entry,
                task: {
                  ...entry.task,
                  status: 'PLANNED',
                  sort_order: appendRank,
                },
              }
            : entry,
        );
        return unscheduleItem(planned, item.id, sentinel, serverToday);
      });
      setActionError(null);
      await runAgendaWriteFlow({
        first: () =>
          moveTaskMutation.mutateAsync({
            id: task.id,
            input: { status } satisfies MoveTaskInput,
          }),
        second: () =>
          rescheduleAgendaItemMutation.mutateAsync({
            id: item.id,
            input:
              status === 'OPEN'
                ? ({ backlog: true } satisfies RescheduleAgendaItemInput)
                : ({ unschedule: true } satisfies RescheduleAgendaItemInput),
          }),
        onFirstSuccess: (data) => {
          mergeRow(item.id, (entry) =>
            entry.task ? { ...entry, task: data.task } : entry,
          );
          setTasksCache((prev) => replaceCachedTask(prev, data.task));
        },
        onSecondSuccess: (data) => {
          mergeRow(data.item.id, () => data.item);
          setTasksCache((prev) => replaceCachedTask(prev, data.item.task));
          setActionNotice({
            message:
              status === 'PLANNED' ? 'Moved to Planned' : 'Moved to Backlog',
            itemId: data.item.id,
            section: status === 'PLANNED' ? 'planned' : 'backlog',
          });
        },
        onFirstFailure: (err) => {
          patchStream(() => snapshot);
          setActionNotice(null);
          setActionError(err instanceof Error ? err.message : 'Move failed');
        },
        onSecondFailure: (err, first) => {
          // The status move succeeded; only the shelving write failed, so the
          // row kept its old date. Repaint the snapshot with the first write's
          // task embed — never the sentinel — before asking the stream to
          // re-read.
          patchStream(() =>
            updateStreamItem(snapshot, item.id, (entry) =>
              entry.task ? { ...entry, task: first.task } : entry,
            ),
          );
          setActionNotice(null);
          setActionError(
            `Changed status, but could not shelve the row: ${
              err instanceof Error ? err.message : 'Move failed'
            }`,
          );
          void queryClient.invalidateQueries({
            queryKey: queryKeys.agenda.all,
          });
        },
      });
    } finally {
      endRowAction(item.id);
    }
  };

  /** Take a WAITING row out to a real day by dropping it on the normal pile
   *  (ADR 0011). The destination says PLANNED, so the task leaves Waiting as a
   *  `PLANNED` row. The status move preserves the row's date (ADR 0013), so a
   *  CROSS-DAY drop needs a second `/reschedule`; a SAME-DAY drop with a rank
   *  needs an agenda `/move` reorder (a same-date reschedule is a server no-op
   *  that would drop the rank); a same-day drop without a rank needs no second
   *  write. A first-write failure restores the snapshot; a second-write failure
   *  repaints the server's real state — the PLANNED task on its old date and
   *  rank — with `first.task` and banners. */
  const handleWaitingToDate = async (
    item: AgendaItemRecord,
    targetDate: string,
    sortOrder?: number,
  ) => {
    const task = item.task;
    if (!task || task.status !== 'WAITING' || !serverToday) return;
    if (!targetDate || targetDate < serverToday) return;
    if (!beginRowAction(item.id, 'grip')) return;
    try {
      const snapshot = readStream();
      // PLANNED preserves any existing row's date: a dated row keeps its day,
      // an undated one stays undated. Fall back to the dragged row when the
      // cache lookup somehow misses.
      const sourceRow = streamItemsOf(snapshot).find(
        (entry) => entry.id === item.id,
      );
      const placedDate = sourceRow?.local_date ?? item.local_date;
      const secondWrite = planWaitingToDateSecondWrite(
        placedDate,
        targetDate,
        sortOrder,
      );
      patchStream((current) => {
        const planned = updateStreamItem(current, item.id, (entry) =>
          entry.task
            ? { ...entry, task: { ...entry.task, status: 'PLANNED' } }
            : entry,
        );
        if (!planned || secondWrite.kind === 'none') return planned;
        if (secondWrite.kind === 'reschedule') {
          return moveStreamItem(
            planned,
            item.id,
            targetDate,
            secondWrite.input.sort_order ??
              appendRankOnStream(planned, targetDate),
          );
        }
        // Same-day drop with a rank: mirror the agenda `/move` reorder over the
        // day's WHOLE pile (parked ranks shift too, like `handleSameDayMove`).
        const dayItems = dayItemsOf(streamItemsOf(planned), targetDate);
        const moved = applyAgendaMove(dayItems, item.id, secondWrite.sortOrder);
        if (moved === dayItems) return planned;
        return moved.reduce<AgendaPages | undefined>(
          (acc, entry) => updateStreamItem(acc, entry.id, () => entry),
          planned,
        );
      });
      setActionError(null);
      const second =
        secondWrite.kind === 'none'
          ? undefined
          : secondWrite.kind === 'reorder'
            ? () =>
                moveAgendaItemMutation.mutateAsync({
                  id: item.id,
                  input: { sort_order: secondWrite.sortOrder },
                })
            : () =>
                rescheduleAgendaItemMutation.mutateAsync({
                  id: item.id,
                  input: secondWrite.input,
                });
      await runAgendaWriteFlow({
        first: () =>
          moveTaskMutation.mutateAsync({
            id: task.id,
            input: { status: 'PLANNED' } satisfies MoveTaskInput,
          }),
        second,
        onFirstSuccess: (data) => {
          mergeRow(item.id, (entry) =>
            entry.task ? { ...entry, task: data.task } : entry,
          );
          setTasksCache((prev) => replaceCachedTask(prev, data.task));
        },
        onSecondSuccess: (data) => {
          mergeRow(data.item.id, () => data.item);
          setTasksCache((prev) => replaceCachedTask(prev, data.item.task));
          showMoveNotice(data.item);
        },
        onFirstFailure: (err) => {
          patchStream(() => snapshot);
          setActionNotice(null);
          setActionError(err instanceof Error ? err.message : 'Move failed');
        },
        onSecondFailure: (err, first) => {
          // The PLANNED transition succeeded; only the second write failed, so
          // the row still sits on its old date and rank. Repaint that row with
          // the first write's task embed — never a date or rank the server
          // refused — before asking the stream to re-read.
          patchStream(() =>
            updateStreamItem(snapshot, item.id, (entry) =>
              entry.task ? { ...entry, task: first.task } : entry,
            ),
          );
          setActionNotice(null);
          setActionError(
            secondWrite.kind === 'reorder'
              ? `Moved out of Waiting, but could not reorder it: ${
                  err instanceof Error ? err.message : 'Move failed'
                }`
              : `Moved out of Waiting, but could not place it on ${agendaShortDateLabel(
                  targetDate,
                  serverToday,
                )}: ${err instanceof Error ? err.message : 'Move failed'}`,
          );
          void queryClient.invalidateQueries({
            queryKey: queryKeys.agenda.all,
          });
        },
      });
    } finally {
      endRowAction(item.id);
    }
  };

  /** Drop a row into the Backlog (ADR 0007): `{ backlog: true }` returns a
   *  `PLANNED` task to `OPEN` (+ `unplanned` log), moves the row to today+50y
   *  and appends it to the Board's Open column. Painted optimistically with
   *  the SAME month clamp the server uses, then merged with the authoritative
   *  row (the server's real backlog date and status). A failure restores the
   *  snapshot and banners. The mutation invalidates the stream on settle —
   *  never awaited, so the rollback cannot race it. */
  const handleBacklogDrop = async (
    item: AgendaItemRecord,
    slot: 'calendar' | 'grip',
  ) => {
    const task = item.task;
    if (!task || !serverToday) return;
    if (!beginRowAction(item.id, slot)) return;
    try {
      const snapshot = readStream();
      patchStream((current) =>
        moveItemToBacklog(current, item.id, addMonthsClamp(serverToday, 600)),
      );
      setActionError(null);
      try {
        const data = await rescheduleAgendaItemMutation.mutateAsync({
          id: item.id,
          input: { backlog: true },
        });
        mergeRow(data.item.id, () => data.item);
        // Patch the shared tasks cache so Board/Lists see the flipped status
        // and the appended Open rank.
        setTasksCache((prev) => replaceCachedTask(prev, data.item.task));
        setActionNotice({
          message: 'Moved to Backlog',
          itemId: data.item.id,
          section: 'backlog',
        });
      } catch (err) {
        patchStream(() => snapshot);
        setActionNotice(null);
        setActionError(err instanceof Error ? err.message : 'Move failed');
      }
    } finally {
      endRowAction(item.id);
    }
  };

  /** Drop a row into the Planned warehouse (ADR 0010): the Board's
   *  `POST /api/tasks/:id/move { status: 'PLANNED' }` with **no rank** — the
   *  plan appends the task to the Planned column server-side and updates only
   *  the task's status: a dated agenda row keeps its day, an undated row keeps
   *  its sentinel. (The hovered card's rank would be a same-status reorder of
   *  that column, not an append.) Painted optimistically with the SAME horizon
   *  rule the server uses, then merged with the authoritative task, whose
   *  status and Board rank both land in the shared `['tasks']` cache. A failure
   *  restores the snapshot and banners; `useMoveTask` invalidates the stream on
   *  settle — never awaited, so the rollback cannot race it. */
  const handlePlanDrop = async (item: AgendaItemRecord) => {
    const task = item.task;
    if (!task || task.status !== 'OPEN' || !serverToday) return;
    if (!beginRowAction(item.id, 'grip')) return;
    try {
      const snapshot = readStream();
      patchStream((current) => planOpenItem(current, item.id, serverToday));
      setActionError(null);
      try {
        const data = await moveTaskMutation.mutateAsync({
          id: task.id,
          input: { status: 'PLANNED' } satisfies MoveTaskInput,
        });
        mergeRow(item.id, (entry) =>
          entry.task ? { ...entry, task: data.task } : entry,
        );
        setTasksCache((prev) => replaceCachedTask(prev, data.task));
        setActionNotice({
          message: 'Moved to Planned',
          itemId: item.id,
          section: 'planned',
        });
      } catch (err) {
        patchStream(() => snapshot);
        setActionNotice(null);
        setActionError(err instanceof Error ? err.message : 'Move failed');
      }
    } finally {
      endRowAction(item.id);
    }
  };

  /** Unschedule a dated `PLANNED` row (ADR 0010): `{ unschedule: true }` keeps
   *  the task `PLANNED`, writes no task log and does NOT re-rank the Planned
   *  column — only the agenda row travels to the backlog sentinel, appended.
   *  Painted optimistically with `unscheduleItem`, merged with the
   *  authoritative row on success; a failure restores the snapshot and
   *  banners. Occurrences, `OPEN` rows and terminal cards never get here. */
  const handleUnschedule = async (
    item: AgendaItemRecord,
    slot: 'calendar' | 'grip',
  ) => {
    if (!canUnschedule(item, serverToday)) return;
    if (!beginRowAction(item.id, slot)) return;
    try {
      const snapshot = readStream();
      patchStream((current) =>
        unscheduleItem(
          current,
          item.id,
          addMonthsClamp(serverToday, 600),
          serverToday,
        ),
      );
      setActionError(null);
      try {
        const data = await rescheduleAgendaItemMutation.mutateAsync({
          id: item.id,
          input: { unschedule: true },
        });
        mergeRow(data.item.id, () => data.item);
        setTasksCache((prev) => replaceCachedTask(prev, data.item.task));
        setActionNotice({
          message: 'Moved to Planned',
          itemId: data.item.id,
          section: 'planned',
        });
      } catch (err) {
        patchStream(() => snapshot);
        setActionNotice(null);
        setActionError(err instanceof Error ? err.message : 'Move failed');
      }
    } finally {
      endRowAction(item.id);
    }
  };

  /** Reorder one warehouse row onto another row's slot (ADR 0007 for OPEN,
   *  ADR 0010 for PLANNED): the Board's own
   *  `POST /api/tasks/:id/move { status, sort_order }`, so Home and the Board
   *  never disagree on a column's order. The optimistic paint mirrors the
   *  server's `reorder_in_place` over every task embed that shares that
   *  column's rank space — the mover takes the sent rank and its peers shift
   *  one — reorders the stretch where the user dropped it, and rebuilds the
   *  page's backlog as dated ++ planned ++ open, so a Planned reorder can never
   *  leave the planned rows after the open ones. A PLANNED reorder also shifts
   *  the dated `PLANNED` rows (same column, same rank space). The shared
   *  `['tasks']` cache gets the same peer shift on success, so the Board's
   *  column follows. A failure restores the snapshot and banners. */
  const handleWarehouseReorder = async (
    item: AgendaItemRecord,
    status: 'OPEN' | 'PLANNED',
    rows: AgendaItemRecord[],
    toIndex: number,
    target: number,
  ) => {
    const task = item.task;
    const fromIndex = rows.findIndex((entry) => entry.id === item.id);
    if (!task || fromIndex < 0 || toIndex < 0) return;
    if (!beginRowAction(item.id, 'grip')) return;
    try {
      // The new order — dnd-kit's own array move, so the row lands where the
      // user dropped it.
      const reordered = arrayMove(rows, fromIndex, toIndex);
      const order = new Map(reordered.map((entry, index) => [entry.id, index]));
      const snapshot = readStream();
      // The task embeds sharing this column's rank space. OPEN reorders only
      // touch the stretch; a PLANNED reorder is the whole Planned column, so the
      // dated PLANNED rows (they live in `items` / the dated backlog) shift too.
      const peerTasks =
        status === 'PLANNED'
          ? streamItemsOf(snapshot).flatMap((entry) =>
              entry.task && entry.task.status === 'PLANNED' ? [entry.task] : [],
            )
          : rows.flatMap((entry) => (entry.task ? [entry.task] : []));
      const shiftedRanks = new Map(
        applyOpenTaskReorder(peerTasks, task.id, target).map((entry) => [
          entry.id,
          entry.sort_order,
        ]),
      );
      /** `entry` with its shifted Board rank, or the same reference when its
       *  task is not part of this column (or kept its rank). */
      const withRank = (entry: AgendaItemRecord): AgendaItemRecord => {
        const rank = entry.task ? shiftedRanks.get(entry.task.id) : null;
        if (!entry.task || rank == null || rank === entry.task.sort_order) {
          return entry;
        }
        return { ...entry, task: { ...entry.task, sort_order: rank } };
      };
      const inDroppedOrder = (entries: AgendaItemRecord[]) =>
        entries
          .slice()
          .sort((a, b) => (order.get(a.id) ?? 0) - (order.get(b.id) ?? 0))
          .map(withRank);
      patchStream((current) => {
        if (!current) return current;
        return {
          ...current,
          pages: current.pages.map((page) => {
            const items = page.items.map(withRank);
            const backlog = page.backlog ?? [];
            if (!backlog.some((entry) => entry.id === item.id)) {
              // Only a PLANNED reorder changes rows off the mover's page (the
              // dated PLANNED ranks); keep the reference when nothing moved.
              return items.some((entry, index) => entry !== page.items[index])
                ? { ...page, items }
                : page;
            }
            // Dated rows keep their places ahead of both warehouses; only the
            // reordered stretch takes the new order. The rebuild preserves rows
            // that belong to neither warehouse pile (a stale optimistic
            // `IN_PROGRESS` at the horizon) instead of dropping them.
            const split = backlogStretchesForRebuild(backlog, serverToday);
            return {
              ...page,
              items,
              backlog: [
                ...split.dated.map(withRank),
                ...(status === 'PLANNED'
                  ? inDroppedOrder(split.planned)
                  : split.planned.map(withRank)),
                ...(status === 'OPEN'
                  ? inDroppedOrder(split.open)
                  : split.open.map(withRank)),
              ],
            };
          }),
        };
      });
      setActionError(null);
      try {
        const data = await moveTaskMutation.mutateAsync({
          id: task.id,
          input: { status, sort_order: target } satisfies MoveTaskInput,
        });
        mergeRow(item.id, (entry) =>
          entry.task ? { ...entry, task: data.task } : entry,
        );
        // Patch the shared tasks cache: the same peer shift, then the mover's
        // authoritative row, so Board/Lists never read a stale column order.
        setTasksCache((prev) =>
          applyOpenTaskReorder(prev, data.task.id, data.task.sort_order).map(
            (entry) => (entry.id === data.task.id ? data.task : entry),
          ),
        );
      } catch (err) {
        patchStream(() => snapshot);
        setActionError(err instanceof Error ? err.message : 'Move failed');
      }
    } finally {
      endRowAction(item.id);
    }
  };

  /** "Move all to today" on a past day: the server sweeps the row set
   *  `POST /api/agenda/days/:date/move-to-today` sweeps, appended to today's
   *  pile in their original order. Painted optimistically, reverted on
   *  failure, and re-read on settle (the sweep has server-side side effects a
   *  local mirror cannot guess). The sweep changes DATES only — an `OPEN` row
   *  it drags onto today stays `OPEN` (ADR 0007), so `moveStreamItem` keeps
   *  its default "leave the status alone". */
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
   *  top) or, for a finished row — `COMPLETED` or `DISCARDED` — back to
   *  PLANNED through the Board's `/move` (ADR 0004 amendment — uncheck is the
   *  Board reopen, no new endpoint; ADR 0002 — a discarded task reopens the
   *  same way, it is never completed). Status and date are decoupled: a dated
   *  row keeps its day; only an undated row is dated on civil today. The paint
   *  mirrors that undated-only rule. Reopening never stamps: living sorts by
   *  `sort_order`. */
  const handleCompleteTask = async (item: AgendaItemRecord) => {
    const task = item.task;
    if (!task) return;
    if (!beginRowAction(item.id, 'check')) return;
    try {
      const snapshot = readStream();
      // Reopen: COMPLETED / DISCARDED → PLANNED (living again — Play/Reschedule
      // return because the row is no longer a finished card).
      const wasDiscarded = task.status === 'DISCARDED';
      if (task.status === 'COMPLETED' || wasDiscarded) {
        patchStream((current) =>
          placeUndatedTaskOnToday(current, item.id, serverToday, (entry) =>
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
            prev.map((entry) =>
              entry.id === data.task.id ? data.task : entry,
            ),
          );
        } catch (err) {
          patchStream(() => snapshot);
          setActionError(
            err instanceof Error
              ? err.message
              : wasDiscarded
                ? 'Undo discard failed'
                : 'Uncheck failed',
          );
        }
        return;
      }
      patchStream((current) =>
        placeUndatedTaskOnToday(current, item.id, serverToday, (entry) =>
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
    } finally {
      endRowAction(item.id);
    }
  };

  /** Task → the existing `/start` (Board → In Progress). A `WAITING` task
   *  takes back the hand-off the same way (ADR 0011): `/start` opens the
   *  review event and flips the status. A dated row keeps its day; only an
   *  undated row is dated on civil today. The FocusTimer is a separate surface
   *  and this just flips the card. */
  const handleStartTask = async (item: AgendaItemRecord) => {
    const task = item.task;
    if (!task) return;
    if (!beginRowAction(item.id, 'play')) return;
    try {
      const snapshot = readStream();
      patchStream((current) =>
        placeUndatedTaskOnToday(current, item.id, serverToday, (entry) =>
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
    } finally {
      endRowAction(item.id);
    }
  };

  /** Task → `/pause` (ADR 0002: IN_PROGRESS → PLANNED, event end patched
   *  closed). Pause never moves the row's day; Play returns once the row is
   *  planned again. Occurrence pause is a separate verb. */
  const handlePauseTask = async (item: AgendaItemRecord) => {
    const task = item.task;
    if (!task) return;
    if (!beginRowAction(item.id, 'play')) return;
    try {
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
    } finally {
      endRowAction(item.id);
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
    // Only a complete takes the row lock: undoing a skip is the reopen
    // path and must never spin the circle.
    const locksRow = verb === 'complete';
    if (locksRow && !beginRowAction(item.id, 'check')) return;
    try {
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
            ? await completeOccurrenceMutation.mutateAsync({
                id: occurrence.id,
              })
            : await skipOccurrenceMutation.mutateAsync({ id: occurrence.id });
        mergeRow(item.id, (entry) =>
          entry.occurrence ? { ...entry, occurrence: data.occurrence } : entry,
        );
      } catch (err) {
        patchStream(() => snapshot);
        setActionError(err instanceof Error ? err.message : 'Update failed');
      }
    } finally {
      if (locksRow) endRowAction(item.id);
    }
  };

  /** Occurrence reopen — the uncheck/unskip path (ADR 0004 amendment):
   *  done/skipped → pending with the chip ids cleared (`calendar_id` +
   *  `google_event_id` → null, matching the server's reopen). Same
   *  optimistic shape as setOccurrenceStatus: paint pending immediately,
   *  merge the authoritative occurrence on success, snapshot-rollback +
   *  banner on failure. */
  const handleReopenOccurrence = async (
    item: AgendaItemRecord,
    slot: 'check' | null,
  ) => {
    const occurrence = item.occurrence;
    if (!occurrence) return;
    // Undoing a skip never locks the row (no spinner); unchecking a
    // done occurrence is the circle's save and does.
    if (slot && !beginRowAction(item.id, slot)) return;
    try {
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
    } finally {
      if (slot) endRowAction(item.id);
    }
  };

  /** Occurrence start: creates the one-shot Google log and flips the chip to
   *  in_progress. Today-only on the server (the row only offers Play on
   *  today). Optimistic → the chip reads In progress immediately; a 401/400
   *  rolls back with a banner. */
  const handleStartOccurrence = async (item: AgendaItemRecord) => {
    const occurrence = item.occurrence;
    if (!occurrence) return;
    if (!beginRowAction(item.id, 'play')) return;
    try {
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
    } finally {
      endRowAction(item.id);
    }
  };

  /** Occurrence pause (ADR 0004 amendment): close the living chip and
   *  return to pending with ids cleared. Optimistic → Play returns
   *  immediately; a 401/400 rolls back with a banner. */
  const handlePauseOccurrence = async (item: AgendaItemRecord) => {
    const occurrence = item.occurrence;
    if (!occurrence) return;
    if (!beginRowAction(item.id, 'play')) return;
    try {
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
    } finally {
      endRowAction(item.id);
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
    // omits it server-side; drop it locally now (the row can sit in a page's
    // day list or in its backlog).
    patchStream((current) =>
      current
        ? {
            ...current,
            pages: current.pages.map((page) => {
              const hasTask = (entry: AgendaItemRecord) =>
                entry.task?.id === taskId;
              const items = page.items.filter((entry) => !hasTask(entry));
              const backlog = (page.backlog ?? []).filter(
                (entry) => !hasTask(entry),
              );
              return items.length === page.items.length &&
                backlog.length === (page.backlog?.length ?? 0)
                ? page
                : { ...page, items, backlog };
            }),
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
  // Stream scrolling: the history sentinel, anchor landing, Today
  // ──────────────────────────────────────────

  const scrollRef = useRef<HTMLDivElement>(null);
  // The empty Waiting strip is hit-tested against its live box, clipped to
  // the stream's own visible box — a sticky header scrolled past either edge
  // must never accept a drop.
  const agendaCollision = useCallback(
    (args: Parameters<typeof agendaCollisionDetection>[0]) =>
      agendaCollisionDetection(args, scrollRef.current),
    [],
  );
  const topSentinelRef = useRef<HTMLDivElement>(null);
  // The top loading row is real height above the day headers; its measured
  // height is added to (or removed from) `scrollTop` when it mounts/unmounts so
  // the content under it does not move.
  const historyLoadingRowRef = useRef<HTMLDivElement>(null);
  const historyLoadingRowHeightRef = useRef(0);
  // Last known intersection of the top sentinel — the observer only fires on
  // CHANGES, so a moored sentinel needs this to be re-checked after every
  // fetch settles (see `maybeFetchMore`).
  const topVisibleRef = useRef(false);
  // History loads stay off until the anchor landing scroll has been placed:
  // an auto-load during that first paint would fetch a week and shift the
  // stream before the user has seen the anchor. Armed at the end of the
  // landing layout effect (see below).
  const historyArmedRef = useRef(false);
  // Scroll geometry captured before a history week loads, so the extra height
  // can be added to `scrollTop` afterwards — the anchor must not jump.
  const prependRef = useRef<{
    scrollHeight: number;
    // The oldest history week loaded at capture time (null when none was):
    // history pages are APPENDED, so a new week is a prepend in the rendered
    // spine exactly when this moves back.
    oldestFrom: string | null;
    pageCount: number;
  } | null>(null);
  const landedOnAnchorRef = useRef(false);
  const [todayVisible, setTodayVisible] = useState(false);
  // The Today button's scroll waits for React to commit the painted day. State,
  // not a ref: clicking Today while its day is ALREADY loaded (just off screen)
  // fetches nothing, so nothing else would re-run the layout effect that
  // scrolls.
  const [scrollTodayPending, setScrollTodayPending] = useState(false);

  /** Success notice's Show: paint the flash and open any collapsed Waiting
   *  section holding the row, then scroll once that commit is in the DOM. The
   *  clipped body must never be used as the scroll geometry — its rows are
   *  stacked outside the zero-height grid row until the section expands. */
  const showDestination = (notice: ActionNotice) => {
    setFlashItemId(notice.itemId);
    setPendingShow(notice);
    setActionNotice(null);
  };

  useLayoutEffect(() => {
    if (!pendingShow) return;
    let target = document.getElementById(`agenda-item-${pendingShow.itemId}`);
    if (!target && pendingShow.date) {
      target = document.getElementById(dayElementId(pendingShow.date));
    }
    if (!target && pendingShow.date) {
      target = document.getElementById(backlogDayElementId(pendingShow.date));
    }
    if (!target) {
      target = document.getElementById(
        pendingShow.section === 'planned'
          ? PLANNED_DROPPABLE_ID
          : BACKLOG_DROPPABLE_ID,
      );
    }
    if (target) {
      const isRow = target.id.startsWith('agenda-item-');
      target.scrollIntoView({ block: isRow ? 'center' : 'start' });
    }
    setPendingShow(null);
  }, [pendingShow]);

  // The history walk's own flags: older weeks APPEND to the history query, so
  // the top-edge gesture is a `fetchNextPage` while `hasNextPage`.
  const hasOlderHistory = history.hasNextPage;
  const isFetchingOlderHistory = history.isFetchingNextPage;
  // Page 0 (the anchor's eve week) and each appended week show the same
  // top-of-stream spinner. A background refetch does neither.
  const isLoadingOlderHistory = history.isLoading || isFetchingOlderHistory;
  const fetchOlderHistory = history.fetchNextPage;
  const pageCount = pages.length;
  const hasPages = pageCount > 0;

  const rememberPrepend = useCallback(() => {
    const root = scrollRef.current;
    if (!root) return;
    prependRef.current = {
      scrollHeight: root.scrollHeight,
      oldestFrom: historyOldestFrom,
      pageCount: historyPages.length,
    };
  }, [historyOldestFrom, historyPages.length]);

  /** The history gesture: the stream is sitting at its top edge. The anchor
   *  day lands aligned with the top sentinel, so `scrollTop` starts at ~0 —
   *  there is no room left for a "moved up by 4px" delta, and a wheel-up at
   *  the clamped edge fires no scroll event at all. `scrollTop <= 1` catches
   *  the edge itself; `topVisibleRef` catches the approach. Returns true when
   *  a fetch was started (so the caller can skip the opposite direction).
   *
   *  The first hit is what turns the history query on (state, so the hook
   *  re-renders and page 0 — the anchor's eve week — loads); every later hit
   *  appends the week before the oldest loaded one. */
  const maybeFetchPreviousPage = useCallback((): boolean => {
    if (!historyArmedRef.current) return false; // still landing on the anchor
    const root = scrollRef.current;
    if (!root) return false;
    if (root.scrollTop > 1 && !topVisibleRef.current) return false;
    if (!historyEnabled) {
      // Nothing older than the anchor: there is no page 0, so Home never asks
      // for an empty week (the query stays off).
      if (!anchorDate || !historyPageZero(anchorDate, earliestDate)) {
        return false;
      }
      rememberPrepend();
      setHistoryEnabled(true);
      return true;
    }
    if (!hasOlderHistory || isFetchingOlderHistory) return false;
    rememberPrepend();
    void fetchOlderHistory();
    return true;
  }, [
    anchorDate,
    earliestDate,
    historyEnabled,
    hasOlderHistory,
    isFetchingOlderHistory,
    fetchOlderHistory,
    rememberPrepend,
  ]);

  /** Load the previous week when the viewport asks for it. Called from the
   *  sentinel observer and — because an observer only fires on intersection
   *  CHANGES, and the sentinel can already be moored in view — from the
   *  scroll listener and after each settle. There is no forward counterpart:
   *  the stream ends at today+6. */
  const maybeFetchMore = useCallback(() => {
    maybeFetchPreviousPage();
  }, [maybeFetchPreviousPage]);

  // Any scroll re-checks the top edge: after the landing scroll the container
  // is parked at `scrollTop ≈ 0`, where an upward gesture cannot produce
  // another scroll event, and the observer already reported the sentinel as
  // intersecting — this call is what makes sitting at the top load history.
  useEffect(() => {
    const root = scrollRef.current;
    if (!root) return;
    const onScroll = () => {
      maybeFetchPreviousPage();
    };
    root.addEventListener('scroll', onScroll, { passive: true });
    return () => root.removeEventListener('scroll', onScroll);
  }, [maybeFetchPreviousPage]);

  // The history sentinel inside the stream's own scroll container. The
  // observer is rebuilt whenever a fetch settles so a still-visible sentinel
  // gets a fresh initial callback.
  useEffect(() => {
    const root = scrollRef.current;
    const top = topSentinelRef.current;
    if (!root || !top || pageCount === 0) return;
    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (entry.target === top) {
            topVisibleRef.current = entry.isIntersecting;
          }
        }
        maybeFetchMore();
      },
      { root, rootMargin: '240px 0px' },
    );
    observer.observe(top);
    return () => observer.disconnect();
  }, [pageCount, hasOlderHistory, isFetchingOlderHistory, maybeFetchMore]);

  // Land on `anchor_date` once, aligned to the top of the stream. Later
  // prepends must NOT re-run this (the height compensation below keeps the
  // anchor exactly where the user left it).
  useLayoutEffect(() => {
    if (!anchorDate) return;
    const root = scrollRef.current;
    const target = root?.querySelector<HTMLElement>(
      `#${dayElementId(anchorDate)}`,
    );
    if (!target) return;
    if (!landedOnAnchorRef.current) {
      landedOnAnchorRef.current = true;
      target.scrollIntoView({ block: 'start' });
    }
    // The landing scroll is placed: the anchor day now starts at the stream's
    // top edge, which is exactly where history begins. Only from here on does
    // "at the top" mean "load the previous week" — the previous code waited
    // for a >4px upward scroll that this edge never leaves room for. The next
    // frame re-checks once the scroll has settled, because a wheel gesture at
    // `scrollTop ≈ 0` cannot move the container and would fire no event.
    historyArmedRef.current = true;
    const frame = requestAnimationFrame(() => maybeFetchMore());
    return () => cancelAnimationFrame(frame);
  }, [anchorDate, days, maybeFetchMore]);

  // A history week adds height ABOVE the viewport: add exactly that much to
  // `scrollTop` so the day under the user's eyes does not jump. History pages
  // are APPENDED, so a new week is a prepend in the rendered spine exactly
  // when the oldest loaded `from` moves back. Only a REAL prepend counts — an
  // optimistic `setQueryData` while a fetch is in flight must leave the
  // recorded geometry alone, or the week that finally arrives gets no
  // correction at all.
  useLayoutEffect(() => {
    const root = scrollRef.current;
    const recorded = prependRef.current;
    if (!root || !recorded) return;
    if (
      historyPages.length <= recorded.pageCount ||
      historyOldestFrom === recorded.oldestFrom
    ) {
      // An optimistic edit, a failed fetch, or a quiet refetch that re-read
      // the same weeks — the viewport is already right, and the pending
      // prepend (if any) is still coming.
      return;
    }
    prependRef.current = null;
    const delta = root.scrollHeight - recorded.scrollHeight;
    if (delta > 0) {
      root.scrollTop += delta;
      // The new height pushes the top sentinel out of view (well past the
      // observer's 240px margin), but its stale "intersecting" flag would let
      // the scroll event from this very write start another prepend. The next
      // observer callback confirms the move.
      topVisibleRef.current = false;
    }
  }, [historyPages, historyOldestFrom]);

  // The loading row is real height above the day headers. Move `scrollTop`
  // with it as it mounts (a fetch starts) or unmounts (the fetch settles), or
  // the stream would shift by the row's height on every load. A successful
  // fetch also prepends a week; that height is handled separately above.
  useLayoutEffect(() => {
    const root = scrollRef.current;
    if (!root) return;
    const next = historyLoadingRowRef.current?.offsetHeight ?? 0;
    const previous = historyLoadingRowHeightRef.current;
    if (next === previous) return;
    historyLoadingRowHeightRef.current = next;
    root.scrollTop += next - previous;
  }, [hasPages, isLoadingOlderHistory]);

  // The history fetch settled without growing the cache into a new oldest week
  // (it failed, or the server had nothing older): drop the pending measurement
  // rather than let it sit across later unrelated page changes. A passive
  // effect, on purpose — layout effects have already run for this commit, so a
  // prepend that DID land has been applied above.
  useEffect(() => {
    if (history.isFetching) return;
    const recorded = prependRef.current;
    if (!recorded) return;
    if (
      historyPages.length > recorded.pageCount &&
      historyOldestFrom !== recorded.oldestFrom
    ) {
      return; // the layout effect above consumed it
    }
    prependRef.current = null;
  }, [history.isFetching, historyPages, historyOldestFrom]);

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

  /** The floating Today button: today sits on the default read (`anchor_date
   *  <= today <= today+6`), which is always loaded once Home paints, so this
   *  only has to scroll its header into view — no crawling up through history
   *  to search unloaded days. The scroll itself is placed by the layout effect
   *  below: the day is not in the DOM until the read commits its days. */
  const scrollToToday = () => {
    if (!scrollRef.current || !serverToday) return;
    historyArmedRef.current = true;
    setScrollTodayPending(true);
  };

  // Today is in the DOM: place the pending scroll now. Fires on the commit
  // that painted today's day, and again if the day was already loaded when
  // the button was clicked (no page fetch, so nothing else would re-run).
  useLayoutEffect(() => {
    if (!scrollTodayPending || !serverToday) return;
    const target = scrollRef.current?.querySelector<HTMLElement>(
      `#${dayElementId(serverToday)}`,
    );
    if (!target) return;
    target.scrollIntoView({ block: 'start' });
    setScrollTodayPending(false);
  }, [scrollTodayPending, serverToday, days]);

  // ──────────────────────────────────────────
  // Render
  // ──────────────────────────────────────────

  // The default read drives the page: no pages until it resolves, and its
  // failure is the only one that can wipe the stream.
  const loadError =
    current.error instanceof Error
      ? current.error.message
      : current.error
        ? 'Failed to load agenda'
        : null;
  // A history fetch failed: a one-line notice above the still-mounted days —
  // never a wipe of today. `isFetchNextPageError` covers an appended week; a
  // history query that errored with no pages at all is the opening eve-week
  // fetch (an `enabled` flip, which TanStack does not tag as a fetch-more).
  const historyFetchError =
    history.isFetchNextPageError ||
    (historyEnabled && !history.data && history.isError);
  const pageFetchError = historyFetchError || (hasPages && !!loadError);

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
    onReschedule: (entry, date) =>
      void handleCrossDayMove(entry, date, 'calendar'),
    onMoveToBacklog: (entry) => void handleBacklogDrop(entry, 'calendar'),
    onUnschedule: (entry) => void handleUnschedule(entry, 'calendar'),
    onCompleteTask: (entry) => void handleCompleteTask(entry),
    onStartTask: (entry) => void handleStartTask(entry),
    onPauseTask: (entry) => void handlePauseTask(entry),
    onOpenTask: (task) => setTaskModal(task),
    // Unchecking a done occurrence is the circle's own save (it spins);
    // undoing a skip is not — it never takes the row lock.
    onCompleteOccurrence: (entry) =>
      entry.occurrence?.status === 'done'
        ? void handleReopenOccurrence(entry, 'check')
        : void setOccurrenceStatus(entry, 'done', 'complete'),
    onSkipOccurrence: (entry) =>
      entry.occurrence?.status === 'skipped'
        ? void handleReopenOccurrence(entry, null)
        : void setOccurrenceStatus(entry, 'skipped', 'skip'),
    onStartOccurrence: (entry) => void handleStartOccurrence(entry),
    onPauseOccurrence: (entry) => void handlePauseOccurrence(entry),
    onRenameOccurrence: (entry) => openRename(entry),
    pendingOf,
  };

  return (
    <div className="min-h-screen bg-cream">
      <div className="mx-auto flex h-[100dvh] max-w-7xl flex-col px-6 pt-8 lg:block lg:h-auto">
        <div className="flex min-h-0 flex-1 flex-col gap-8 lg:grid lg:min-h-0 lg:grid-cols-3">
          {/* Main column — the day stream */}
          <div className="flex min-h-0 min-w-0 flex-1 flex-col lg:col-span-2 lg:block">
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

            {/* Action error banner (move/complete/skip failures) — the stream
                stays mounted */}
            {actionError && (
              <div className="mb-3 flex items-center justify-between gap-4 rounded-xl bg-destructive/10 px-4 py-3 text-destructive">
                <p className="selectable text-sm">{actionError}</p>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setActionError(null)}
                >
                  Dismiss
                </Button>
              </div>
            )}

            {/* A successful move's confirmation: the row may have left the
                visible day, so Show scrolls the stream to it. */}
            {actionNotice && !actionError && (
              <div
                role="status"
                aria-live="polite"
                className="mb-3 flex items-center gap-2 rounded-xl bg-primary/10 px-3 py-2 text-foreground"
              >
                <Check className="h-4 w-4 shrink-0 text-primary" />
                <p
                  className="min-w-0 flex-1 truncate text-sm"
                  title={actionNotice.message}
                >
                  {actionNotice.message}
                </p>
                <button
                  type="button"
                  onClick={() => showDestination(actionNotice)}
                  className="shrink-0 rounded-md px-2 py-1 text-xs font-semibold text-primary hover:bg-primary/10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/30"
                >
                  Show
                </button>
                <button
                  type="button"
                  onClick={() => setActionNotice(null)}
                  aria-label="Dismiss move confirmation"
                  className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-primary/10 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/30"
                >
                  <X className="h-3.5 w-3.5" />
                </button>
              </div>
            )}

            {/* A later page failed: a one-line notice, never a wipe */}
            {pageFetchError && (
              <div className="mb-3 flex items-center justify-between gap-3 rounded-lg bg-destructive/10 px-3 py-2 text-destructive">
                <p className="text-xs">Could not load more days.</p>
                <button
                  type="button"
                  onClick={() => {
                    // A failed history page retries itself: `fetchNextPage` on
                    // a query with no pages yet re-reads page 0 from
                    // `initialPageParam` (the eve week).
                    if (historyFetchError) {
                      rememberPrepend();
                      void history.fetchNextPage();
                    } else {
                      void current.refetch();
                    }
                  }}
                  className="text-xs font-medium underline"
                >
                  Retry
                </button>
              </div>
            )}

            {/* The stream: its own scroll container, so the page itself never
                grows without bound and dnd-kit can auto-scroll a drag. On
                mobile it is the flex remainder under the timer and quote, so
                the compact quote never pushes the stream past the viewport.
                Native scroll anchoring is off: the prepend compensation below
                already shifts `scrollTop` by the new week's height, and the
                browser's own shift would land on top of it (a double jump). */}
            <div
              ref={scrollRef}
              className="relative min-h-0 flex-1 overflow-y-auto overscroll-contain bg-cream [overflow-anchor:none] lg:h-[calc(100dvh-5.25rem)] lg:flex-none"
            >
              {/* First load — the default read has not resolved yet */}
              {!hasPages && current.isLoading && (
                <div className="flex items-center justify-center gap-2 py-24 text-muted-foreground">
                  <Loader2 className="h-5 w-5 animate-spin" />
                  Loading agenda…
                </div>
              )}

              {/* First fetch failed and there is nothing to show */}
              {!hasPages && !current.isLoading && loadError && (
                <div className="m-2 flex items-center justify-between gap-4 rounded-xl bg-destructive/10 px-4 py-3 text-destructive">
                  <p className="selectable text-sm">{loadError}</p>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => void current.refetch()}
                  >
                    Retry
                  </Button>
                </div>
              )}

              {hasPages && (
                <>
                  {/* A real row above the sticky day headers — one row above
                      the date the user is looking at. Its height is compensated
                      in `scrollTop` when it mounts/unmounts. */}
                  {isLoadingOlderHistory && (
                    <div
                      ref={historyLoadingRowRef}
                      role="status"
                      className="sticky top-0 z-30 flex h-8 items-center justify-center gap-2 border-b border-border/50 bg-cream/95 text-muted-foreground backdrop-blur"
                    >
                      <Loader2 className="h-3.5 w-3.5 animate-spin" />
                      <span className="text-xs font-medium">
                        Loading earlier days…
                      </span>
                    </div>
                  )}
                  <div
                    ref={topSentinelRef}
                    aria-hidden
                    className="h-1 w-full"
                  />
                  <DndContext
                    sensors={sensors}
                    collisionDetection={agendaCollision}
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
                        loadingHistory={isLoadingOlderHistory}
                        highlightedItemId={flashItemId}
                        dragging={activeDrag?.kind === 'task'}
                        onMoveDayToToday={(target) =>
                          void handleMoveDayToToday(target)
                        }
                      />
                    ))}

                    {/* Under the last stream day: the two warehouses (ADR
                        0007, ADR 0010) — Planned first, then the Backlog
                        (dated rows grouped by day, then the Board-ordered OPEN
                        stretch). Each is its own drop target, so an empty
                        warehouse still accepts a row. */}
                    <PlannedSection
                      serverToday={serverToday}
                      planned={backlogSplit.planned}
                      handlers={handlers}
                      highlightedItemId={flashItemId}
                    />

                    <BacklogSection
                      serverToday={serverToday}
                      dated={backlogSplit.dated}
                      open={backlogSplit.open}
                      handlers={handlers}
                      highlightedItemId={flashItemId}
                    />

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
                </>
              )}
            </div>
          </div>

          {/* Right panel — Focus & Quotes. Below lg this panel LEADS the page
              (`order-first`): the compact FocusTimer and the compact quote
              strip sit above the stream, and the stream takes the remaining
              height. At lg it returns to the right column, timer on top and
              the full quote card below. One instance of each: the timer owns
              its state, and the quote strip owns its carousel index. */}
          <div className="order-first space-y-2 lg:order-none lg:space-y-6">
            <div className="max-lg:sticky max-lg:top-2 max-lg:z-20">
              <FocusTimer />
            </div>
            <QuotesSection quotes={quotes} />
          </div>
        </div>
      </div>

      {/* Today: jump back from anywhere in the stream (hidden while today's
          header is on screen) */}
      {serverToday && !todayVisible && (
        <div className="pointer-events-none fixed inset-x-0 bottom-24 z-40">
          <div className="mx-auto max-w-7xl px-6">
            <div className="grid grid-cols-1 gap-8 lg:grid-cols-3">
              <div className="flex justify-center lg:col-span-2">
                <button
                  type="button"
                  onClick={() => void scrollToToday()}
                  className="pointer-events-auto rounded-full border border-border bg-card px-4 py-2 text-sm font-medium text-foreground shadow-lg transition-colors hover:bg-muted"
                >
                  Today
                </button>
              </div>
            </div>
          </div>
        </div>
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
              <p className="selectable mt-3 text-sm text-destructive">
                {renameError}
              </p>
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
