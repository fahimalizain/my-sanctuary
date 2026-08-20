import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  DndContext,
  DragOverlay,
  PointerSensor,
  closestCorners,
  useSensor,
  useSensors,
} from '@dnd-kit/core';
import type { DragEndEvent, DragStartEvent } from '@dnd-kit/core';
import { Loader2, Plus } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { DisplaceDialog } from '@/app/components/DisplaceDialog';
import { TaskModal } from '@/app/components/TaskModal';
import { useNavigate, useSearch } from '@tanstack/react-router';
import { API_BASE_URL } from '@/lib/api';
import type {
  CategoriesResponse,
  Category,
  MoveDisplaceInput,
  MoveTaskInput,
  MoveTaskResponse,
  NewTaskInput,
  TaskDifficulty,
  TaskListsResponse,
  TaskPriority,
  TaskRecord,
  TaskResponse,
  TaskStatus,
  TasksResponse,
  UpdateTaskInput,
} from '@/app/types';
import { BoardColumnView } from './BoardColumn';
import { FilterPill } from './FilterPill';
import { TaskCard } from './TaskCard';
import {
  COLUMNS,
  COLUMN_ID_PREFIX,
  TERMINAL_COLUMN_CAP,
  readError,
  readMoveError,
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

// A drop onto an occupied In Progress column (ADR 0002 § UI): the move is NOT
// applied optimistically yet — the user picks where the running task A is
// parked, then a single `/move` with `displace` runs. Cancel drops the stash.
interface DisplacePrompt {
  /** The dragged task (B) trying to enter In Progress. */
  taskId: string;
  /** B's status before the drop (used to describe the move). */
  fromStatus: TaskStatus;
  /** The task (A) currently running — the one the dialog parks. */
  runningTask: TaskRecord;
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
  // Cards with a /move request in flight — dragging them again is ignored
  // until the response lands (no queuing).
  const [movingIds, setMovingIds] = useState<Set<string>>(new Set());
  // Occupied In Progress conflict stash; non-null renders the park dialog.
  const [displacePrompt, setDisplacePrompt] = useState<DisplacePrompt | null>(
    null,
  );

  // PointerSensor with an 8px activation distance: a plain click still opens
  // the edit modal, and only a real 8px+ movement starts a drag (ADR 0002
  // § DnD). Sensors live on the cards, so grabbing the scroll gutter or the
  // column headers still scrolls the board horizontally.
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 8 } }),
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

  const toggleCategoryFilter = (id: string) => {
    const next = new Set(selectedCategoryIds);
    if (next.has(id)) {
      next.delete(id);
    } else {
      next.add(id);
    }
    updateSearch({
      category: next.size > 0 ? [...next].join(',') : undefined,
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
      if (
        selectedCategoryIds.length > 0 &&
        !selectedCategoryIds.includes(task.category.id)
      ) {
        return false;
      }
      return true;
    },
    [priority, difficulty, selectedCategoryIds],
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

  // ──────────────────────────────────────────
  // Drag + optimistic move (ADR 0002 § DnD, § UI)
  // ──────────────────────────────────────────

  const handleDragStart = (event: DragStartEvent) => {
    const task = tasksRef.current.find((entry) => entry.id === event.active.id);
    // Unknown (e.g. deleted while dragging) — show nothing in the overlay.
    setActiveDrag(task ?? null);
  };

  /** One negotiated `/move` request. Applies the OPTIMISTIC change first
   *  (caller has already snapshotted and decided what to send), then:
   *  - 200 → merge `response.task` (and `response.displaced`, if present);
   *  - failure with `displaced` in the body → the parked task A stays, only
   *    the moved card B is snapped back to its pre-drop snapshot;
   *  - failure without `displaced` → restore the full snapshot.
   *  Every failure raises the error banner and returns the message (null on
   *  success) so the task modal can show it on the form too. */
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
        const failure = await readMoveError(res);
        if (failure.displaced) {
          // ADR 0002 § Move API: no rollback for A — it stays parked. Only
          // the moved card goes back to where it was before the drop.
          setTasks((prev) =>
            prev.map((entry) => {
              if (entry.id === failure.displaced!.id) return failure.displaced!;
              if (entry.id === taskId) {
                return snapshot.find((snap) => snap.id === taskId) ?? entry;
              }
              return entry;
            }),
          );
        } else {
          setTasks(snapshot);
        }
        setActionError(failure.error);
        return failure.error;
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
   *  persist. Dropped-card status/sort_order are set directly; sibling
   *  ranks are left alone until the response lands, so a moment of two cards
   *  at the same rank is fine (tasksForColumn re-sorts immediately). */
  const performMove = async (
    taskId: string,
    destStatus: TaskStatus,
    destSortOrder: number,
  ) => {
    const snapshot = tasksRef.current;
    setTasks((prev) =>
      prev.map((entry) =>
        entry.id === taskId
          ? { ...entry, status: destStatus, sort_order: destSortOrder }
          : entry,
      ),
    );
    await sendMoveRequest(
      taskId,
      { status: destStatus, sort_order: destSortOrder },
      snapshot,
      (data) => {
        setTasks((prev) =>
          prev.map((entry) => {
            if (entry.id === data.task.id) return data.task;
            if (data.displaced && entry.id === data.displaced.id) {
              return data.displaced;
            }
            return entry;
          }),
        );
      },
    );
  };

  /** Resolves a drop against the displayed (filtered + capped) lists and
   *  either starts the optimistic move, opens the displace dialog, or does
   *  nothing (dropped back onto its own slot). */
  const handleDragEnd = (event: DragEndEvent) => {
    const { active, over } = event;
    setActiveDrag(null);
    if (!over || typeof active.id !== 'string') return;
    const activeId = active.id;

    const activeTask = tasksRef.current.find((task) => task.id === activeId);
    if (!activeTask) return;

    // `over` is either a card (task id) or a column (`column:<status>`).
    const overId = over.id;
    let destStatus: TaskStatus;
    let destIndex: number; // view index in the dest column's displayed list
    if (typeof overId === 'string' && overId.startsWith(COLUMN_ID_PREFIX)) {
      destStatus = overId.slice(COLUMN_ID_PREFIX.length) as TaskStatus;
      destIndex = tasksForColumn(destStatus).length; // end (0 when empty)
    } else if (typeof overId === 'string') {
      const overTask = tasksRef.current.find((task) => task.id === overId);
      if (!overTask) return;
      destStatus = overTask.status;
      const destShown = tasksForColumn(destStatus);
      destIndex = destShown.findIndex((task) => task.id === overId);
      if (destIndex === -1) return; // not displayed (filtered out) — ignore
    } else {
      return;
    }

    const destShown = tasksForColumn(destStatus);
    const remaining = destShown.filter((task) => task.id !== activeId);
    const destSortOrder = resolveSortOrder(remaining, destIndex, destStatus);

    // Same column + same slot = no movement: never call the API (In Progress
    // singleton drops land here too — there is only slot 0).
    if (destStatus === activeTask.status) {
      const oldIndex = destShown.findIndex((task) => task.id === activeId);
      const slot = destIndex >= remaining.length ? remaining.length : destIndex;
      if (oldIndex !== -1 && slot === oldIndex) return;
    }

    // Occupied In Progress (ADR 0002 § UI): the optimistic move is NOT
    // applied. Stash the drop and ask where the running task goes.
    if (destStatus === 'IN_PROGRESS') {
      const runningTask = tasksRef.current.find(
        (task) => task.status === 'IN_PROGRESS' && task.id !== activeId,
      );
      if (runningTask) {
        setDisplacePrompt({
          taskId: activeId,
          fromStatus: activeTask.status,
          runningTask,
        });
        return;
      }
    }

    void performMove(activeId, destStatus, destSortOrder);
  };

  /** Confirm in the conflict dialog: park the running task A at the chosen
   *  status (prepend, rank 0 — displacement has no drop position), move B to
   *  In Progress, then ONE `/move` carrying both halves via `displace`. */
  const confirmDisplace = (
    parkStatus: 'PLANNED' | 'COMPLETED' | 'DISCARDED',
  ) => {
    const prompt = displacePrompt;
    if (!prompt) return;
    setDisplacePrompt(null);

    const snapshot = tasksRef.current;
    setTasks((prev) =>
      prev.map((entry) => {
        if (entry.id === prompt.runningTask.id) {
          return { ...entry, status: parkStatus, sort_order: 0 };
        }
        if (entry.id === prompt.taskId) {
          return { ...entry, status: 'IN_PROGRESS', sort_order: 0 };
        }
        return entry;
      }),
    );
    const body: MoveTaskInput = {
      status: 'IN_PROGRESS',
      sort_order: 0,
      displace: {
        id: prompt.runningTask.id,
        status: parkStatus,
        sort_order: 0,
      },
    };
    void sendMoveRequest(prompt.taskId, body, snapshot, (data) => {
      setTasks((prev) =>
        prev.map((entry) => {
          if (entry.id === data.task.id) return data.task;
          if (data.displaced && entry.id === data.displaced.id) {
            return data.displaced;
          }
          return entry;
        }),
      );
    });
  };

  /** TaskModal's immediate status change (ADR 0002): a drop with no drop
   *  position — always prepend, `sort_order: 0`. Optimistic, then ONE
   *  `/move`; `sendMoveRequest` already owns the displaced-aware revert and
   *  the banner. Returns the error message for the form (null on success).
   *  The modal stays open; `task` in props updates via this merge. */
  const handleMoveTask = async (
    taskId: string,
    status: TaskStatus,
    displace?: MoveDisplaceInput,
  ): Promise<string | null> => {
    const snapshot = tasksRef.current;
    setTasks((prev) =>
      prev.map((entry) => {
        if (displace && entry.id === displace.id) {
          return {
            ...entry,
            status: displace.status,
            sort_order: displace.sort_order,
          };
        }
        return entry.id === taskId
          ? { ...entry, status, sort_order: 0 }
          : entry;
      }),
    );
    return sendMoveRequest(
      taskId,
      { status, sort_order: 0, displace },
      snapshot,
      (data) => {
        setTasks((prev) =>
          prev.map((entry) => {
            if (entry.id === data.task.id) return data.task;
            if (data.displaced && entry.id === data.displaced.id) {
              return data.displaced;
            }
            return entry;
          }),
        );
        // Keep the modal's `task` prop in sync so the selected pill follows
        // the server — the modal stays open after a status change.
        setTaskForm((prev) =>
          prev && prev.mode === 'edit' && prev.task?.id === data.task.id
            ? { ...prev, task: data.task }
            : prev,
        );
      },
    );
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
  // prepends (sort_order 0) — the same matrix the board drop uses. Create
  // into an OCCUPIED In Progress column carries `displace`: TaskModal opened
  // the park dialog BEFORE the create write, so this only runs after the
  // user confirmed where the runner goes (cancel never writes a thing). The
  // move failure path closes the modal, raises the banner and leaves the
  // orphan card OPEN in Backlog (the server-owned create is never deleted);
  // a `displaced` failure body keeps the runner parked (same rule as
  // sendMoveRequest), anything else full-restores the pre-move snapshot.
  const handleTaskSubmit = async (
    values: {
      title: string;
      description: string;
      durationMinutes: number;
      priority: TaskPriority;
      difficulty: TaskDifficulty;
    },
    displace?: MoveDisplaceInput,
  ): Promise<string | null> => {
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
    // /move reorder. Card prepends Backlog.
    if (dest === 'OPEN') {
      setTasks((prev) => [data.task, ...prev]);
      closeTaskForm();
      return null;
    }

    // Otherwise create-then-move: optimistically place the card at the front
    // of the destination column (sibling ranks untouched), then persist with
    // ONE /move. With `displace` (occupied In Progress create) the runner is
    // parked in the same optimistic frame — the dialog ran before the create
    // write, so canceling never left a stray Backlog card to clean up.
    const snapshotBeforeOptimistic = tasksRef.current;
    const moveBody: MoveTaskInput = {
      status: dest,
      sort_order: 0,
      ...(displace ? { displace } : {}),
    };
    setTasks((prev) => {
      const next = prev.map((entry) =>
        displace && entry.id === displace.id
          ? {
              ...entry,
              status: displace.status,
              sort_order: displace.sort_order,
            }
          : entry,
      );
      return [{ ...data.task, status: dest, sort_order: 0 }, ...next];
    });

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
      // created card is present as OPEN/Backlog (the orphan stays — the
      // server owns the create, nothing to undo). When displace was applied
      // optimistically, this also restores the parked runner. Banner, close.
      setTasks(() => [data.task, ...snapshotBeforeOptimistic]);
      const message = err instanceof Error ? err.message : 'Move failed';
      setActionError(message);
      closeTaskForm();
      return null;
    }
    // Move failure (409 on occupied In Progress, 401 missing Google token,
    // …): when the body carries a `displaced` row, the runner stays parked
    // (same rule as sendMoveRequest) and only the created card snaps back to
    // OPEN/Backlog; otherwise the full pre-move snapshot is restored with
    // the created card prepended OPEN. Banner, close (null closes the modal).
    if (!moveRes.ok) {
      const failure = await readMoveError(moveRes);
      if (failure.displaced) {
        // A stays parked; B (new task) goes to OPEN/Backlog; every other
        // card comes back from the snapshot (taken before B's insert).
        setTasks(() => {
          const base = snapshotBeforeOptimistic.map((entry) =>
            entry.id === failure.displaced!.id ? failure.displaced! : entry,
          );
          return [data.task, ...base.filter((e) => e.id !== data.task.id)];
        });
      } else {
        setTasks(() => [data.task, ...snapshotBeforeOptimistic]);
      }
      setActionError(failure.error);
      closeTaskForm();
      return null;
    }
    // Move success: merge the authoritative row (and any displaced task).
    const moveData = (await moveRes.json()) as MoveTaskResponse;
    setTasks((prev) =>
      prev.map((entry) => {
        if (entry.id === moveData.task.id) return moveData.task;
        if (moveData.displaced && entry.id === moveData.displaced.id) {
          return moveData.displaced;
        }
        return entry;
      }),
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

  // The sole running task, if any (may be the task being edited). Decides
  // whether tapping In Progress in the modal needs the park dialog.
  const runningTask =
    tasks.find((task) => task.status === 'IN_PROGRESS') ?? null;

  // The conflict dialog reads both titles from live state so they stay
  // correct even if the optimistic render happens first.
  const runningTaskTitle =
    displacePrompt &&
    (
      tasks.find((task) => task.id === displacePrompt.runningTask.id) ??
      displacePrompt.runningTask
    ).title;
  const draggedTaskTitle =
    displacePrompt &&
    (tasks.find((task) => task.id === displacePrompt.taskId)?.title ??
      'this task');

  return (
    <div className="min-h-screen bg-cream">
      {/* pb-28 clears the floating nav, so the last cards stay reachable */}
      <div className="max-w-7xl mx-auto px-6 py-8 pb-28">
        {/* Header */}
        <header className="flex items-center justify-between mb-8">
          <div>
            <h1 className="font-heading text-3xl font-bold text-foreground mb-2">
              Board
            </h1>
            <p className="text-muted-foreground">
              Backlog, Planned, In Progress, Done, Discarded
            </p>
          </div>
          <div className="flex gap-3">
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
          <>
            {/* URL filters — priority / difficulty are single-select with an
                All option, category is multi-select. Combined with AND. */}
            <section className="mb-6 space-y-3">
              <div className="flex flex-wrap items-center gap-2">
                <span className="w-20 shrink-0 text-xs font-medium uppercase tracking-wide text-muted-foreground">
                  Priority
                </span>
                <FilterPill
                  selected={priority === undefined}
                  onClick={() => setPriorityFilter(undefined)}
                >
                  All
                </FilterPill>
                {(['high', 'medium', 'low'] as TaskPriority[]).map((value) => (
                  <FilterPill
                    key={value}
                    selected={priority === value}
                    onClick={() => setPriorityFilter(value)}
                  >
                    {value[0].toUpperCase() + value.slice(1)}
                  </FilterPill>
                ))}
              </div>

              <div className="flex flex-wrap items-center gap-2">
                <span className="w-20 shrink-0 text-xs font-medium uppercase tracking-wide text-muted-foreground">
                  Difficulty
                </span>
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

              <div className="flex flex-wrap items-center gap-2">
                <span className="w-20 shrink-0 text-xs font-medium uppercase tracking-wide text-muted-foreground">
                  Category
                </span>
                {filterableCategories.length > 0 ? (
                  filterableCategories.map((cat) => (
                    <FilterPill
                      key={cat.id}
                      selected={selectedCategoryIds.includes(cat.id)}
                      onClick={() => toggleCategoryFilter(cat.id)}
                    >
                      <span
                        className="h-2 w-2 rounded-full"
                        style={{ backgroundColor: cat.color }}
                        aria-hidden
                      />
                      {cat.title}
                    </FilterPill>
                  ))
                ) : (
                  <span className="text-sm text-muted-foreground italic">
                    No categories yet
                  </span>
                )}
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

            {/* Five status columns, horizontal scroll on narrow screens.
                DndContext wraps the scroll row: cards start a drag after an
                8px press-move, the scroll gutter and headers still pan. */}
            <DndContext
              sensors={sensors}
              collisionDetection={closestCorners}
              onDragStart={handleDragStart}
              onDragEnd={handleDragEnd}
              onDragCancel={() => setActiveDrag(null)}
            >
              <div className="flex gap-6 overflow-x-auto pb-4">
                {COLUMNS.map((column) => (
                  <BoardColumnView
                    key={column.status}
                    column={column}
                    tasks={tasksForColumn(column.status)}
                    items={itemsByColumn[column.status]}
                    movingIds={movingIds}
                    onEditTask={openEditTask}
                    onAddTask={openCreateTask}
                  />
                ))}
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
          </>
        )}
      </div>

      {/* Occupied In Progress conflict dialog (ADR 0002 § UI): pick where the
          running task is parked. Cancel / overlay close drops the stash with
          no request. Shared component — also used by the task modal's status
          pills. */}
      <DisplaceDialog
        open={displacePrompt !== null}
        onOpenChange={(open) => {
          if (!open) setDisplacePrompt(null);
        }}
        runningTitle={runningTaskTitle || ''}
        incomingTitle={draggedTaskTitle || ''}
        onConfirm={confirmDisplace}
      />

      {/* New / Edit Task Dialog — edit mode carries the immediate status
          pills (onMove) and the running task for the park dialog */}
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
        runningTask={runningTask}
      />
    </div>
  );
}
