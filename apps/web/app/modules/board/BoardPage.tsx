import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  DndContext,
  DragOverlay,
  MouseSensor,
  TouchSensor,
  useSensor,
  useSensors,
} from '@dnd-kit/core';
import type {
  DragEndEvent,
  DragOverEvent,
  DragStartEvent,
} from '@dnd-kit/core';
import { Loader2, Plus, SlidersHorizontal } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
} from '@/components/ui/sheet';
import { cn } from '@/lib/utils';
import { setBoardTasksBusy } from '@/app/background-sync/board-tasks-busy';
import { TaskModal } from '@/app/components/TaskModal';
import { useNavigate, useSearch } from '@tanstack/react-router';
import { useCategoriesQuery } from '@/app/queries/categories';
import { useListsQuery } from '@/app/queries/lists';
import {
  setTasksCache,
  useCreateTask,
  useDeleteTask,
  useFocusTask,
  useMoveTask,
  useTasksQuery,
  useUnfocusTask,
  useUpdateTask,
} from '@/app/queries/tasks';
import {
  type FocusTaskResponse,
  type MoveTaskInput,
  type MoveTaskResponse,
  type NewTaskInput,
  type TaskDifficulty,
  type TaskPriority,
  type TaskRecord,
  type TaskResponse,
  type TaskStatus,
} from '@/app/types';
import { BoardColumnView } from './BoardColumn';
import { BoardColumnTabs } from './BoardColumnTabs';
import { BoardFilters } from './BoardFilters';
import { TaskCard } from './TaskCard';
import {
  boardCollisionDetection,
  MOUSE_DND_DISTANCE_PX,
  TOUCH_DND_DELAY_MS,
  TOUCH_DND_TOLERANCE_PX,
  applyDragOverPreview,
  cloneBoardItems,
  columnOf,
  dropInsertIndex,
} from './board-dnd';
import type { BoardColumnItems } from './board-dnd';
import {
  categoryMatchesSelection,
  countActiveFilterGroups,
  taskMatchesSearch,
  toggleCategoryId,
} from './board-filters';
import {
  COLUMNS,
  COLUMN_ID_PREFIX,
  TERMINAL_COLUMN_CAP,
  applyOptimisticMove,
  defaultMoveRank,
  resolveSortOrder,
} from './board-model';
import type { BoardSearch } from './board-model';
import { pickVisibleColumn } from './board-scroll';

interface TaskFormState {
  mode: 'create' | 'edit';
  /** Edit target. */
  task?: TaskRecord;
  /** Create destination. Always set when mode === 'create' on the board. */
  createStatus?: TaskStatus;
}

export function BoardPage() {
  const navigate = useNavigate();
  const { priority, difficulty, category, search } = useSearch({
    from: '/board',
  });

  // Shared queries: one `['tasks']` cache written by Board, Lists
  // and the Home task picker. Tasks and categories are seed-gated on lists
  // success — GET /api/lists performs the first-visit seed (default lists +
  // category taxonomy), so their requests must run after it (computed
  // categories depend on the seeded taxonomy). Tasks and categories are
  // independent of each other and fetch in parallel (the same seed rule as
  // ListsPage).
  const listsQuery = useListsQuery();
  const lists = listsQuery.data?.lists ?? [];
  const categoriesQuery = useCategoriesQuery({ enabled: listsQuery.isSuccess });
  const categories = categoriesQuery.data?.categories ?? [];

  // Drag state (ADR 0002 § DnD). Declared BEFORE `useTasksQuery`: `busy`
  // below feeds the tasks query's refresh options, and hooks must run in a
  // stable order.
  const [activeDrag, setActiveDrag] = useState<TaskRecord | null>(null);
  // Live cross-column preview: per-column id arrays cloned from the
  // displayed columns on lift and discarded on end/cancel. `tasks` stays
  // committed until the drop — only this throwaway map moves ids between
  // columns mid-drag so the dest column can part around the mover.
  const [dragItems, setDragItems] = useState<BoardColumnItems | null>(null);
  // Latest preview for `handleDragEnd` (same "latest value during render"
  // pattern as tasksRef): the drop resolves against the columns as shown,
  // before the handler clears them.
  const dragItemsRef = useRef<BoardColumnItems | null>(null);
  dragItemsRef.current = dragItems;
  // Cards with a /move request in flight — dragging them again is ignored
  // until the response lands (no queuing).
  const [movingIds, setMovingIds] = useState<Set<string>>(new Set());
  // A focus request (POST /api/tasks/:id/focus or DELETE /api/focus) is in
  // flight: further pin taps are ignored and every pin is disabled.
  const [focusInFlight, setFocusInFlight] = useState(false);

  // A live drag or preview, an in-flight /move, an in-flight focus toggle:
  // while `busy` no tasks refetch may land — a window-focus or interval
  // refetch would overwrite the optimistic cache with the pre-move server
  // list. Busy is published to background sync, which pauses its always-on
  // tasks poll; the Board observer keeps `refetchOnWindowFocus: !busy`, and
  // every task write also cancels any in-flight refetch via its mutation's
  // `onMutate`.
  const busy =
    activeDrag !== null ||
    dragItems !== null ||
    movingIds.size > 0 ||
    focusInFlight;

  // Publish the busy flag for background sync's tasks observer. Clear it on
  // unmount so leaving the board does not leave the poll paused.
  useEffect(() => {
    setBoardTasksBusy(busy);
    return () => setBoardTasksBusy(false);
  }, [busy]);

  const tasksQuery = useTasksQuery({
    enabled: listsQuery.isSuccess,
    refetchOnWindowFocus: !busy,
  });
  const tasks = tasksQuery.data?.tasks ?? [];
  const setTasks = setTasksCache;
  // Same pattern as before for `tasks`: the async move flow reads the
  // pre-drop snapshot and looks up the dropped card from the latest render,
  // never a stale one. Only the storage behind `setTasks` changed (useState
  // → shared cache); the ref stays.
  const tasksRef = useRef<TaskRecord[]>([]);
  tasksRef.current = tasks;
  // Full-page loader only while there is no data yet (first load, or a retry
  // after a hard error) — Query's `isLoading` already means "no data yet",
  // so a quiet refresh with cards on screen never flashes the spinner.
  const isLoading =
    listsQuery.isLoading ||
    (listsQuery.isSuccess &&
      (tasksQuery.isLoading || categoriesQuery.isLoading));
  // Load failures: the first Error.message among the three queries, else
  // null. Replaces the board with the error+retry banner when there are no
  // lists to show.
  const firstErrorMessage = (err: unknown, fallback: string): string | null =>
    err instanceof Error ? err.message : err ? fallback : null;
  const loadError =
    firstErrorMessage(listsQuery.error, 'Failed to load board') ??
    firstErrorMessage(tasksQuery.error, 'Failed to load tasks') ??
    firstErrorMessage(categoriesQuery.error, 'Failed to load categories');
  // Action failures (move 409, etc.): rendered as a banner above the
  // still-visible board — cards are never unmounted by an action error.
  const [actionError, setActionError] = useState<string | null>(null);

  // The board (mobile tab row + scroller) is hidden only when there is
  // nothing to show: first load in flight, or a load error that replaced
  // the board. One boolean, so the scroller, the tabs and the observer can
  // never disagree about whether the board is mounted — the observer effect
  // depends on it to re-run once the scroller ref exists.
  const boardShown = lists.length > 0 || (!isLoading && !loadError);

  // ──────────────────────────────────────────
  // Mobile column tabs (below md)
  // ──────────────────────────────────────────

  // Column currently in view. Written by the IntersectionObserver below as
  // the user swipes, and immediately on a tab tap so the highlight never
  // waits for the smooth scroll to land.
  const [activeColumn, setActiveColumn] = useState<TaskStatus>(
    COLUMNS[0].status,
  );
  // The horizontal column scroller — also the IntersectionObserver root:
  // the columns are clipped by THIS box, not by the viewport, so viewport
  // roots would measure columns hidden behind the page's own clip.
  const scrollerRef = useRef<HTMLDivElement>(null);
  // Cumulative intersection ratio per column. IntersectionObserver only
  // reports entries that crossed a threshold, so the max of one batch can
  // be a 0.15 peek while the truly-visible column (0.9) simply did not
  // change this frame. Merge every batch here and let the helper pick.
  const ratiosRef = useRef<Record<string, number>>({});
  // A tab tap's smooth scroll is in flight: ignore observer updates so the
  // highlight stays on the tapped column instead of flickering through the
  // columns it pans past.
  const programmaticScroll = useRef(false);

  useEffect(() => {
    const root = scrollerRef.current;
    if (!boardShown || !root) return;
    const targets = [
      ...root.querySelectorAll<HTMLElement>('[data-board-column]'),
    ];
    if (targets.length === 0) return;
    ratiosRef.current = {};
    const observer = new IntersectionObserver(
      (entries) => {
        const next = { ...ratiosRef.current };
        for (const entry of entries) {
          const id = (entry.target as HTMLElement).dataset.boardColumn;
          if (id) next[id] = entry.intersectionRatio;
        }
        ratiosRef.current = next;
        // Ignore updates during a programmatic tab scroll (see below).
        if (programmaticScroll.current) return;
        const picked = pickVisibleColumn(
          next,
          COLUMNS.map((column) => column.status),
        );
        if (picked) setActiveColumn(picked as TaskStatus);
      },
      // Five thresholds: below md a snapped column covers most of the
      // scroller and the next only peeks, so the winner stays stable
      // instead of flipping on every pixel of a drag.
      { root, threshold: [0, 0.25, 0.5, 0.75, 1] },
    );
    for (const target of targets) observer.observe(target);
    return () => observer.disconnect();
  }, [boardShown]);

  /** Tab tap → pan the scroller to that column. The scroll itself is what
   *  moves the board: the tabs never reorder or re-mount columns. */
  const scrollToColumn = (status: TaskStatus) => {
    const root = scrollerRef.current;
    const target = root?.querySelector<HTMLElement>(
      `[data-board-column="${status}"]`,
    );
    if (!root || !target) return;
    setActiveColumn(status);
    programmaticScroll.current = true;
    // `inline: 'start'` honors the scroller's scroll-padding (scroll-px-4),
    // so the column lands on the same inset snap-start uses. `block:
    // 'nearest'` so no ancestor scrolls vertically.
    target.scrollIntoView({
      behavior: 'smooth',
      inline: 'start',
      block: 'nearest',
    });
    const release = () => {
      programmaticScroll.current = false;
    };
    root.addEventListener('scrollend', release, { once: true });
    // scrollend is missing on older WebKit; don't leave the guard stuck.
    window.setTimeout(() => {
      root.removeEventListener('scrollend', release);
      release();
    }, 600);
  };

  // Task dialog state.
  const [taskForm, setTaskForm] = useState<TaskFormState | null>(null);

  // Mobile filters sheet (below md only). The inline filter row stays mounted
  // behind `hidden md:block`, so crossing to md+ with the sheet open would
  // leave two filter UIs on screen — close on the media query itself
  // (rotation, window resize, desktop DevTools) rather than on a render.
  const [filtersOpen, setFiltersOpen] = useState(false);
  // Portal host for the sheet's category panel (see CategoryFilter's
  // `portalContainer`). A callback ref, so it is populated the commit the
  // sheet mounts; `null` means "not mounted yet" and the panel renders
  // nothing rather than falling back to document.body.
  const [portalEl, setPortalEl] = useState<HTMLDivElement | null>(null);

  useEffect(() => {
    const mq = window.matchMedia('(min-width: 768px)');
    const closeIfDesktop = () => {
      if (mq.matches) setFiltersOpen(false);
    };
    mq.addEventListener('change', closeIfDesktop);
    return () => mq.removeEventListener('change', closeIfDesktop);
  }, []);

  // Mouse: 8px so a click still opens the modal (ADR 0002 § DnD).
  // Touch: PointerSensor loses to the board's overflow-x pan (and Chrome
  // DevTools device mode speaks touch events, not pointer). Hold ~250ms
  // to lift a card; a swipe still scrolls.
  const sensors = useSensors(
    useSensor(MouseSensor, {
      activationConstraint: { distance: MOUSE_DND_DISTANCE_PX },
    }),
    useSensor(TouchSensor, {
      activationConstraint: {
        delay: TOUCH_DND_DELAY_MS,
        tolerance: TOUCH_DND_TOLERANCE_PX,
      },
    }),
  );

  // Task write mutations: thin wrappers over the API that cancel
  // any in-flight `['tasks']` refetch on mutate. The optimistic paint and
  // the snapshot rollback stay here in the page — the hooks never write the
  // cache themselves and never invalidate on success (invalidation would
  // fight applyOptimisticMove's sibling-rank contract).
  const moveTaskMutation = useMoveTask();
  const focusTaskMutation = useFocusTask();
  const unfocusTaskMutation = useUnfocusTask();
  const createTaskMutation = useCreateTask();
  const updateTaskMutation = useUpdateTask();
  const deleteTaskMutation = useDeleteTask();

  // ──────────────────────────────────────────
  // URL filters (ADR 0002 § Filters)
  // ──────────────────────────────────────────

  // Resolves the URL's explicit category ids. Untracked is a system category
  // that is never offered as a filter chip (that derivation now lives in
  // `BoardFilters`), but its id stays resolvable here: untracked cards are
  // still visible in their columns, and a `?category=` URL naming the sink
  // still matches. Unknown ids are ignored (ADR 0002) — they cannot match any
  // task's computed category.
  const categoryById = useMemo(
    () => new Map(categories.map((cat) => [cat.id, cat])),
    [categories],
  );

  // Resolved explicit ids: the category control and `matchesFilters` below
  // read the same list.
  const selectedCategoryIds = useMemo(() => {
    if (!category) return [];
    return category
      .split(',')
      .map((id) => id.trim())
      .filter((id) => id.length > 0 && categoryById.has(id));
  }, [category, categoryById]);

  // Badge count for the mobile Filters button — the page's only read of the
  // helper. `BoardFilters` derives its own "are filters active?" boolean for
  // the Clear filters affordance from these same four groups
  // (`countActiveFilterGroups`), so the badge and the affordance share one
  // source of truth and can never disagree (ADR 0002 § UI). Category counts
  // once however many ids are selected; a whitespace-only search is not a
  // filter.
  const activeFilterCount = countActiveFilterGroups({
    priority,
    difficulty,
    categoryIds: selectedCategoryIds,
    search,
  });

  // Merges the given fields into the URL search params. Only keys that are
  // PRESENT in `updates` are touched: an explicitly `undefined` value removes
  // the param (the "All" toggles and clearFilters), while omitted keys (e.g.
  // `{ priority: 'high' }` from setPriorityFilter) leave the sibling params
  // intact — otherwise priority and category could never be combined.
  // `replace` keeps filter toggling out of the history stack.
  const updateSearch = useCallback(
    (updates: Partial<BoardSearch>) => {
      navigate({
        to: '/board',
        replace: true,
        search: (prev) => {
          const next: BoardSearch = { ...prev };
          if ('priority' in updates) {
            if (updates.priority === undefined) {
              delete next.priority;
            } else {
              next.priority = updates.priority;
            }
          }
          if ('difficulty' in updates) {
            if (updates.difficulty === undefined) {
              delete next.difficulty;
            } else {
              next.difficulty = updates.difficulty;
            }
          }
          if ('category' in updates) {
            if (updates.category === undefined) {
              delete next.category;
            } else {
              next.category = updates.category;
            }
          }
          if ('search' in updates) {
            // Whitespace-only is a no-op: drop the param entirely.
            if (
              updates.search === undefined ||
              updates.search.trim().length === 0
            ) {
              delete next.search;
            } else {
              next.search = updates.search;
            }
          }
          return next;
        },
      });
    },
    [navigate],
  );

  const setPriorityFilter = (value: TaskPriority | undefined) =>
    updateSearch({ priority: value });

  const setDifficultyFilter = (value: TaskDifficulty | undefined) =>
    updateSearch({ difficulty: value });

  // Explicit ids only — a click on a row implied by a selected parent is a
  // no-op (`toggleCategoryId`), so implied ids never leak into the URL and
  // the URL never rewrites an implied child out (ADR 0002 § Filters).
  const toggleCategoryFilter = (id: string) => {
    const next = toggleCategoryId(selectedCategoryIds, id, categories);
    updateSearch({
      category: next.length > 0 ? next.join(',') : undefined,
    });
  };

  const clearFilters = () =>
    updateSearch({
      priority: undefined,
      difficulty: undefined,
      category: undefined,
      search: undefined,
    });

  // ──────────────────────────────────────────
  // Columns: filter → sort → cap (display + drag targets)
  // ──────────────────────────────────────────

  const matchesFilters = useCallback(
    (task: TaskRecord) => {
      if (priority !== undefined && task.priority !== priority) return false;
      if (difficulty !== undefined && task.difficulty !== difficulty) {
        return false;
      }
      // Category matches against the EXPANDED selection: each explicit id
      // plus the living children of any explicit root (ADR 0002 § Filters,
      // amended 2026-08-20). Unknown ids were already dropped when parsing
      // `selectedCategoryIds`; expanding a known parent adds its children
      // even if they were never in the URL.
      if (
        !categoryMatchesSelection(
          task.category.id,
          selectedCategoryIds,
          categories,
        )
      ) {
        return false;
      }
      // Free-text filter over stored title/description (never display_title);
      // an empty/whitespace `search` matches everything.
      if (!taskMatchesSearch(task, search ?? '')) return false;
      return true;
    },
    [priority, difficulty, selectedCategoryIds, categories, search],
  );

  // Filter the column's tasks (AND), sort by `sort_order ASC` (tie-break
  // `created_at`), then cap Done/Discarded at 20 matches — untracked
  // cards count like any other. This is the DISPLAYED list: drops,
  // reorders and the cap are all relative to it (ADR 0002 § Filters).
  const tasksForColumn = useCallback(
    (status: TaskStatus): TaskRecord[] => {
      const matches = tasks.filter(
        (task) => task.status === status && matchesFilters(task),
      );
      matches.sort(
        (a, b) =>
          a.sort_order - b.sort_order ||
          a.created_at.localeCompare(b.created_at),
      );
      if (status === 'COMPLETED' || status === 'DISCARDED') {
        return matches.slice(0, TERMINAL_COLUMN_CAP);
      }
      return matches;
    },
    [tasks, matchesFilters],
  );

  // Task ids per column for the SortableContexts — always in sync with what
  // is rendered, so dnd-kit never sorts a card it cannot measure.
  const itemsByColumn = useMemo((): Record<TaskStatus, string[]> => {
    const items = {
      OPEN: [] as string[],
      PLANNED: [] as string[],
      IN_PROGRESS: [] as string[],
      COMPLETED: [] as string[],
      DISCARDED: [] as string[],
    };
    for (const column of COLUMNS) {
      items[column.status] = tasksForColumn(column.status).map(
        (task) => task.id,
      );
    }
    return items;
  }, [tasksForColumn]);

  // During a drag the columns render the PREVIEW ids. Running those back
  // through `tasksForColumn` would re-filter/re-cap and could drop the
  // ghost, so they map straight through the loaded tasks instead (missing
  // = deleted mid-drag — skipped, never rendered as undefined).
  const displayItems = dragItems ?? itemsByColumn;
  const taskById = useMemo(
    () => new Map(tasks.map((task) => [task.id, task])),
    [tasks],
  );
  const displayTasks = useCallback(
    (status: TaskStatus): TaskRecord[] =>
      displayItems[status]
        .map((id) => taskById.get(id))
        .filter((task) => task !== undefined),
    [displayItems, taskById],
  );

  // ──────────────────────────────────────────
  // Drag + optimistic move (ADR 0002 § DnD, § UI)
  // ──────────────────────────────────────────

  const handleDragStart = (event: DragStartEvent) => {
    const task = tasksRef.current.find((entry) => entry.id === event.active.id);
    // Unknown (e.g. deleted while dragging) — show nothing in the overlay.
    setActiveDrag(task ?? null);
    // Snapshot the displayed (filtered + capped) columns: the drag mutates
    // only this preview, never the committed `tasks`.
    setDragItems(cloneBoardItems(itemsByColumn));
  };

  /** Live preview while hovering (`onDragOver`, ADR 0002 § DnD): moves the
   *  REAL dragged id between the preview's column arrays so the dest
   *  SortableContext mounts a dimmed ghost and its cards part. Nothing is
   *  persisted and `applyOptimisticMove` stays drop-only — a cancel or
   *  drop-outside just discards the map. */
  const handleDragOver = (event: DragOverEvent) => {
    const { active, over } = event;
    if (!over || typeof active.id !== 'string') return;
    setDragItems((prev) =>
      applyDragOverPreview(
        prev ?? itemsByColumn,
        String(active.id),
        String(over.id),
      ),
    );
  };

  /** One negotiated `/move` request. Applies the OPTIMISTIC change first
   *  (caller has already snapshotted and decided what to send), then:
   *  - 200 → merge `response.task`;
   *  - failure → restore the full snapshot and raise the error banner.
   *  Returns the error message (null on success) so the task modal can show
   *  it on the form too. */
  const sendMoveRequest = async (
    taskId: string,
    body: MoveTaskInput,
    snapshot: TaskRecord[],
    onSuccess: (data: MoveTaskResponse) => void,
  ): Promise<string | null> => {
    setActionError(null);
    setMovingIds((prev) => new Set(prev).add(taskId));
    try {
      const data = await moveTaskMutation.mutateAsync({
        id: taskId,
        input: body,
      });
      onSuccess(data);
      return null;
    } catch (err) {
      setTasks(snapshot);
      const message = err instanceof Error ? err.message : 'Move failed';
      setActionError(message);
      return message;
    } finally {
      setMovingIds((prev) => {
        const next = new Set(prev);
        next.delete(taskId);
        return next;
      });
    }
  };

  /** Normal drop path (no conflict): optimistically move the card, then
   *  persist. `applyOptimisticMove` shifts the dest column's sibling ranks
   *  exactly like the server (reorder_in_place / place_at), so the board
   *  paints the real slot immediately — the success merge only replaces the
   *  mover (`response.task`). */
  const performMove = async (
    taskId: string,
    destStatus: TaskStatus,
    destSortOrder: number,
  ) => {
    const snapshot = tasksRef.current;
    setTasks((prev) =>
      applyOptimisticMove(prev, taskId, destStatus, destSortOrder),
    );
    await sendMoveRequest(
      taskId,
      { status: destStatus, sort_order: destSortOrder },
      snapshot,
      (data) => {
        setTasks((prev) =>
          prev.map((entry) => (entry.id === data.task.id ? data.task : entry)),
        );
      },
    );
  };

  /** Resolves a drop against the displayed (filtered + capped) lists and
   *  starts the optimistic move, or does nothing (dropped back onto its own
   *  slot). A drop onto In Progress is a plain `/move` — it starts the card
   *  even while other tasks already run (IN_PROGRESS is a column, not a
   *  singleton lock). */
  const handleDragEnd = (event: DragEndEvent) => {
    const { active, over } = event;
    // Capture the preview BEFORE clearing it: the drop resolves against the
    // columns as the pointer saw them during the drag.
    const preview = dragItemsRef.current;
    setActiveDrag(null);
    // The preview is throwaway — clearing restores the committed columns
    // even when the /move below fails and reverts `tasks`.
    setDragItems(null);
    if (!over || typeof active.id !== 'string') return;
    const activeId = active.id;

    const activeTask = tasksRef.current.find((task) => task.id === activeId);
    if (!activeTask) return;

    // `over` is either a card (task id) or a column (`column:<status>`).
    // Dest IDENTITY prefers the preview column (where the pointer last saw
    // things); every index math stays on committed `tasksForColumn` — the
    // preview decides where the card goes, never what gets sent.
    const overId = over.id;
    let destStatus: TaskStatus;
    let destIndex: number; // view index in the dest column's displayed list
    // Only the self-over branch yields a PREVIEW index; it is already shaped
    // for `remaining` and must skip destIndexInRemaining (dropInsertIndex).
    let overIsSelf = false;
    if (typeof overId === 'string' && overId.startsWith(COLUMN_ID_PREFIX)) {
      destStatus = overId.slice(COLUMN_ID_PREFIX.length) as TaskStatus;
      // Column-body drop means append. The committed length (mover counted)
      // is what the hoist expects: in-column it becomes remaining.length,
      // cross-column it already is. The preview index would read one short.
      destIndex = tasksForColumn(destStatus).length; // end (0 when empty)
    } else if (typeof overId === 'string' && overId === activeId) {
      // Dropped onto the mover's own ghost: its committed status is still
      // the SOURCE column, so resolve through the preview — trusting
      // `overTask.status` here would collapse a cross-column hover into a
      // same-slot no-op.
      overIsSelf = true;
      destStatus =
        (preview !== null ? columnOf(preview, activeId) : null) ??
        activeTask.status;
      const previewIdx = preview?.[destStatus]?.indexOf(activeId) ?? -1;
      destIndex =
        previewIdx >= 0
          ? previewIdx
          : tasksForColumn(destStatus).findIndex(
              (task) => task.id === activeId,
            );
      if (destIndex === -1) return; // unresolvable slot (filtered/capped out)
    } else if (typeof overId === 'string') {
      const overTask = tasksRef.current.find((task) => task.id === overId);
      if (!overTask) return;
      destStatus =
        (preview !== null ? columnOf(preview, overId) : null) ??
        overTask.status;
      const destShown = tasksForColumn(destStatus);
      destIndex = destShown.findIndex((task) => task.id === overId);
      if (destIndex === -1) return; // not displayed (filtered out) — ignore
    } else {
      return;
    }

    const destShown = tasksForColumn(destStatus);
    const remaining = destShown.filter((task) => task.id !== activeId);
    // The mover's shown index, hoisted so the same-slot no-op can compare
    // against the unadjusted destIndex and the rank lookup can skip the
    // mover (dropInsertIndex) — a card/column destIndex counts the mover,
    // `remaining` does not, so the raw index would read the NEXT card when
    // moving down. Self-over destIndex skips the hoist (already
    // remaining-shaped).
    const oldIndex = destShown.findIndex((task) => task.id === activeId);

    // Same column + same slot = no movement: never call the API.
    if (destStatus === activeTask.status) {
      const slot = destIndex >= remaining.length ? remaining.length : destIndex;
      if (oldIndex !== -1 && slot === oldIndex) return;
    }

    const insertIndex = dropInsertIndex(destIndex, oldIndex, overIsSelf);
    const destSortOrder = resolveSortOrder(remaining, insertIndex, destStatus);

    void performMove(activeId, destStatus, destSortOrder);
  };

  /** TaskModal's immediate status change (ADR 0002 § UI): a drop with no
   *  drop position — `sort_order` is OMITTED and the server applies the
   *  column default (`defaultMoveRank` mirrors it for the optimistic frame:
   *  OPEN appends, pause prepends Planned, complete/discard prepend). A
   *  status pill to IN_PROGRESS is a plain `/move` — it starts the card even
   *  while other tasks run. Optimistic, then ONE `/move`;
   *  `sendMoveRequest` already owns the revert and the banner. Returns the
   *  error message for the form (null on success). The modal stays open;
   *  `task` in props updates via this merge. */
  const handleMoveTask = async (
    taskId: string,
    status: TaskStatus,
  ): Promise<string | null> => {
    const snapshot = tasksRef.current;
    const current = snapshot.find((entry) => entry.id === taskId);
    const destRank = defaultMoveRank(
      current?.status ?? status,
      status,
      snapshot,
      taskId,
    );
    setTasks((prev) => applyOptimisticMove(prev, taskId, status, destRank));
    return sendMoveRequest(taskId, { status }, snapshot, (data) => {
      setTasks((prev) =>
        prev.map((entry) => (entry.id === data.task.id ? data.task : entry)),
      );
      // Keep the modal's `task` prop in sync so the selected pill follows
      // the server — the modal stays open after a status change.
      setTaskForm((prev) =>
        prev && prev.mode === 'edit' && prev.task?.id === data.task.id
          ? { ...prev, task: data.task }
          : prev,
      );
    });
  };

  // ──────────────────────────────────────────
  // Focus toggle
  // ──────────────────────────────────────────

  /** Merges the authoritative `{ task, previous }` rows from a focus response
   *  into the board by id, and keeps the edit modal's `task` prop in sync when
   *  it is one of the two — the modal stays open after a focus toggle, so the
   *  Focus pill follows the server like the status pills follow a /move. */
  const mergeFocusRows = (data: FocusTaskResponse): void => {
    const focusedRow = data.task;
    const previousRow = data.previous;
    setTasks((prev) =>
      prev.map((entry) => {
        if (focusedRow && entry.id === focusedRow.id) return focusedRow;
        if (previousRow && entry.id === previousRow.id) return previousRow;
        return entry;
      }),
    );
    setTaskForm((prev) => {
      if (!prev || prev.mode !== 'edit' || !prev.task) return prev;
      if (focusedRow && focusedRow.id === prev.task.id) {
        return { ...prev, task: focusedRow };
      }
      if (previousRow && previousRow.id === prev.task.id) {
        return { ...prev, task: previousRow };
      }
      return prev;
    });
  };

  /** One negotiated focus request. The caller has already applied the
   *  OPTIMISTIC paint; here we wait for the server:
   *  - 200 → merge the authoritative `{ task, previous }` rows (onSuccess);
   *  - failure → restore the full snapshot and raise the action banner.
   *  Returns the error message (null on success) so the modal Focus pill can
   *  show it on the form too. Mirrors sendMoveRequest. */
  const sendFocusRequest = async (
    isFocus: boolean,
    taskId: string,
    snapshot: TaskRecord[],
    onSuccess: (data: FocusTaskResponse) => void,
  ): Promise<string | null> => {
    setFocusInFlight(true);
    setActionError(null);
    try {
      onSuccess(
        isFocus
          ? await focusTaskMutation.mutateAsync(taskId)
          : await unfocusTaskMutation.mutateAsync(),
      );
      return null;
    } catch (err) {
      setTasks(snapshot);
      const message =
        err instanceof Error ? err.message : 'Focus change failed';
      setActionError(message);
      return message;
    } finally {
      setFocusInFlight(false);
    }
  };

  /** Optimistic focus toggle, used by both the card pin and the modal Focus
   *  control. Focus is IN_PROGRESS-only — the pin renders only on IP cards and
   *  the modal shows the control only for IP tasks, so every other status is
   *  unreachable here (and would 400). While a request is in flight further
   *  taps are ignored (focusInFlight guard) and every pin is disabled
   *  (focusDisabled). POST focuses the target and clears every other card;
   *  DELETE unfocuses (all clear). On 502/4xx the pre-toggle snapshot is
   *  restored and the actionError banner raised. Returns the error message
   *  (null on success). */
  const handleToggleFocus = async (
    task: TaskRecord,
  ): Promise<string | null> => {
    if (focusInFlight) return null;
    const snapshot = tasksRef.current;
    const targetId = task.id;
    if (task.focused) {
      setTasks((prev) => prev.map((entry) => ({ ...entry, focused: false })));
      return sendFocusRequest(false, targetId, snapshot, mergeFocusRows);
    }
    setTasks((prev) =>
      prev.map((entry) => ({
        ...entry,
        focused: entry.id === targetId,
      })),
    );
    return sendFocusRequest(true, targetId, snapshot, mergeFocusRows);
  };

  // ──────────────────────────────────────────
  // Task actions (New Task + click-to-edit)
  // ──────────────────────────────────────────

  /** Opens the create dialog with the status pills locked to `status` —
   *  the column whose + was tapped, or OPEN for the header New Task button
   *  (which is the Backlog shortcut). */
  const openCreateTask = (status: TaskStatus = 'OPEN') => {
    setTaskForm({ mode: 'create', createStatus: status });
  };

  const openEditTask = (task: TaskRecord) => {
    setTaskForm({ mode: 'edit', task });
  };

  const closeTaskForm = () => {
    setTaskForm(null);
  };

  // Persists the task. Returns an error message to show on the form (the
  // server explains 400s like "title does not match a category"), or null
  // when the modal may close. Create runs as create-then-move: POST /api/
  // tasks always stamps OPEN (status is never in the body), and a
  // non-Backlog destination is reached by an immediate follow-up /move that
  // OMITS sort_order — the server applies the column default, the same
  // matrix the board drop uses. A create into In Progress is a plain
  // create-then-move that starts the card even while other tasks run (a
  // column, not a singleton lock). The move failure path closes the modal,
  // raises the banner and leaves the orphan card OPEN in Backlog at the
  // server-assigned append rank (the server-owned create is never deleted);
  // any failure full-restores the pre-move snapshot.
  const handleTaskSubmit = async (values: {
    title: string;
    description: string;
    durationMinutes: number;
    priority: TaskPriority;
    difficulty: TaskDifficulty;
  }): Promise<string | null> => {
    if (!taskForm) return null;
    setActionError(null);
    // `NewTaskInput` on purpose: create sends the full set, and the edit
    // PATCH accepts it too (every field optional server-side).
    const body: NewTaskInput = {
      title: values.title,
      description: values.description,
      duration_minutes: values.durationMinutes,
      priority: values.priority,
      difficulty: values.difficulty,
    };

    // ── Edit (PATCH) — unchanged: merge the returned task, close.
    if (taskForm.mode === 'edit') {
      try {
        const data = await updateTaskMutation.mutateAsync({
          id: taskForm.task!.id,
          input: body,
        });
        setTasks((prev) =>
          prev.map((entry) => (entry.id === data.task.id ? data.task : entry)),
        );
        closeTaskForm();
        return null;
      } catch (err) {
        return err instanceof Error ? err.message : 'Save failed';
      }
    }

    // ── Create (POST always stamps OPEN on the server — `createStatus` only
    //    decides whether a follow-up /move is needed, it never goes in the
    //    request body).
    let data: TaskResponse;
    try {
      data = await createTaskMutation.mutateAsync(body);
    } catch (err) {
      // Create 400 etc.: return the error, the modal stays open on the form.
      return err instanceof Error ? err.message : 'Create failed';
    }
    // The response carries the computed category — reuse it directly so the
    // card lands in the right column instantly. The server never returns an
    // untracked result on create (create stays strict).
    const dest = taskForm.createStatus ?? 'OPEN';

    // Destination is the create status itself: done, no useless same-status
    // /move reorder. The server already returned the append rank (create
    // appends Backlog); the column sorts by `sort_order`, so the card lands
    // at the back automatically.
    if (dest === 'OPEN') {
      setTasks((prev) => [data.task, ...prev]);
      closeTaskForm();
      return null;
    }

    // Otherwise create-then-move: optimistically place the card at the
    // server's column default for a create (OPEN → dest; `defaultMoveRank`
    // mirrors it — do NOT hardcode 0, a Planned-append dest would flash the
    // card at the head), then persist with ONE /move that OMITS sort_order.
    const snapshotBeforeOptimistic = tasksRef.current;
    const destRank = defaultMoveRank(
      'OPEN',
      dest,
      snapshotBeforeOptimistic,
      data.task.id,
    );
    const moveBody: MoveTaskInput = { status: dest };
    setTasks((prev) => [
      { ...data.task, status: dest, sort_order: destRank },
      ...prev,
    ]);

    let moveData: MoveTaskResponse;
    try {
      moveData = await moveTaskMutation.mutateAsync({
        id: data.task.id,
        input: moveBody,
      });
    } catch (err) {
      // Failure — network or HTTP (401 missing Google token, 400, …):
      // restore the pre-move snapshot, then ensure the created card is
      // present as OPEN/Backlog at the server-assigned append rank (the
      // orphan stays — the server owns the create, nothing to undo).
      // Banner, close (null closes the modal).
      setTasks(() => [data.task, ...snapshotBeforeOptimistic]);
      const message = err instanceof Error ? err.message : 'Move failed';
      setActionError(message);
      closeTaskForm();
      return null;
    }
    // Move success: merge the authoritative row.
    setTasks((prev) =>
      prev.map((entry) =>
        entry.id === moveData.task.id ? moveData.task : entry,
      ),
    );
    closeTaskForm();
    return null;
  };

  const handleTaskDelete = async (taskId: string): Promise<string | null> => {
    setActionError(null);
    try {
      await deleteTaskMutation.mutateAsync(taskId);
    } catch (err) {
      return err instanceof Error ? err.message : 'Delete failed';
    }
    setTasks((prev) => prev.filter((entry) => entry.id !== taskId));
    closeTaskForm();
    return null;
  };

  // Viewport-locked page (same height idiom as CalendarPage): the root is a
  // fixed 100dvh flex column that never grows with column content, so the
  // header and filters hold their place and only a column body scrolls.
  return (
    <div className="flex h-[100dvh] flex-col overflow-hidden bg-cream">
      {/* Header, banners, loader and filters — fixed height (shrink-0), so
          everything below them is the leftover viewport. */}
      <div className="mx-auto w-full max-w-7xl shrink-0 px-6 pt-4 md:pt-8">
        {/* Header. Below md it is ONE compact row — title + icon buttons —
            and the filters live in the sheet; from md up it is the original
            row (subtitle, full labels, gap-4, mb-8). */}
        <header className="mb-4 flex flex-row items-center justify-between gap-3 md:mb-8 md:gap-4">
          <div>
            <h1 className="font-heading text-2xl font-bold text-foreground md:mb-2 md:text-3xl">
              Board
            </h1>
            <p className="hidden text-muted-foreground md:block">
              Backlog, Planned, In Progress, Done, Discarded
            </p>
          </div>
          {/* Mobile shortcuts first, desktop pair after; `hidden md:inline-flex`
              overrides Button's base `inline-flex` through tailwind-merge. */}
          <div className="flex shrink-0 gap-2 md:gap-3">
            <Button
              variant="outline"
              size="icon"
              className="md:hidden"
              aria-label="New task"
              onClick={() => openCreateTask('OPEN')}
            >
              <Plus className="h-4 w-4" />
            </Button>
            <Button
              variant="outline"
              size="icon"
              className="relative md:hidden"
              aria-haspopup="dialog"
              aria-expanded={filtersOpen}
              aria-label={
                activeFilterCount === 0
                  ? 'Filters'
                  : `Filters, ${activeFilterCount} active`
              }
              onClick={() => setFiltersOpen(true)}
            >
              <SlidersHorizontal className="h-4 w-4" />
              {/* Group count, not a chip: the `Badge` component is far too
                  large for a 40px icon button. aria-hidden — the count is
                  already in the button's label. */}
              {activeFilterCount > 0 && (
                <span
                  aria-hidden
                  className="absolute -right-1 -top-1 flex h-4 min-w-4 items-center justify-center rounded-full bg-primary px-1 text-[10px] font-medium leading-none text-primary-foreground"
                >
                  {activeFilterCount}
                </span>
              )}
            </Button>
            <Button
              variant="outline"
              className="hidden md:inline-flex"
              onClick={() => openCreateTask('OPEN')}
            >
              <Plus className="h-4 w-4 mr-2" />
              New Task
            </Button>
            <Button
              variant="outline"
              className="hidden md:inline-flex"
              onClick={() => navigate({ to: '/categories' })}
            >
              Edit Categories
            </Button>
          </div>
        </header>

        {/* Load error banner — replaces the board only when there are no
            lists to show; with cards on screen it reads as a refresh-failure
            notice above the still-visible board */}
        {loadError && (
          <div className="mb-6 flex items-center justify-between gap-4 bg-destructive/10 text-destructive rounded-xl px-4 py-3">
            <p className="selectable text-sm">{loadError}</p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                // Refetch lists first — tasks/categories are seed-gated on
                // lists success and re-run automatically; when lists are
                // already loaded, refetch tasks and categories too.
                void listsQuery.refetch();
                if (listsQuery.isSuccess) {
                  void tasksQuery.refetch();
                  void categoriesQuery.refetch();
                }
              }}
            >
              Retry
            </Button>
          </div>
        )}

        {/* Action error banner (e.g. a failed move) — the board stays mounted */}
        {actionError && (
          <div className="mb-6 flex items-center justify-between gap-4 bg-destructive/10 text-destructive rounded-xl px-4 py-3">
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

        {/* Loading — only while the board is empty (first load or a retry
            after a hard error); reloads with cards on screen never flash
            this */}
        {isLoading && lists.length === 0 && tasks.length === 0 && (
          <div className="flex items-center justify-center py-24 gap-2 text-muted-foreground">
            <Loader2 className="h-5 w-5 animate-spin" />
            Loading board…
          </div>
        )}

        {/* Board — hidden only when there is nothing to show (first load in
            flight, or a load error that replaced the board). Action errors
            never unmount it. */}
        {boardShown && (
          <section className="mb-6 hidden md:block">
            {/* Filter chrome lives in the shared `BoardFilters` (the mobile
                sheet renders the same component); every URL write stays in
                this page and is handed down as a callback (ADR 0002 § UI).
                Left MOUNTED (hidden, not unmounted) so the two copies share
                one state — which is why the sheet passes its own
                `searchInputId` and no portal container here. */}
            <BoardFilters
              layout="row"
              priority={priority}
              difficulty={difficulty}
              search={search}
              lists={lists}
              categories={categories}
              selectedCategoryIds={selectedCategoryIds}
              onPriority={setPriorityFilter}
              onDifficulty={setDifficultyFilter}
              onToggleCategory={toggleCategoryFilter}
              onClearCategories={() => updateSearch({ category: undefined })}
              onSearch={(value) => updateSearch({ search: value })}
              onClearFilters={clearFilters}
            />
          </section>
        )}
      </div>

      {/* Mobile column tabs — a single thin row between the header block
          and the board scroller (below md; hidden from md up, where every
          column is already on screen). A DIRECT flex child of the page
          root, NOT inside the scroller or the DndContext: the controls must
          neither pan with the columns nor take part in a drag. Counts are
          `displayTasks` — the same filtered + capped array the column
          header pills show, including the in-drag preview. */}
      {boardShown && (
        <BoardColumnTabs
          tabs={COLUMNS.map((column) => ({
            status: column.status,
            title: column.title,
            count: displayTasks(column.status).length,
          }))}
          activeStatus={activeColumn}
          onSelect={scrollToColumn}
        />
      )}

      {boardShown && (
        /* Five status columns (ADR 0002 § UI). The page is a fixed 100dvh
                column, so this scroller takes exactly the height the header
                and filters leave over and pans horizontally only
                (`overflow-y-hidden`) — the vertical scroll lives in each
                column body. Columns are `85vw` below md and a fixed 260px
                from md up; below md the row snap-scrolls so the next column
                peeks in, with snapping OFF while a card is dragged so
                dnd-kit's horizontal auto-scroll can carry it into an
                off-screen column. The row stays centered and overflows
                sideways on narrow screens, and `mb-24` sits the scrollbar
                just above the floating nav. */
        <DndContext
          sensors={sensors}
          collisionDetection={boardCollisionDetection}
          onDragStart={handleDragStart}
          onDragOver={handleDragOver}
          onDragEnd={handleDragEnd}
          onDragCancel={() => {
            setActiveDrag(null);
            // Discard the preview or a cancelled cross-column hover would
            // leave the ghost parked in dest.
            setDragItems(null);
          }}
        >
          <div
            ref={scrollerRef}
            className={cn(
              'mb-24 min-h-0 flex-1 overflow-x-auto overflow-y-hidden pb-2',
              // mandatory snap fights dnd-kit: it writes scrollLeft every frame and
              // the browser snaps back, so a card can never reach an off-screen column.
              activeDrag === null &&
                'max-md:snap-x max-md:snap-mandatory max-md:scroll-px-4',
            )}
          >
            <div className="mx-auto flex h-full w-max gap-4 px-4 md:gap-6 md:px-6">
              {COLUMNS.map((column) => (
                <BoardColumnView
                  key={column.status}
                  column={column}
                  tasks={displayTasks(column.status)}
                  items={displayItems[column.status]}
                  movingIds={movingIds}
                  onEditTask={openEditTask}
                  onToggleFocus={handleToggleFocus}
                  focusDisabled={focusInFlight}
                  onAddTask={openCreateTask}
                />
              ))}
            </div>
          </div>

          {/* Slightly elevated copy of the card following the pointer */}
          <DragOverlay>
            {activeDrag && (
              <div className="cursor-grabbing opacity-90 shadow-2xl ring-1 ring-border/60 rounded-lg">
                <TaskCard task={activeDrag} />
              </div>
            )}
          </DragOverlay>
        </DndContext>
      )}

      {/* Mobile filters sheet (below md — the inline row is `hidden md:block`
          and both stay mounted, so each copy has its own search input id).
          Radix portals it, so it sits here next to TaskModal, never inside
          the board scroller. Filters apply live: no Apply button, and the
          only dismissal is Escape / backdrop / the X. */}
      <Sheet open={filtersOpen} onOpenChange={setFiltersOpen}>
        <SheetContent
          onEscapeKeyDown={(event) => {
            // The category panel open INSIDE this sheet must swallow Escape:
            // Radix listens for Escape in the CAPTURE phase (registered when
            // the dialog opens), so it has already decided to dismiss by the
            // time CategoryFilter's own bubble listener runs — preventing
            // default is only possible here. The panel's listener then closes
            // just the panel, leaving the sheet mounted.
            //
            // Scope the check to the sheet's own host: `event.currentTarget`
            // is `document`, and a `document.querySelector` would also find
            // the DESKTOP row's panel (portaled to `document.body`) while the
            // sheet happens to be open. Only the in-sheet panel lives here.
            if (portalEl?.querySelector('[data-category-filter-panel]')) {
              event.preventDefault();
            }
          }}
        >
          {/* The visible shell: rounded top corners and the scroll clip live
              HERE, not on SheetContent — the sheet frame must stay
              `overflow-visible` so the category panel (portaled into the host
              below) can grow past the shell. `max-h`, never `h`: a short
              filter stack must not force an 85dvh empty panel. */}
          <div className="flex max-h-[85dvh] flex-col overflow-hidden rounded-t-2xl border border-border bg-card shadow-lg">
            <SheetHeader className="shrink-0 px-4 pb-2 pr-12 pt-4 text-left">
              <SheetTitle>Filters</SheetTitle>
            </SheetHeader>
            {/* min-h-0 + flex-1: once the shell hits 85dvh the controls scroll
                inside the sheet instead of pushing it taller. The safe-area
                padding keeps the last control off the home indicator. */}
            <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-4 pb-[max(1rem,env(safe-area-inset-bottom))]">
              <BoardFilters
                layout="stack"
                // The inline row is still mounted behind `hidden md:block`, so
                // the two copies must not share an input id — its `htmlFor`
                // would point at the hidden field.
                searchInputId="board-task-search-sheet"
                priority={priority}
                difficulty={difficulty}
                search={search}
                lists={lists}
                categories={categories}
                selectedCategoryIds={selectedCategoryIds}
                onPriority={setPriorityFilter}
                onDifficulty={setDifficultyFilter}
                onToggleCategory={toggleCategoryFilter}
                onClearCategories={() => updateSearch({ category: undefined })}
                onSearch={(value) => updateSearch({ search: value })}
                onClearFilters={clearFilters}
                // The category panel must be a DESCENDANT of the dialog
                // content or Radix focus/outside-click break; `null` until the
                // host below mounts, so CategoryFilter never falls back to
                // document.body.
                categoryPortalContainer={portalEl}
              />
              {/* Always shown, filters or not (the desktop header button is
                  unreachable from here). Closes first so the sheet is not
                  left open behind the categories route. */}
              <button
                type="button"
                className="mt-4 py-1.5 text-sm font-medium text-primary hover:underline"
                onClick={() => {
                  setFiltersOpen(false);
                  navigate({ to: '/categories' });
                }}
              >
                Edit Categories
              </button>
            </div>
          </div>
          {/* Portal host for the category panel: a SIBLING of the shell (the
              shell clips) pinned to the whole SheetContent frame, which is
              `fixed` and therefore the containing block. `pointer-events-none`
              so taps pass through to the form; the panel re-enables pointers
              on itself, and `overflow-visible` lets it extend past the frame. */}
          <div
            ref={setPortalEl}
            className="pointer-events-none absolute inset-0 z-10 overflow-visible"
          />
        </SheetContent>
      </Sheet>

      {/* New / Edit Task Dialog — edit mode carries the immediate status
          pills (onMove) */}
      <TaskModal
        open={taskForm !== null}
        onOpenChange={(open) => !open && closeTaskForm()}
        task={taskForm?.mode === 'edit' ? taskForm.task : undefined}
        createStatus={
          taskForm?.mode === 'create' ? taskForm.createStatus : undefined
        }
        onSubmit={handleTaskSubmit}
        onDelete={handleTaskDelete}
        onMove={handleMoveTask}
        onToggleFocus={handleToggleFocus}
      />
    </div>
  );
}
