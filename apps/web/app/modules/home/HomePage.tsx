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
// Planned plans an `OPEN` row (`/move { status: 'PLANNED' }` with no rank — it
// never lands on today, the row parks on the sentinel), and dragging a dated
// `PLANNED` row there unschedules it (`{ unschedule: true }`: status, log and
// Board rank all untouched). Dragging inside one warehouse is that column's
// Board task `/move`, so Home and Board share one order per column;
// occurrences and running tasks are never dropped there. The old header date
// picker, the add-task picker and remove-from-day are gone — tasks are born on
// the Board, and a task's row follows the task's own lifecycle. A row's
// calendar menu offers today-relative presets, a month picker and (for tasks)
// Unschedule / Backlog.
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
  closestCenter,
  useDroppable,
  useSensor,
  useSensors,
  type DragEndEvent,
} from '@dnd-kit/core';
import {
  SortableContext,
  arrayMove,
  verticalListSortingStrategy,
} from '@dnd-kit/sortable';
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
import { AgendaItemRow, SortableAgendaItemRow } from './AgendaItemRow';
import { addCivilDays } from '../routines/rrule-preview';
import {
  addMonthsClamp,
  agendaDateLabel,
  agendaShortDateLabel,
  agendaMoveTargetAt,
  appendRankOnStream,
  applyAgendaMove,
  applyOpenTaskReorder,
  canUnschedule,
  crossDaySortOrder,
  dedupeAgendaItems,
  backlogStretchesForRebuild,
  eachCivilDate,
  groupItemsByDate,
  historyPageZero,
  isAgendaItemPending,
  isOpenBacklogTask,
  isPlannedBacklogTask,
  moveItemToBacklog,
  moveStreamItem,
  openTaskMoveTargetAt,
  partitionAgendaItems,
  planOpenItem,
  replaceCachedTask,
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

/** Every day's drop container id (`day-2026-08-23`) — also the day section's
 *  DOM id, which is how the initial scroll and the Today button find a day. */
const DAY_ID_PREFIX = 'day-';
/** The Backlog section's droppable id — a constant, never a `day-` id. */
const BACKLOG_DROPPABLE_ID = 'backlog';
/** The Planned section's droppable id — a constant, never a `day-` id and
 *  never the sentinel date: a day header at 2076 would be a future droppable
 *  and the API would 400 the reschedule it implies. */
const PLANNED_DROPPABLE_ID = 'planned';
/** A dated stretch group inside the Backlog (past today+6 … today+10y−1). */
const BACKLOG_DAY_ID_PREFIX = 'backlog-day-';

function dayElementId(date: string): string {
  return `${DAY_ID_PREFIX}${date}`;
}

function backlogDayElementId(date: string): string {
  return `${BACKLOG_DAY_ID_PREFIX}${date}`;
}

/** The default read's own `anchor_date` as the cache holds it right now — the
 *  key that names Home's history cache and the split in `writeAgendaPages`.
 *  Empty until the default read resolves (nothing to read or write then). */
function currentAnchor(): string {
  return (
    queryClient.getQueryData<AgendaResponse>(queryKeys.agenda.current())
      ?.anchor_date ?? ''
  );
}

interface AgendaRowHandlers {
  onReschedule: (item: AgendaItemRecord, date: string) => void;
  onMoveToBacklog: (item: AgendaItemRecord) => void;
  /** Park a dated `PLANNED` row in the Planned warehouse (`{ unschedule: true
   *  }`, ADR 0009) — never a status change. Task rows only. */
  onUnschedule: (item: AgendaItemRecord) => void;
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

/** A successful move's transient confirmation. `itemId` lets Show scroll
 *  straight to the exact row (the day header is only a fallback). */
interface ActionNotice {
  message: string;
  itemId: string;
  /** Set for a date move; omitted for a warehouse move. */
  date?: string;
  /** Set by the warehouse moves so Show falls back to the RIGHT section header
   *  when the row itself is not mounted (a Planned move never scrolls to the
   *  Backlog header, and vice versa). */
  section?: 'planned' | 'backlog';
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
  loadingHistory,
  highlightedItemId,
}: {
  day: string;
  serverToday: string;
  items: AgendaItemRecord[];
  handlers: AgendaRowHandlers;
  onMoveDayToToday: (day: string) => void;
  loadingHistory: boolean;
  highlightedItemId: string | null;
}) {
  const { living, completed } = partitionAgendaItems(items);
  const pendingCount = items.filter(isAgendaItemPending).length;
  // The count above includes a running occurrence, but the sweep leaves it
  // behind — the button may only appear when something would actually move.
  const canMoveDay = items.some(canMoveToToday);
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
  // While the history row is above the stream, the day headers stick below
  // it (the row is 2rem tall) instead of sliding underneath it.
  const stickyTop = loadingHistory ? 'top-8' : 'top-0';

  return (
    <section id={dayElementId(day)}>
      <header
        className={
          empty
            ? `sticky ${stickyTop} z-10 flex items-center gap-2 border-b border-border/50 bg-cream/95 px-1 py-1.5 backdrop-blur`
            : `sticky ${stickyTop} z-10 flex items-center gap-2 border-b border-border/60 bg-cream/95 px-1 py-2 backdrop-blur`
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
            {canMoveDay && (
              <button
                type="button"
                onClick={() => onMoveDayToToday(day)}
                aria-label={`Move all pending items from ${day} to today`}
                className="ml-auto flex-shrink-0 rounded-full bg-amber-400/15 px-2 py-1 text-[11px] font-medium text-amber-700 transition-colors hover:bg-amber-400/25"
              >
                Move all to today
              </button>
            )}
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
                    highlighted={highlightedItemId === entry.id}
                    {...handlers}
                  />
                ) : (
                  <AgendaItemRow
                    key={entry.id}
                    item={entry}
                    isDragging={false}
                    showHandle={false}
                    serverToday={serverToday}
                    highlighted={highlightedItemId === entry.id}
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
                highlighted={highlightedItemId === entry.id}
                {...handlers}
              />
            ))}
          </div>
        </section>
      )}
    </section>
  );
}

/** The Planned warehouse (ADR 0009): every `PLANNED` row parked at/after the
 *  10-year horizon, in the Planned column's Board order — the server already
 *  sorted it, so it is never re-sorted here. Always visible, so an empty
 *  section still accepts a drop; a drop ON the section means "the end of the
 *  Planned list", exactly like a day container. Reordering is the Board's
 *  `POST /api/tasks/:id/move { status: 'PLANNED', sort_order }`, never the
 *  agenda's `/move`. It sits immediately ABOVE the Backlog and owns nothing
 *  else: dated `PLANNED` rows stay in their day groups in the stream. */
function PlannedSection({
  serverToday,
  planned,
  handlers,
  highlightedItemId,
}: {
  serverToday: string;
  planned: AgendaItemRecord[];
  handlers: AgendaRowHandlers;
  highlightedItemId: string | null;
}) {
  const { setNodeRef, isOver } = useDroppable({ id: PLANNED_DROPPABLE_ID });

  const renderRow = (entry: AgendaItemRecord) =>
    isAgendaRowDraggable(entry) ? (
      <SortableAgendaItemRow
        key={entry.id}
        item={entry}
        serverToday={serverToday}
        highlighted={highlightedItemId === entry.id}
        {...handlers}
      />
    ) : (
      <AgendaItemRow
        key={entry.id}
        item={entry}
        isDragging={false}
        showHandle={false}
        serverToday={serverToday}
        highlighted={highlightedItemId === entry.id}
        {...handlers}
      />
    );

  return (
    <section ref={setNodeRef} id={PLANNED_DROPPABLE_ID} aria-label="Planned">
      <header className="sticky top-0 z-10 flex items-center gap-2 border-b border-border/60 bg-cream/95 px-1 py-2 backdrop-blur">
        <h2 className="font-heading text-sm font-semibold text-foreground">
          Planned
        </h2>
      </header>

      <div
        className={
          isOver
            ? 'min-h-10 rounded-lg bg-primary/5 px-1 py-2 ring-1 ring-primary/20 transition-colors'
            : 'min-h-10 px-1 py-2 transition-colors'
        }
      >
        {planned.length === 0 && (
          <p className="px-1 py-1.5 text-sm italic text-muted-foreground/70">
            Nothing planned
          </p>
        )}
        {planned.length > 0 && (
          <SortableContext
            items={planned
              .filter(isAgendaRowDraggable)
              .map((entry) => entry.id)}
            strategy={verticalListSortingStrategy}
          >
            <div className="space-y-2">{planned.map(renderRow)}</div>
          </SortableContext>
        )}
      </div>
    </section>
  );
}

/** The Backlog (ADR 0007): everything past the day stream, in the two
 *  stretches the server returns — dated rows grouped by their own day (a
 *  same-day drop reorders through the agenda `/move`; a drop onto another
 *  dated day reschedules onto that date, which the server accepts because every
 *  one of them is `>= today`), then the `OPEN` rows as ONE Board-ordered list
 *  (reordering them is the Board's task `/move`, never the agenda). The section
 *  itself is the droppable that sends a row back to the backlog, so an empty
 *  section still accepts a drop. Warehoused Planned rows (ADR 0009) live in
 *  their own section above; dated Planned rows still render in their day
 *  groups here. */
function BacklogSection({
  serverToday,
  dated,
  open,
  handlers,
  highlightedItemId,
}: {
  serverToday: string;
  dated: AgendaItemRecord[];
  open: AgendaItemRecord[];
  handlers: AgendaRowHandlers;
  highlightedItemId: string | null;
}) {
  const { setNodeRef, isOver } = useDroppable({ id: BACKLOG_DROPPABLE_ID });
  // Server order already puts the dated stretch first; grouping in encounter
  // order keeps each day's rows in `sort_order`, and never alphabetizes.
  const datedGroups = useMemo(() => groupItemsByDate(dated), [dated]);
  const empty = dated.length === 0 && open.length === 0;

  const renderRow = (entry: AgendaItemRecord) =>
    isAgendaRowDraggable(entry) ? (
      <SortableAgendaItemRow
        key={entry.id}
        item={entry}
        serverToday={serverToday}
        highlighted={highlightedItemId === entry.id}
        {...handlers}
      />
    ) : (
      <AgendaItemRow
        key={entry.id}
        item={entry}
        isDragging={false}
        showHandle={false}
        serverToday={serverToday}
        highlighted={highlightedItemId === entry.id}
        {...handlers}
      />
    );

  return (
    <section ref={setNodeRef} id={BACKLOG_DROPPABLE_ID} aria-label="Backlog">
      <header className="sticky top-0 z-10 flex items-center gap-2 border-b border-border/60 bg-cream/95 px-1 py-2 backdrop-blur">
        <h2 className="font-heading text-sm font-semibold text-foreground">
          Backlog
        </h2>
      </header>

      <div
        className={
          isOver
            ? 'min-h-10 rounded-lg bg-primary/5 px-1 py-2 ring-1 ring-primary/20 transition-colors'
            : 'min-h-10 px-1 py-2 transition-colors'
        }
      >
        {empty && (
          <p className="px-1 py-1.5 text-sm italic text-muted-foreground/70">
            Nothing in the backlog
          </p>
        )}
        {[...datedGroups.entries()].map(([date, rows]) => (
          <div
            key={date}
            id={backlogDayElementId(date)}
            className="space-y-2 pb-2"
          >
            <h3 className="px-1 text-xs font-medium tracking-wide text-muted-foreground">
              {agendaDateLabel(date, serverToday)}
            </h3>
            <SortableContext
              items={rows.filter(isAgendaRowDraggable).map((entry) => entry.id)}
              strategy={verticalListSortingStrategy}
            >
              <div className="space-y-2">{rows.map(renderRow)}</div>
            </SortableContext>
          </div>
        ))}
        {open.length > 0 && (
          <SortableContext
            items={open.filter(isAgendaRowDraggable).map((entry) => entry.id)}
            strategy={verticalListSortingStrategy}
          >
            <div className="space-y-2 pt-1">{open.map(renderRow)}</div>
          </SortableContext>
        )}
      </div>
    </section>
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
  // The two warehouses (ADR 0007, ADR 0009): the DEFAULT read's `backlog`
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
  // The row currently dragged — feeds the DragOverlay title chip.
  const [activeDrag, setActiveDrag] = useState<AgendaItemRecord | null>(null);

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

  /** Optimistic mirror of the server's `place_task_on_today` (ADR 0004
   *  amendment): a task's single row travels to civil today on a real
   *  lifecycle transition (start / complete / discard / reopen), appended at
   *  the pile's end; a row already there keeps its rank. `fn` paints the
   *  embed flip first (so `partitionAgendaItems` parks a completed row in the
   *  Completed dump). */
  const placeTaskOnToday = useCallback(
    (
      data: AgendaPages | undefined,
      itemId: string,
      fn: (item: AgendaItemRecord) => AgendaItemRecord,
    ): AgendaPages | undefined => {
      const next = updateStreamItem(data, itemId, fn);
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
   *  → `/reschedule` with the target day's rank; the Planned warehouse (its
   *  section or a Planned row) → plan / unschedule / the Planned column's
   *  Board reorder; the Backlog (its section or an OPEN row) → a backlog drop
   *  or the Open column's Board reorder. Past days are never drop targets (the
   *  API 400s), an `in_progress` occurrence may only reorder inside its own
   *  day, and an `IN_PROGRESS` / terminal task or an occurrence never enters
   *  either warehouse. */
  const handleDragEnd = (event: DragEndEvent) => {
    setActiveDrag(null);
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const activeId = String(active.id);
    const overId = String(over.id);
    const itemsNow = streamItemsOf(readStream());
    const dragged = itemsNow.find((entry) => entry.id === activeId);
    if (!dragged) return;

    const draggedIsOpen = isOpenBacklogTask(dragged, serverToday);
    const draggedIsPlanned = isPlannedBacklogTask(dragged, serverToday);

    // The on-screen sortable list holds the DRAGGABLE rows only — dnd-kit
    // resolves a same-day drop index against that, never the full pile
    // (parked and read-only rows are not in it).
    const sortableSameDay = () =>
      partitionAgendaItems(
        dayItemsOf(itemsNow, dragged.local_date),
      ).living.filter(isAgendaRowDraggable);
    // A cross-day drop over a living row carries THAT row's rank — its list
    // index would land the item in the wrong slot as soon as the pile has
    // rank gaps or parked rows (see `crossDaySortOrder`). An id outside the
    // living pile (a Completed row, a stale id) appends.
    const crossDayRank = (day: string, overItemId: string) =>
      crossDaySortOrder(
        partitionAgendaItems(dayItemsOf(itemsNow, day)).living,
        overItemId,
      );

    const overItem = itemsNow.find((entry) => entry.id === overId) ?? null;

    // Which warehouse is the drop aimed at? The section itself, or one of its
    // own rows. Resolved up front so the Planned branch sits ahead of the
    // backlog drop: a Planned row's `local_date` is the sentinel, and letting
    // it fall through to the cross-day branch would reschedule it onto 2076 —
    // which the API 400s.
    const plannedTarget =
      overId === PLANNED_DROPPABLE_ID ||
      (overItem !== null && isPlannedBacklogTask(overItem, serverToday));
    const openTarget =
      overId === BACKLOG_DROPPABLE_ID ||
      (overItem !== null && isOpenBacklogTask(overItem, serverToday));

    // 1. The Planned warehouse (ADR 0009): a same-status reorder through the
    //    Board, a plan from an OPEN row, or an unschedule from a dated row.
    //    Occurrences, a running task and terminal cards are rejected on BOTH
    //    warehouse targets.
    if (plannedTarget) {
      if (dragged.kind !== 'task' || !dragged.task) return;
      if (
        dragged.task.status === 'IN_PROGRESS' ||
        dragged.task.status === 'COMPLETED' ||
        dragged.task.status === 'DISCARDED'
      ) {
        return;
      }
      if (draggedIsPlanned) {
        const planned = warehouseRowsOf(readStream(), 'PLANNED');
        const fromIndex = planned.findIndex((entry) => entry.id === dragged.id);
        // A drop on the section itself means "the end of the Planned list",
        // exactly like a day container.
        const toIndex = overItem
          ? planned.findIndex((entry) => entry.id === overItem.id)
          : planned.length - 1;
        const target = openTaskMoveTargetAt(planned, fromIndex, toIndex);
        if (target === null) return;
        void handleWarehouseReorder(
          dragged,
          'PLANNED',
          planned,
          toIndex,
          target,
        );
        return;
      }
      if (dragged.task.status === 'OPEN') {
        void handlePlanDrop(dragged);
        return;
      }
      if (dragged.task.status === 'PLANNED') {
        // A dated (or past) PLANNED row: the warehouse, not a status change.
        void handleUnschedule(dragged);
      }
      return;
    }

    // 2. The Backlog (ADR 0007): the section itself, or an OPEN row. An
    //    already-OPEN row already sits in the stretch, so a drop on the
    //    section is a no-op; over another OPEN row it is the Board's reorder.
    if (openTarget) {
      if (dragged.kind !== 'task' || !dragged.task) return;
      if (
        dragged.task.status === 'IN_PROGRESS' ||
        dragged.task.status === 'COMPLETED' ||
        dragged.task.status === 'DISCARDED'
      ) {
        return;
      }
      if (draggedIsOpen) {
        if (!overItem) return;
        const open = warehouseRowsOf(readStream(), 'OPEN');
        const fromIndex = open.findIndex((entry) => entry.id === dragged.id);
        const toIndex = open.findIndex((entry) => entry.id === overItem.id);
        const target = openTaskMoveTargetAt(open, fromIndex, toIndex);
        if (target === null) return;
        void handleWarehouseReorder(dragged, 'OPEN', open, toIndex, target);
        return;
      }
      if (dragged.task.status === 'OPEN' || dragged.task.status === 'PLANNED') {
        void handleBacklogDrop(dragged);
      }
      return;
    }

    // 3. A drop on a day's own container means "the end of that day's pile":
    //    no rank at all, so the server appends at `max + 1`.
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
      if (dragged.task?.status === 'IN_PROGRESS') return;
      void handleCrossDayMove(dragged, day);
      return;
    }

    // 4. A drop over another row: same civil day → the agenda reorder (the
    //    Board rank only owns the two warehouse stretches, handled above); a
    //    different day → reschedule onto it.
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
    if (dragged.task?.status === 'IN_PROGRESS') return;
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
    sortOrder?: number,
  ) => {
    if (!targetDate || targetDate === item.local_date) return;
    if (!serverToday || targetDate < serverToday) return;
    // `in_progress` occurrences are reorder-only until they are paused; an
    // `IN_PROGRESS` task cannot change day at all (the API 400s — pause
    // first).
    if (item.occurrence?.status === 'in_progress') return;
    if (item.task?.status === 'IN_PROGRESS') return;
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
  };

  /** Drop a row into the Backlog (ADR 0007): `{ backlog: true }` returns a
   *  `PLANNED` task to `OPEN` (+ `unplanned` log), moves the row to today+50y
   *  and appends it to the Board's Open column. Painted optimistically with
   *  the SAME month clamp the server uses, then merged with the authoritative
   *  row (the server's real backlog date and status). A failure restores the
   *  snapshot and banners. The mutation invalidates the stream on settle —
   *  never awaited, so the rollback cannot race it. */
  const handleBacklogDrop = async (item: AgendaItemRecord) => {
    const task = item.task;
    if (!task || !serverToday) return;
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
  };

  /** Drop a row into the Planned warehouse (ADR 0009): the Board's
   *  `POST /api/tasks/:id/move { status: 'PLANNED' }` with **no rank** — the
   *  plan appends the task to the Planned column server-side and parks its
   *  agenda row on the backlog sentinel, never on civil today. (The hovered
   *  card's rank would be a same-status reorder of that column, not an
   *  append.) Painted optimistically with the SAME month clamp the server uses,
   *  then merged with the authoritative task, whose status and Board rank both
   *  land in the shared `['tasks']` cache. A failure restores the snapshot and
   *  banners; `useMoveTask` invalidates the stream on settle — never awaited,
   *  so the rollback cannot race it. */
  const handlePlanDrop = async (item: AgendaItemRecord) => {
    const task = item.task;
    if (!task || task.status !== 'OPEN' || !serverToday) return;
    const snapshot = readStream();
    patchStream((current) =>
      planOpenItem(
        current,
        item.id,
        addMonthsClamp(serverToday, 600),
        serverToday,
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
  };

  /** Unschedule a dated `PLANNED` row (ADR 0009): `{ unschedule: true }` keeps
   *  the task `PLANNED`, writes no task log and does NOT re-rank the Planned
   *  column — only the agenda row travels to the backlog sentinel, appended.
   *  Painted optimistically with `unscheduleItem`, merged with the
   *  authoritative row on success; a failure restores the snapshot and
   *  banners. Occurrences, `OPEN` rows and terminal cards never get here. */
  const handleUnschedule = async (item: AgendaItemRecord) => {
    if (!canUnschedule(item, serverToday)) return;
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
  };

  /** Reorder one warehouse row onto another row's slot (ADR 0007 for OPEN,
   *  ADR 0009 for PLANNED): the Board's own
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

  /** Success notice's Show: find the moved row (it has a stable DOM id) and
   *  scroll it into view inside the stream, then ring it briefly. The day
   *  header / dated Backlog group are fallbacks, then the warehouse section the
   *  move named — a Planned move never scrolls to the Backlog header. */
  const showDestination = (notice: ActionNotice) => {
    let target = document.getElementById(`agenda-item-${notice.itemId}`);
    if (!target && notice.date) {
      target = document.getElementById(dayElementId(notice.date));
    }
    if (!target && notice.date) {
      target = document.getElementById(backlogDayElementId(notice.date));
    }
    if (!target) {
      target = document.getElementById(
        notice.section === 'planned'
          ? PLANNED_DROPPABLE_ID
          : BACKLOG_DROPPABLE_ID,
      );
    }
    if (!target) return;
    const isRow = target.id.startsWith('agenda-item-');
    target.scrollIntoView({ block: isRow ? 'center' : 'start' });
    setFlashItemId(notice.itemId);
    setActionNotice(null);
  };

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
    onReschedule: (entry, date) => void handleCrossDayMove(entry, date),
    onMoveToBacklog: (entry) => void handleBacklogDrop(entry),
    onUnschedule: (entry) => void handleUnschedule(entry),
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
                        loadingHistory={isLoadingOlderHistory}
                        highlightedItemId={flashItemId}
                        onMoveDayToToday={(target) =>
                          void handleMoveDayToToday(target)
                        }
                      />
                    ))}

                    {/* Under the last stream day: the two warehouses (ADR
                        0007, ADR 0009) — Planned first, then the Backlog
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
