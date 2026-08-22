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
import { Loader2, Plus } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { TaskModal } from '@/app/components/TaskModal';
import { useNavigate, useSearch } from '@tanstack/react-router';
import { API_BASE_URL } from '@/lib/api';
import {
  TASK_PRIORITIES,
  TASK_PRIORITY_LABELS,
  type CategoriesResponse,
  type Category,
  type FocusTaskResponse,
  type MoveTaskInput,
  type MoveTaskResponse,
  type NewTaskInput,
  type TaskDifficulty,
  type TaskListsResponse,
  type TaskPriority,
  type TaskRecord,
  type TaskResponse,
  type TaskStatus,
  type TasksResponse,
  type UpdateTaskInput,
} from '@/app/types';
import { BoardColumnView } from './BoardColumn';
import { CategoryFilter } from './CategoryFilter';
import { FilterPill } from './FilterPill';
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
import { categoryMatchesSelection, toggleCategoryId } from './board-filters';
import {
  COLUMNS,
  COLUMN_ID_PREFIX,
  TERMINAL_COLUMN_CAP,
  applyOptimisticMove,
  defaultMoveRank,
  readError,
  resolveSortOrder,
} from './board-model';
import type { BoardSearch } from './board-model';

interface TaskFormState {
  mode: 'create' | 'edit';
  /** Edit target. */
  task?: TaskRecord;
  /** Create destination. Always set when mode === 'create' on the board. */
  createStatus?: TaskStatus;
}

export function BoardPage() {
  const navigate = useNavigate();
  const { priority, difficulty, category } = useSearch({ from: '/board' });

  const [lists, setLists] = useState<TaskListsResponse['lists']>([]);
  const [tasks, setTasks] = useState<TaskRecord[]>([]);
  const [categories, setCategories] = useState<Category[]>([]);
  // Latest `lists` for the dependency-free `load` callback below (writing a
  // ref during render is the "latest value" pattern). Reading it lets `load`
  // decide whether to show the full-page loader without closing over a stale
  // array.
  const listsRef = useRef<TaskListsResponse['lists']>([]);
  listsRef.current = lists;
  // Same pattern for `tasks`: the async move flow reads the pre-drop snapshot
  // and looks up the dropped card from the latest render, never a stale one.
  const tasksRef = useRef<TaskRecord[]>([]);
  tasksRef.current = tasks;
  const [isLoading, setIsLoading] = useState(true);
  // Load failures: only set from `load()`. Replaces the board with the
  // error+retry banner when there are no lists to show.
  const [loadError, setLoadError] = useState<string | null>(null);
  // Action failures (move 409, etc.): rendered as a banner above the
  // still-visible board — cards are never unmounted by an action error.
  const [actionError, setActionError] = useState<string | null>(null);

  // Task dialog state.
  const [taskForm, setTaskForm] = useState<TaskFormState | null>(null);

  // Drag state (ADR 0002 § DnD).
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

  const load = useCallback(() => {
    // Full-page loader only when the board is empty (first load, or a retry
    // after a hard error cleared it) — the same rule as ListsPage, so
    // reloads fired while cards are on screen never flash the spinner.
    setIsLoading(listsRef.current.length === 0);
    setLoadError(null);
    // Sequential on purpose: GET /api/lists performs the first-visit seed (it
    // inserts the default lists AND the category taxonomy), so the tasks and
    // categories requests must run after it — their computed categories
    // depend on the seeded taxonomy. Tasks and categories are independent of
    // each other and load in parallel (the same seed rule as ListsPage).
    fetch(`${API_BASE_URL}/api/lists`, { credentials: 'include' })
      .then(async (listsRes) => {
        if (!listsRes.ok) throw new Error(await readError(listsRes));
        const listsData = (await listsRes.json()) as TaskListsResponse;
        setLists(listsData.lists ?? []);
        const [tasksRes, categoriesRes] = await Promise.all([
          fetch(`${API_BASE_URL}/api/tasks`, { credentials: 'include' }),
          fetch(`${API_BASE_URL}/api/categories`, { credentials: 'include' }),
        ]);
        if (!tasksRes.ok) throw new Error(await readError(tasksRes));
        if (!categoriesRes.ok) throw new Error(await readError(categoriesRes));
        const [tasksData, categoriesData] = await Promise.all([
          tasksRes.json() as Promise<TasksResponse>,
          categoriesRes.json() as Promise<CategoriesResponse>,
        ]);
        setTasks(tasksData.tasks ?? []);
        setCategories(categoriesData.categories ?? []);
      })
      .catch((err: unknown) => {
        const message =
          err instanceof Error ? err.message : 'Failed to load board';
        setLoadError(message);
      })
      .finally(() => setIsLoading(false));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  // ──────────────────────────────────────────
  // URL filters (ADR 0002 § Filters)
  // ──────────────────────────────────────────

  // Filter chips are built from the loaded categories; untracked is a
  // system category that is never offered as a filter chip. Untracked
  // cards are not hidden from the columns, and a `?category=` URL that
  // names the sink still matches (the sink id is in `categories`).
  const filterableCategories = useMemo(
    () => categories.filter((cat) => !cat.is_untracked),
    [categories],
  );

  const categoryById = useMemo(
    () => new Map(categories.map((cat) => [cat.id, cat])),
    [categories],
  );

  // Unknown ids from the URL are ignored (ADR 0002): they cannot match any
  // task's computed category.
  const selectedCategoryIds = useMemo(() => {
    if (!category) return [];
    return category
      .split(',')
      .map((id) => id.trim())
      .filter((id) => id.length > 0 && categoryById.has(id));
  }, [category, categoryById]);

  const hasActiveFilters =
    priority !== undefined ||
    difficulty !== undefined ||
    selectedCategoryIds.length > 0;

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
      return true;
    },
    [priority, difficulty, selectedCategoryIds, categories],
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
      const res = await fetch(`${API_BASE_URL}/api/tasks/${taskId}/move`, {
        method: 'POST',
        credentials: 'include',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (!res.ok) {
        const message = await readError(res);
        setTasks(snapshot);
        setActionError(message);
        return message;
      }
      onSuccess((await res.json()) as MoveTaskResponse);
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
  // Focus toggle (task-focus, slice 4)
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
   *  show it on the form too. Mirrors sendMoveRequest (API_BASE_URL,
   *  credentials: 'include', readError). */
  const sendFocusRequest = async (
    isFocus: boolean,
    taskId: string,
    snapshot: TaskRecord[],
    onSuccess: (data: FocusTaskResponse) => void,
  ): Promise<string | null> => {
    setFocusInFlight(true);
    setActionError(null);
    try {
      const res = isFocus
        ? await fetch(`${API_BASE_URL}/api/tasks/${taskId}/focus`, {
            method: 'POST',
            credentials: 'include',
          })
        : await fetch(`${API_BASE_URL}/api/focus`, {
            method: 'DELETE',
            credentials: 'include',
          });
      if (!res.ok) {
        const message = await readError(res);
        setTasks(snapshot);
        setActionError(message);
        return message;
      }
      onSuccess((await res.json()) as FocusTaskResponse);
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
  // Task actions (New Task + click-to-edit; no timer buttons this slice)
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
    const body: NewTaskInput | UpdateTaskInput = {
      title: values.title,
      description: values.description,
      duration_minutes: values.durationMinutes,
      priority: values.priority,
      difficulty: values.difficulty,
    };

    // ── Edit (PATCH) — unchanged: merge the returned task, close.
    if (taskForm.mode === 'edit') {
      const res = await fetch(
        `${API_BASE_URL}/api/tasks/${taskForm.task!.id}`,
        {
          method: 'PATCH',
          credentials: 'include',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(body),
        },
      );
      if (!res.ok) {
        return await readError(res);
      }
      const data = (await res.json()) as TaskResponse;
      setTasks((prev) =>
        prev.map((entry) => (entry.id === data.task.id ? data.task : entry)),
      );
      closeTaskForm();
      return null;
    }

    // ── Create (POST always stamps OPEN on the server — `createStatus` only
    //    decides whether a follow-up /move is needed, it never goes in the
    //    request body).
    const createRes = await fetch(`${API_BASE_URL}/api/tasks`, {
      method: 'POST',
      credentials: 'include',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
    // Create 400 etc.: return the error, the modal stays open on the form.
    if (!createRes.ok) {
      return await readError(createRes);
    }
    // The response carries the computed category — reuse it directly so the
    // card lands in the right column instantly. The server never returns an
    // untracked result on create (create stays strict).
    const data = (await createRes.json()) as TaskResponse;
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

    let moveRes: Response;
    try {
      moveRes = await fetch(`${API_BASE_URL}/api/tasks/${data.task.id}/move`, {
        method: 'POST',
        credentials: 'include',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(moveBody),
      });
    } catch (err) {
      // Network failure: restore the pre-move snapshot, then ensure the
      // created card is present as OPEN/Backlog at the server-assigned
      // append rank (the orphan stays — the server owns the create, nothing
      // to undo). Banner, close.
      setTasks(() => [data.task, ...snapshotBeforeOptimistic]);
      const message = err instanceof Error ? err.message : 'Move failed';
      setActionError(message);
      closeTaskForm();
      return null;
    }
    // Move failure (401 missing Google token, 400, …): restore the full
    // pre-move snapshot with the created card OPEN at its append rank.
    // Banner, close (null closes the modal).
    if (!moveRes.ok) {
      const message = await readError(moveRes);
      setTasks(() => [data.task, ...snapshotBeforeOptimistic]);
      setActionError(message);
      closeTaskForm();
      return null;
    }
    // Move success: merge the authoritative row.
    const moveData = (await moveRes.json()) as MoveTaskResponse;
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
    const res = await fetch(`${API_BASE_URL}/api/tasks/${taskId}`, {
      method: 'DELETE',
      credentials: 'include',
    });
    if (!res.ok) {
      return await readError(res);
    }
    setTasks((prev) => prev.filter((entry) => entry.id !== taskId));
    closeTaskForm();
    return null;
  };

  return (
    <div className="flex min-h-screen flex-col bg-cream">
      <div className="mx-auto w-full max-w-7xl px-6 pt-8">
        {/* Header */}
        <header className="mb-8 flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
          <div>
            <h1 className="font-heading text-3xl font-bold text-foreground mb-2">
              Board
            </h1>
            <p className="text-muted-foreground">
              Backlog, Planned, In Progress, Done, Discarded
            </p>
          </div>
          <div className="flex shrink-0 gap-3">
            <Button variant="outline" onClick={() => openCreateTask('OPEN')}>
              <Plus className="h-4 w-4 mr-2" />
              New Task
            </Button>
            <Button
              variant="outline"
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
            <p className="text-sm">{loadError}</p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                setLoadError(null);
                load();
              }}
            >
              Retry
            </Button>
          </div>
        )}

        {/* Action error banner (e.g. a failed move) — the board stays mounted */}
        {actionError && (
          <div className="mb-6 flex items-center justify-between gap-4 bg-destructive/10 text-destructive rounded-xl px-4 py-3">
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
        {(lists.length > 0 || (!isLoading && !loadError)) && (
          /* URL filters — one wrapping horizontal row (ADR 0002 § UI):
                Priority and Difficulty are single-select All+value pills;
                Category is a searchable checkbox combobox grouped by list →
                root → children. Filters combine with AND. */
          <section className="mb-6 space-y-3">
            <div className="flex flex-wrap items-center gap-x-6 gap-y-3">
              <div className="flex flex-col items-start gap-1.5">
                <span className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
                  Priority
                </span>
                <div className="flex flex-wrap items-center gap-2">
                  <FilterPill
                    selected={priority === undefined}
                    onClick={() => setPriorityFilter(undefined)}
                  >
                    All
                  </FilterPill>
                  {TASK_PRIORITIES.map((value) => (
                    <FilterPill
                      key={value}
                      selected={priority === value}
                      onClick={() => setPriorityFilter(value)}
                    >
                      {TASK_PRIORITY_LABELS[value]}
                    </FilterPill>
                  ))}
                </div>
              </div>

              <div className="flex flex-col items-start gap-1.5">
                <span className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
                  Difficulty
                </span>
                <div className="flex flex-wrap items-center gap-2">
                  <FilterPill
                    selected={difficulty === undefined}
                    onClick={() => setDifficultyFilter(undefined)}
                  >
                    All
                  </FilterPill>
                  {(['easy', 'medium', 'hard'] as TaskDifficulty[]).map(
                    (value) => (
                      <FilterPill
                        key={value}
                        selected={difficulty === value}
                        onClick={() => setDifficultyFilter(value)}
                      >
                        {value[0].toUpperCase() + value.slice(1)}
                      </FilterPill>
                    ),
                  )}
                </div>
              </div>

              <div className="flex flex-col items-start gap-1.5">
                <span className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
                  Category
                </span>
                {filterableCategories.length > 0 ? (
                  <CategoryFilter
                    lists={lists}
                    categories={categories}
                    selectedIds={selectedCategoryIds}
                    onToggle={toggleCategoryFilter}
                    onClear={() => updateSearch({ category: undefined })}
                  />
                ) : (
                  <span className="text-sm text-muted-foreground italic">
                    No categories yet
                  </span>
                )}
              </div>
            </div>

            {hasActiveFilters && (
              <div className="flex justify-end">
                <button
                  type="button"
                  onClick={clearFilters}
                  className="text-sm font-medium text-primary hover:underline"
                >
                  Clear filters
                </button>
              </div>
            )}
          </section>
        )}
      </div>

      {(lists.length > 0 || (!isLoading && !loadError)) && (
        /* Five status columns. The scroller fills leftover viewport
                height so empty space below the columns still pans. Columns
                are a fixed 260px; the row stays centered and overflow-x
                on narrow screens. mb-24 sits the scrollbar just above
                the floating nav. */
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
          <div className="mb-24 min-h-0 flex-1 overflow-x-auto pb-2">
            <div className="mx-auto flex min-h-full w-max gap-6 px-6">
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
