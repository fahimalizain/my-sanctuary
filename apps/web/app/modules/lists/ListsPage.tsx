import { useCallback, useEffect, useRef, useState } from 'react';
import {
  Check,
  Loader2,
  MoreHorizontal,
  Pause,
  Pencil,
  Play,
  Plus,
  Square,
  Trash2,
  X,
} from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from '@/components/ui/dialog';
import { TaskModal } from '@/app/components/TaskModal';
import { useNavigate } from '@tanstack/react-router';
import { API_BASE_URL } from '@/lib/api';
import { cn } from '@/lib/utils';
// Lists is unlinked from the nav; one shared helper is fine to import across
// modules — no new package.
import { defaultMoveRank } from '../board/board-model';
import type {
  MoveDisplaceInput,
  MoveTaskError,
  MoveTaskInput,
  MoveTaskResponse,
  NewTaskInput,
  TaskDifficulty,
  TaskList,
  TaskListsResponse,
  TaskPriority,
  TaskRecord,
  TaskResponse,
  TaskStatus,
  TasksResponse,
  UpdateTaskInput,
} from '@/app/types';

// Timer actions map to the same POST endpoints; the server explains 409s.
type TaskAction = 'start' | 'stop' | 'pause' | 'complete' | 'discard';

// The server's error envelope is `{"error": "message"}`; fall back to a
// generic message when the body is not JSON.
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

// Move failures can carry a `displaced` task (ADR 0002 § Move API): when the
// start fails AFTER a successful displace, the parked task stays and only the
// moved task snaps back. Same page-local copy convention as `readError`
// (BoardPage has its own).
async function readMoveError(res: Response): Promise<MoveTaskError> {
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

interface ListFormState {
  mode: 'create' | 'edit';
  list?: TaskList;
}

interface TaskFormState {
  mode: 'create' | 'edit';
  /** Edit target. */
  task?: TaskRecord;
}

export function ListsPage() {
  const navigate = useNavigate();
  const [lists, setLists] = useState<TaskList[]>([]);
  const [tasks, setTasks] = useState<TaskRecord[]>([]);
  // Latest `lists` for the dependency-free `load` callback below (writing a
  // ref during render is the "latest value" pattern). Reading it lets `load`
  // decide whether to show the full-page loader without closing over a stale
  // array.
  const listsRef = useRef<TaskList[]>([]);
  listsRef.current = lists;
  // Latest `tasks` for the async move flow, so the pre-move snapshot and the
  // reverted card lookups never close over a stale array (same pattern as
  // BoardPage).
  const tasksRef = useRef<TaskRecord[]>([]);
  tasksRef.current = tasks;
  const [isLoading, setIsLoading] = useState(true);
  // Load failures: only set from `load()`. Replaces the grid with the
  // error+retry banner when there are no lists to show.
  const [loadError, setLoadError] = useState<string | null>(null);
  // Action failures (delete 409, etc.): rendered as a banner above the
  // still-visible grid — cards are never unmounted by an action error.
  const [actionError, setActionError] = useState<string | null>(null);

  // List dialog state.
  const [listForm, setListForm] = useState<ListFormState | null>(null);
  const [listName, setListName] = useState('');
  const [listColor, setListColor] = useState('#2a5c8a');

  const [saving, setSaving] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);

  // Task dialog state.
  const [taskForm, setTaskForm] = useState<TaskFormState | null>(null);

  const load = useCallback(() => {
    // Full-page loader only when the grid is empty (first load, or a retry
    // after a hard error cleared it) — the same rule as CalendarPage
    // (`isLoading: prev.events.length === 0`), so reloads fired while cards
    // are on screen never flash the spinner.
    setIsLoading(listsRef.current.length === 0);
    setLoadError(null);
    // Sequential on purpose: GET /api/lists performs the first-visit seed (it
    // inserts the default lists AND the category taxonomy), so the tasks
    // request must run after it — their computed categories depend on the
    // seeded taxonomy, and GET /api/tasks also runs the count-gated seed (a
    // no-op once lists seeded). Lists hide untracked tasks (no list to
    // belong to), so the categories endpoint is never needed here.
    fetch(`${API_BASE_URL}/api/lists`, { credentials: 'include' })
      .then(async (listsRes) => {
        if (!listsRes.ok) throw new Error(await readError(listsRes));
        const listsData = (await listsRes.json()) as TaskListsResponse;
        setLists(listsData.lists ?? []);
        return fetch(`${API_BASE_URL}/api/tasks`, { credentials: 'include' });
      })
      .then(async (tasksRes) => {
        if (!tasksRes.ok) throw new Error(await readError(tasksRes));
        const tasksData = (await tasksRes.json()) as TasksResponse;
        setTasks(tasksData.tasks ?? []);
      })
      .catch((err: unknown) => {
        const message =
          err instanceof Error ? err.message : 'Failed to load lists';
        setLoadError(message);
      })
      .finally(() => setIsLoading(false));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  // Refreshes only the tasks (timer actions never change lists, so the
  // sequential lists→tasks reload is unnecessary here).
  const reloadTasks = useCallback(() => {
    fetch(`${API_BASE_URL}/api/tasks`, { credentials: 'include' })
      .then(async (res) => {
        if (!res.ok) throw new Error(await readError(res));
        const data = (await res.json()) as TasksResponse;
        setTasks(data.tasks ?? []);
      })
      .catch((err: unknown) => {
        setActionError(
          err instanceof Error ? err.message : 'Failed to reload tasks',
        );
      });
  }, []);

  // ──────────────────────────────────────────
  // Task timer actions (start/stop/pause/complete/discard)
  // ──────────────────────────────────────────

  // Any task IN_PROGRESS means the single running slot is taken: every Start
  // button is disabled until a stop/pause/complete/discard frees it.
  const anyRunning = tasks.some((task) => task.status === 'IN_PROGRESS');
  // The actual running task record — the task modal needs it to decide
  // whether tapping In Progress must park someone first.
  const runningTask =
    tasks.find((task) => task.status === 'IN_PROGRESS') ?? null;

  const handleTaskAction = async (taskId: string, action: TaskAction) => {
    setActionError(null);
    const res = await fetch(`${API_BASE_URL}/api/tasks/${taskId}/${action}`, {
      method: 'POST',
      credentials: 'include',
    });
    if (!res.ok) {
      // The server's 409 message ("a task is already running") lands in the
      // banner; the cards stay visible.
      setActionError(await readError(res));
      return;
    }
    reloadTasks();
  };

  const handleStartTask = (taskId: string) => handleTaskAction(taskId, 'start');
  const handleStopTask = (taskId: string) => handleTaskAction(taskId, 'stop');
  const handlePauseTask = (taskId: string) => handleTaskAction(taskId, 'pause');
  const handleCompleteTask = (taskId: string) =>
    handleTaskAction(taskId, 'complete');
  const handleDiscardTask = (taskId: string) =>
    handleTaskAction(taskId, 'discard');

  // ──────────────────────────────────────────
  // List actions
  // ──────────────────────────────────────────

  const openCreateList = () => {
    setListForm({ mode: 'create' });
    setListName('');
    setListColor('#2a5c8a');
    setFormError(null);
  };

  const openEditList = (list: TaskList) => {
    setListForm({ mode: 'edit', list });
    setListName(list.name);
    setListColor(list.color);
    setFormError(null);
  };

  const closeListForm = () => {
    setListForm(null);
    setSaving(false);
    setFormError(null);
  };

  const handleListSubmit = async () => {
    if (!listForm) return;
    const trimmed = listName.trim();
    if (!trimmed || !listColor) return;

    setSaving(true);
    setFormError(null);
    setActionError(null);
    const res =
      listForm.mode === 'create'
        ? await fetch(`${API_BASE_URL}/api/lists`, {
            method: 'POST',
            credentials: 'include',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ name: trimmed, color: listColor }),
          })
        : await fetch(`${API_BASE_URL}/api/lists/${listForm.list!.id}`, {
            method: 'PATCH',
            credentials: 'include',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ name: trimmed, color: listColor }),
          });
    if (!res.ok) {
      setSaving(false);
      setFormError(await readError(res));
      return;
    }
    closeListForm();
    load();
  };

  const handleDeleteList = async (list: TaskList) => {
    const confirmed = window.confirm(`Delete "${list.name}"?`);
    if (!confirmed) return;

    setActionError(null);
    const res = await fetch(`${API_BASE_URL}/api/lists/${list.id}`, {
      method: 'DELETE',
      credentials: 'include',
    });
    if (!res.ok) {
      const message = await readError(res);
      // 409 from the backend: living root categories still reference the list.
      setActionError(
        res.status === 409
          ? `"${list.name}" is still in use and cannot be deleted.`
          : message,
      );
      return;
    }
    load();
  };

  // ──────────────────────────────────────────
  // Task actions
  // ──────────────────────────────────────────

  // New Task is page-level: no category anchor. The title matcher files the
  // task onto a list (0 / many / untracked matches → the modal shows the
  // server's 400 message).
  const openCreateTask = () => {
    setTaskForm({ mode: 'create' });
    setFormError(null);
  };

  const openEditTask = (task: TaskRecord) => {
    setTaskForm({ mode: 'edit', task });
    setFormError(null);
  };

  const closeTaskForm = () => {
    setTaskForm(null);
  };

  /** TaskModal's immediate status change — the same /move contract as the
   *  board: no drop position, so `sort_order` is OMITTED and the server
   *  applies the column default (`defaultMoveRank` mirrors it for the
   *  optimistic frame — the moved card to its default rank, the parked
   *  runner to 0), then one POST /api/tasks/:id/move. Failure with a
   *  `displaced` task keeps A parked and snaps only this task back; any
   *  other failure restores the full snapshot. Sets the banner and returns
   *  the message for the form (null on success). */
  const handleMoveTask = async (
    taskId: string,
    status: TaskStatus,
    displace?: MoveDisplaceInput,
  ): Promise<string | null> => {
    setActionError(null);
    const snapshot = tasksRef.current;
    const current = snapshot.find((entry) => entry.id === taskId);
    const destRank = defaultMoveRank(
      current?.status ?? status,
      status,
      snapshot,
      taskId,
    );
    // No-drop: omit `displace.sort_order` too — the park always prepends.
    const park = displace
      ? { id: displace.id, status: displace.status }
      : undefined;
    setTasks((prev) =>
      prev.map((entry) => {
        if (displace && entry.id === displace.id) {
          return {
            ...entry,
            status: displace.status,
            sort_order: 0,
          };
        }
        return entry.id === taskId
          ? { ...entry, status, sort_order: destRank }
          : entry;
      }),
    );
    const body: MoveTaskInput = { status, displace: park };
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
          // the moved task goes back to where it was before.
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
      const data = (await res.json()) as MoveTaskResponse;
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
      return null;
    } catch (err) {
      setTasks(snapshot);
      const message = err instanceof Error ? err.message : 'Move failed';
      setActionError(message);
      return message;
    }
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
    // card regroups instantly (the task lands on the card whose
    // `inherited_list_id` matches; an untracked result stays hidden, which is
    // correct on this page).
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
      <div className="max-w-7xl mx-auto px-6 py-8">
        {/* Header */}
        <header className="flex items-center justify-between mb-8">
          <div>
            <h1 className="font-heading text-3xl font-bold text-foreground mb-2">
              My Lists
            </h1>
            <p className="text-muted-foreground">
              Tasks are filed into lists by their title
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
            <Button
              onClick={openCreateList}
              className="bg-sanctuary-green hover:bg-sanctuary-green/90"
            >
              <Plus className="h-4 w-4 mr-2" />
              New List
            </Button>
          </div>
        </header>

        {/* Load error banner — replaces the grid only when there are no lists
            to show; with cards on screen it reads as a refresh-failure notice
            above the still-visible grid */}
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

        {/* Action error banner (e.g. a delete 409) — the grid stays mounted */}
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

        {/* Loading — only while the grid is empty (first load or a retry after
            a hard error); reloads with cards on screen never flash this */}
        {isLoading && lists.length === 0 && (
          <div className="flex items-center justify-center py-24 gap-2 text-muted-foreground">
            <Loader2 className="h-5 w-5 animate-spin" />
            Loading lists…
          </div>
        )}

        {/* Lists Grid — hidden only when there is nothing to show (first load
            in flight, or a load error that replaced the cards). Action errors
            never unmount the grid. */}
        {(lists.length > 0 || (!isLoading && !loadError)) && (
          <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-6">
            {lists.map((list) => (
              <ListCard
                key={list.id}
                list={list}
                tasks={tasks}
                onEditList={openEditList}
                onDeleteList={handleDeleteList}
                onEditTask={openEditTask}
                anyRunning={anyRunning}
                onStartTask={handleStartTask}
                onStopTask={handleStopTask}
                onPauseTask={handlePauseTask}
                onCompleteTask={handleCompleteTask}
                onDiscardTask={handleDiscardTask}
              />
            ))}
          </div>
        )}
      </div>

      {/* New / Edit List Dialog */}
      <Dialog
        open={listForm !== null}
        onOpenChange={(open) => !open && closeListForm()}
      >
        <DialogContent className="sm:max-w-[420px] p-0 gap-0 overflow-hidden bg-card border-border">
          <div className="h-2" style={{ backgroundColor: listColor }} />
          <div className="p-6">
            <DialogHeader className="mb-6">
              <DialogTitle className="text-foreground">
                {listForm?.mode === 'edit' ? 'Edit List' : 'New List'}
              </DialogTitle>
              <DialogDescription>
                {listForm?.mode === 'edit'
                  ? 'Update the name or color of your list.'
                  : 'Create a new list to organize your tasks.'}
              </DialogDescription>
            </DialogHeader>

            <div className="space-y-2 mb-5">
              <label className="text-sm font-medium text-foreground">
                Name
              </label>
              <input
                type="text"
                value={listName}
                onChange={(e) => setListName(e.target.value)}
                placeholder="e.g. Work"
                className="w-full px-4 py-3 rounded-xl border border-input bg-background text-foreground placeholder:text-muted-foreground/60 focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all"
              />
            </div>

            <div className="space-y-2 mb-6">
              <label className="text-sm font-medium text-foreground">
                Color
              </label>
              <div className="flex items-center gap-3">
                <input
                  type="color"
                  value={listColor}
                  onChange={(e) => setListColor(e.target.value)}
                  className="h-10 w-14 rounded-lg border border-input bg-background cursor-pointer"
                  aria-label="List color"
                />
                <span className="text-sm text-muted-foreground font-mono">
                  {listColor}
                </span>
              </div>
            </div>

            {formError && (
              <p className="mb-4 text-sm text-destructive">{formError}</p>
            )}

            <div className="flex justify-end gap-3">
              <Button
                variant="outline"
                onClick={closeListForm}
                className="border-input text-foreground hover:bg-muted"
              >
                Cancel
              </Button>
              <Button
                onClick={handleListSubmit}
                disabled={!listName.trim() || saving}
                className="bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
              >
                {saving && <Loader2 className="h-4 w-4 mr-2 animate-spin" />}
                {listForm?.mode === 'edit' ? 'Save Changes' : 'Create List'}
              </Button>
            </div>
          </div>
        </DialogContent>
      </Dialog>

      {/* New / Edit Task Dialog — edit mode carries the immediate status
          pills (onMove) and the running task for the park dialog */}
      <TaskModal
        open={taskForm !== null}
        onOpenChange={(open) => !open && closeTaskForm()}
        task={taskForm?.mode === 'edit' ? taskForm.task : undefined}
        onSubmit={handleTaskSubmit}
        onDelete={handleTaskDelete}
        onMove={handleMoveTask}
        runningTask={runningTask}
      />
    </div>
  );
}

interface ListCardProps {
  list: TaskList;
  tasks: TaskRecord[];
  onEditList: (list: TaskList) => void;
  onDeleteList: (list: TaskList) => void;
  onEditTask: (task: TaskRecord) => void;
  anyRunning: boolean;
  onStartTask: (taskId: string) => void;
  onStopTask: (taskId: string) => void;
  onPauseTask: (taskId: string) => void;
  onCompleteTask: (taskId: string) => void;
  onDiscardTask: (taskId: string) => void;
}

function ListCard({
  list,
  tasks,
  onEditList,
  onDeleteList,
  onEditTask,
  anyRunning,
  onStartTask,
  onStopTask,
  onPauseTask,
  onCompleteTask,
  onDiscardTask,
}: ListCardProps) {
  const [menuOpen, setMenuOpen] = useState(false);

  // Flat, category-blind pile: a tracked task lands here when its computed
  // category inherits this list. Untracked tasks are hidden on this page.
  const listTasks = tasks.filter(
    (task) =>
      !task.category.is_untracked &&
      task.category.inherited_list_id === list.id,
  );

  return (
    <div
      className="rounded-xl overflow-hidden"
      style={{ backgroundColor: list.color }}
    >
      <div className="p-4">
        <div className="flex items-center justify-between mb-4 gap-2">
          <h3 className="font-heading text-lg font-semibold text-primary-foreground truncate">
            {list.name}
          </h3>
          <div className="relative flex-shrink-0">
            <button
              onClick={() => setMenuOpen((open) => !open)}
              className="p-1 rounded-md hover:bg-primary-foreground/10 transition-colors"
              aria-label={`Options for ${list.name}`}
            >
              <MoreHorizontal className="h-5 w-5 text-primary-foreground/70" />
            </button>
            {menuOpen && (
              <>
                {/* Invisible backdrop to close the menu on outside click */}
                <div
                  className="fixed inset-0 z-10"
                  onClick={() => setMenuOpen(false)}
                />
                <div className="absolute right-0 top-9 z-20 w-36 bg-popover rounded-lg shadow-lg border border-border py-1">
                  <button
                    onClick={() => {
                      setMenuOpen(false);
                      onEditList(list);
                    }}
                    className="flex w-full items-center gap-2 px-3 py-2 text-sm text-foreground hover:bg-muted transition-colors"
                  >
                    <Pencil className="h-4 w-4" />
                    Edit
                  </button>
                  <button
                    onClick={() => {
                      setMenuOpen(false);
                      onDeleteList(list);
                    }}
                    className="flex w-full items-center gap-2 px-3 py-2 text-sm text-destructive hover:bg-destructive/10 transition-colors"
                  >
                    <Trash2 className="h-4 w-4" />
                    Delete
                  </button>
                </div>
              </>
            )}
          </div>
        </div>
      </div>

      {/* Flat task pile — no categories; tasks land here by their computed
          category's inherited list */}
      {listTasks.length > 0 && (
        <div className="bg-black/20 p-3 space-y-1.5">
          {listTasks.map((task) => (
            <TaskChip
              key={task.id}
              task={task}
              onEdit={onEditTask}
              anyRunning={anyRunning}
              onStart={onStartTask}
              onStop={onStopTask}
              onPause={onPauseTask}
              onComplete={onCompleteTask}
              onDiscard={onDiscardTask}
            />
          ))}
        </div>
      )}

      {/* Empty state — no tracked task matches this list */}
      {listTasks.length === 0 && (
        <div className="bg-black/20 p-4 border-t border-black/10">
          <p className="text-sm text-primary-foreground/70 italic">
            No tasks yet
          </p>
        </div>
      )}
    </div>
  );
}

/** One task row: title + duration + status badge + timer actions. Clicking
 *  the row (not a button) opens the edit modal; every action button stops
 *  propagation so it never opens the modal. */
function TaskChip({
  task,
  onEdit,
  anyRunning,
  onStart,
  onStop,
  onPause,
  onComplete,
  onDiscard,
}: {
  task: TaskRecord;
  onEdit: (task: TaskRecord) => void;
  anyRunning: boolean;
  onStart: (taskId: string) => void;
  onStop: (taskId: string) => void;
  onPause: (taskId: string) => void;
  onComplete: (taskId: string) => void;
  onDiscard: (taskId: string) => void;
}) {
  const isRunning = task.status === 'IN_PROGRESS';
  return (
    <div
      onClick={() => onEdit(task)}
      className="flex w-full cursor-pointer items-center gap-1.5 rounded-lg bg-black/25 px-2.5 py-1.5 text-left hover:bg-black/35 transition-colors"
      title={`${task.title} — ${task.duration_minutes} min, ${task.priority}${
        task.difficulty !== 'easy' ? `, ${task.difficulty}` : ''
      }`}
    >
      <span className="flex-1 min-w-0 text-xs text-primary-foreground/90 truncate">
        {task.title}
      </span>
      <span className="flex-shrink-0 text-[10px] text-primary-foreground/60">
        {task.duration_minutes} min
      </span>
      {task.difficulty === 'hard' || task.difficulty === 'medium' ? (
        <span
          className={cn(
            'flex-shrink-0 rounded-full px-1.5 py-0.5 text-[9px] uppercase tracking-wide',
            task.difficulty === 'hard'
              ? 'bg-primary-foreground/25 font-semibold text-primary-foreground/90'
              : 'bg-primary-foreground/15 font-medium text-primary-foreground/70',
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
      {/* Status badge — only for states that changed the task's look */}
      {isRunning && (
        <span className="flex-shrink-0 rounded-full bg-emerald-400/30 px-1.5 py-0.5 text-[9px] font-semibold uppercase tracking-wide text-emerald-100">
          running
        </span>
      )}
      {task.status === 'COMPLETED' && (
        <span className="flex-shrink-0 rounded-full bg-sky-400/30 px-1.5 py-0.5 text-[9px] font-semibold uppercase tracking-wide text-sky-100">
          done
        </span>
      )}
      {task.status === 'DISCARDED' && (
        <span className="flex-shrink-0 rounded-full bg-rose-400/30 px-1.5 py-0.5 text-[9px] font-semibold uppercase tracking-wide text-rose-100">
          discarded
        </span>
      )}
      {/* Actions — Start on OPEN and PLANNED (pause parks tasks in PLANNED,
          so the Lists page must be able to restart them) and only when the
          running slot is free; Stop + Pause only while this task runs;
          Complete/Discard whenever the task is not already in that terminal
          state */}
      {(task.status === 'OPEN' || task.status === 'PLANNED') && (
        <button
          onClick={(e) => {
            e.stopPropagation();
            onStart(task.id);
          }}
          disabled={anyRunning}
          className={cn(
            'flex-shrink-0 rounded-md p-1 text-primary-foreground/70 transition-colors hover:bg-primary-foreground/15 hover:text-primary-foreground',
            anyRunning &&
              'cursor-not-allowed opacity-40 hover:bg-transparent hover:text-primary-foreground/70',
          )}
          aria-label={`Start ${task.title}`}
          title={anyRunning ? 'Another task is running' : 'Start'}
        >
          <Play className="h-3 w-3" />
        </button>
      )}
      {isRunning && (
        <>
          <button
            onClick={(e) => {
              e.stopPropagation();
              onStop(task.id);
            }}
            className="flex-shrink-0 rounded-md p-1 text-primary-foreground/70 transition-colors hover:bg-primary-foreground/15 hover:text-primary-foreground"
            aria-label={`Stop ${task.title}`}
            title="Stop"
          >
            <Square className="h-3 w-3" />
          </button>
          <button
            onClick={(e) => {
              e.stopPropagation();
              onPause(task.id);
            }}
            className="flex-shrink-0 rounded-md p-1 text-primary-foreground/70 transition-colors hover:bg-primary-foreground/15 hover:text-primary-foreground"
            aria-label={`Pause ${task.title}`}
            title="Pause"
          >
            <Pause className="h-3 w-3" />
          </button>
        </>
      )}
      {task.status !== 'COMPLETED' && (
        <button
          onClick={(e) => {
            e.stopPropagation();
            onComplete(task.id);
          }}
          className="flex-shrink-0 rounded-md p-1 text-primary-foreground/70 transition-colors hover:bg-primary-foreground/15 hover:text-primary-foreground"
          aria-label={`Complete ${task.title}`}
          title="Complete"
        >
          <Check className="h-3 w-3" />
        </button>
      )}
      {task.status !== 'DISCARDED' && (
        <button
          onClick={(e) => {
            e.stopPropagation();
            onDiscard(task.id);
          }}
          className="flex-shrink-0 rounded-md p-1 text-primary-foreground/70 transition-colors hover:bg-primary-foreground/15 hover:text-primary-foreground"
          aria-label={`Discard ${task.title}`}
          title="Discard"
        >
          <X className="h-3 w-3" />
        </button>
      )}
    </div>
  );
}
