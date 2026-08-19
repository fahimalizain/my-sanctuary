import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { Loader2, Plus } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { TaskModal } from '@/app/components/TaskModal';
import { useNavigate, useSearch } from '@tanstack/react-router';
import { API_BASE_URL } from '@/lib/api';
import { cn } from '@/lib/utils';
import type {
  CategoriesResponse,
  Category,
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
async function readError(res: Response): Promise<string> {
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

interface TaskFormState {
  mode: 'create' | 'edit';
  /** Edit target. */
  task?: TaskRecord;
}

interface BoardColumn {
  title: string;
  status: TaskStatus;
  /** 2px accent strip on the column header only — the rest of the column
   *  stays neutral (Categories style). */
  accent: string;
}

// Column order and the status each one renders (ADR 0002 § Status model).
const COLUMNS: BoardColumn[] = [
  { title: 'Backlog', status: 'OPEN', accent: 'bg-muted-foreground/20' },
  { title: 'Planned', status: 'PLANNED', accent: 'bg-sky-500' },
  { title: 'In Progress', status: 'IN_PROGRESS', accent: 'bg-emerald-500' },
  { title: 'Done', status: 'COMPLETED', accent: 'bg-sky-400' },
  { title: 'Discarded', status: 'DISCARDED', accent: 'bg-rose-400' },
];

// Done / Discarded render at most this many filtered matches (ADR 0002
// § Done/Discarded cap). Backlog / Planned / In Progress show all matches.
const TERMINAL_COLUMN_CAP = 20;

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
  const [isLoading, setIsLoading] = useState(true);
  // Load failures: only set from `load()`. Replaces the board with the
  // error+retry banner when there are no lists to show.
  const [loadError, setLoadError] = useState<string | null>(null);
  // Action failures (delete 409, etc.): rendered as a banner above the
  // still-visible board — cards are never unmounted by an action error.
  const [actionError, setActionError] = useState<string | null>(null);

  // Task dialog state.
  const [taskForm, setTaskForm] = useState<TaskFormState | null>(null);

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
  // system category that is never offered as a filter (it is also hidden
  // from the columns themselves).
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
    updateSearch({ priority: undefined, difficulty: undefined, category: undefined });

  // ──────────────────────────────────────────
  // Columns: filter → sort → cap (display only; no /move in this slice)
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
  // `created_at`), then cap Done/Discarded at 20 matches. Untracked tasks
  // stay hidden, the same as Lists.
  const tasksForColumn = useCallback(
    (status: TaskStatus): TaskRecord[] => {
      const matches = tasks.filter(
        (task) =>
          task.status === status &&
          !task.category.is_untracked &&
          matchesFilters(task),
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

  // ──────────────────────────────────────────
  // Task actions (New Task + click-to-edit; no timer buttons this slice)
  // ──────────────────────────────────────────

  const openCreateTask = () => {
    setTaskForm({ mode: 'create' });
  };

  const openEditTask = (task: TaskRecord) => {
    setTaskForm({ mode: 'edit', task });
  };

  const closeTaskForm = () => {
    setTaskForm(null);
  };

  // Persists the task. Returns an error message to show on the form (the
  // server explains 400s like "title does not match a category"), or null
  // when the modal may close.
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
    const res =
      taskForm.mode === 'create'
        ? await fetch(`${API_BASE_URL}/api/tasks`, {
            method: 'POST',
            credentials: 'include',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(body),
          })
        : await fetch(`${API_BASE_URL}/api/tasks/${taskForm.task!.id}`, {
            method: 'PATCH',
            credentials: 'include',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(body),
          });
    if (!res.ok) {
      return await readError(res);
    }
    // The response carries the computed category — reuse it directly so the
    // card lands in the right column instantly (creates prepend Backlog; the
    // columns sort by `sort_order` anyway, and an untracked result — which
    // the server never returns on create — would stay hidden here).
    const data = (await res.json()) as TaskResponse;
    setTasks((prev) =>
      taskForm.mode === 'create'
        ? [data.task, ...prev]
        : prev.map((entry) => (entry.id === data.task.id ? data.task : entry)),
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
            <Button variant="outline" onClick={openCreateTask}>
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

        {/* Action error banner (e.g. a delete 409) — the board stays mounted */}
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
                No drag in this slice — cards are click-to-edit only. */}
            <div className="flex gap-6 overflow-x-auto pb-4">
              {COLUMNS.map((column) => (
                <BoardColumnView
                  key={column.status}
                  column={column}
                  tasks={tasksForColumn(column.status)}
                  onEditTask={openEditTask}
                />
              ))}
            </div>
          </>
        )}
      </div>

      {/* New / Edit Task Dialog */}
      <TaskModal
        open={taskForm !== null}
        onOpenChange={(open) => !open && closeTaskForm()}
        task={taskForm?.mode === 'edit' ? taskForm.task : undefined}
        onSubmit={handleTaskSubmit}
        onDelete={handleTaskDelete}
      />
    </div>
  );
}

/** A pill/toggle in the filter row — same shape as TaskModal's
 *  priority/difficulty chips; selected pills flip to the primary fill. */
function FilterPill({
  selected,
  onClick,
  children,
}: {
  selected: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        'flex items-center gap-2 rounded-xl border-2 px-3 py-1.5 text-sm font-medium transition-all',
        selected
          ? 'bg-primary border-primary text-primary-foreground'
          : 'bg-background border-input text-muted-foreground hover:border-primary/30',
      )}
    >
      {children}
    </button>
  );
}

/** One board column: neutral surface with a 2px status accent on the header
 *  only. The count is the number of cards currently shown (after filter +
 *  cap) — never a `20 / 64` overflow hint. */
function BoardColumnView({
  column,
  tasks,
  onEditTask,
}: {
  column: BoardColumn;
  tasks: TaskRecord[];
  onEditTask: (task: TaskRecord) => void;
}) {
  return (
    <section className="flex min-w-[260px] flex-1 flex-col overflow-hidden rounded-xl border border-border bg-card">
      <header className="shrink-0 border-b border-border">
        <div className={cn('h-0.5', column.accent)} />
        <div className="flex items-center justify-between gap-2 px-4 py-3">
          <h3 className="font-heading text-sm font-semibold text-foreground truncate">
            {column.title}
          </h3>
          <span
            className="flex-shrink-0 rounded-full bg-muted px-2 py-0.5 text-xs font-medium text-muted-foreground"
            aria-label={`${tasks.length} tasks`}
          >
            {tasks.length}
          </span>
        </div>
      </header>

      <div className="flex-1 space-y-2 p-3">
        {tasks.length > 0 ? (
          tasks.map((task) => (
            <TaskCard key={task.id} task={task} onEdit={onEditTask} />
          ))
        ) : (
          <p className="text-sm text-muted-foreground italic">No tasks</p>
        )}
      </div>
    </section>
  );
}

/** A light-surface task chip (the Lists chip on a light card, not the dark
 *  list-colored chip): title + duration + difficulty badge (medium/hard
 *  only) + priority dot + category swatch. The whole chip is the click
 *  target that opens the edit modal — no timer buttons in this slice. */
function TaskCard({
  task,
  onEdit,
}: {
  task: TaskRecord;
  onEdit: (task: TaskRecord) => void;
}) {
  return (
    <button
      type="button"
      onClick={() => onEdit(task)}
      title={`${task.title} — ${task.duration_minutes} min, ${task.priority}${
        task.difficulty !== 'easy' ? `, ${task.difficulty}` : ''
      }`}
      className="flex w-full cursor-pointer items-center gap-1.5 rounded-lg border border-border/60 bg-background px-2.5 py-2 text-left transition-colors hover:border-primary/40 hover:bg-muted/40"
    >
      <span className="flex-1 min-w-0 text-sm text-foreground truncate">
        {task.title}
      </span>
      <span className="flex-shrink-0 text-[10px] text-muted-foreground">
        {task.duration_minutes} min
      </span>
      {task.difficulty === 'hard' || task.difficulty === 'medium' ? (
        <span
          className={cn(
            'flex-shrink-0 rounded-full px-1.5 py-0.5 text-[9px] uppercase tracking-wide',
            task.difficulty === 'hard'
              ? 'bg-foreground/10 font-semibold text-foreground'
              : 'bg-muted font-medium text-muted-foreground',
          )}
        >
          {task.difficulty === 'hard' ? 'HARD' : 'MED'}
        </span>
      ) : null}
      <span
        className={cn(
          'h-1.5 w-1.5 rounded-full flex-shrink-0',
          task.priority === 'high'
            ? 'bg-red-400'
            : task.priority === 'medium'
              ? 'bg-amber-400'
              : 'bg-sky-400',
        )}
        aria-hidden
      />
      <span
        className="h-2 w-2 rounded-full flex-shrink-0"
        style={{ backgroundColor: task.category.color }}
        title={task.category.title}
        aria-hidden
      />
    </button>
  );
}
