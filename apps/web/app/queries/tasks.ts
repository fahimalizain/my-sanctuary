import { queryOptions, useMutation, useQuery } from '@tanstack/react-query';
import {
  createTask,
  deleteTask,
  focusTask,
  listTasks,
  moveTask,
  runTaskAction,
  unfocusTask,
  updateTask,
} from '@/lib/api';
import type {
  MoveTaskInput,
  NewTaskInput,
  TaskRecord,
  TasksResponse,
  UpdateTaskInput,
} from '@/app/types';
import { queryKeys } from './keys';
import { queryClient } from '@/lib/queryClient';

// Same split as lists.ts: React-free `queryOptions` factory + hooks. The one
// `['tasks']` cache is shared by Board, Lists and the Home task picker.

/** Background refetch while the board is idle. Background sync owns the
 *  interval on its always-on tasks observer and pauses it while the board is
 *  busy (`setTasksBusy`), so Board must not also pass an interval. */
export const TASKS_REFETCH_INTERVAL_MS = 60_000;

export function tasksQueryOptions() {
  return queryOptions({
    queryKey: queryKeys.tasks.all,
    queryFn: listTasks,
  });
}

export function useTasksQuery(opts?: {
  enabled?: boolean;
  refetchOnWindowFocus?: boolean;
}) {
  return useQuery({
    ...tasksQueryOptions(),
    enabled: opts?.enabled ?? true,
    refetchOnWindowFocus: opts?.refetchOnWindowFocus,
  });
}

/** Writes `queryKeys.tasks.all` as `{ tasks }`. Accepts an array or an
 *  updater over the current list. */
export function setTasksCache(
  updater: TaskRecord[] | ((prev: TaskRecord[]) => TaskRecord[]),
): void {
  queryClient.setQueryData<TasksResponse>(queryKeys.tasks.all, (old) => {
    const prev = old?.tasks ?? [];
    const next = typeof updater === 'function' ? updater(prev) : updater;
    return { tasks: next };
  });
}

// ──────────────────────────────────────────
// Task write mutations — thin wrappers, no optimistic paint, no invalidation.
// Every one cancels the shared `['tasks']` query on `onMutate` so an in-flight
// refetch (interval tick, window focus, another tab's mutation) can never
// resolve over the page's optimistic cache mid-write. The pages own the
// optimistic paint (`setTasksCache` / `applyOptimisticMove`) and merge the
// authoritative row on success — `invalidateQueries(['tasks'])` on success
// would fight applyOptimisticMove's sibling-rank contract, so it is never
// used here.
//
// `['agenda']` is a different story: a task's ONE agenda row follows the
// task's status (ADR 0007), so Home's day-stream must re-read after every
// write that can move it — the page's optimistic paint cannot know which way
// the row travelled. Create, unplan (`/stop`, a Board drag to Open, or a
// reopen to `OPEN`) park the row on the **backlog date** (today+50y); reopen
// to `PLANNED`, start, complete and discard put it on civil today. Plan
// (`OPEN → PLANNED`) is the ADR 0009 exception: it parks the row on the
// backlog sentinel too (unless the row already sits at/after the 10-year
// horizon), because planning a task must never invent a civil day the user
// did not choose. `/pause` alone leaves the row where it is, so its re-read is
// redundant — never wrong.
// ──────────────────────────────────────────

async function cancelTasksQuery(): Promise<void> {
  await queryClient.cancelQueries({ queryKey: queryKeys.tasks.all });
}

/** Re-reads Home's agenda stream after a task write that can repoint the
 *  task's single agenda row (and, for a rename, change the title Home embeds).
 *  Fired WITHOUT returning a promise: `mutateAsync` must not wait out every
 *  loaded page's refetch, and on failure the caller's rollback must not run
 *  after — and overwrite — the fresh server data. Never invalidates
 *  `['tasks']` (see above). */
function invalidateAgendaQuery(): void {
  void queryClient.invalidateQueries({ queryKey: queryKeys.agenda.all });
}

export function useMoveTask() {
  return useMutation({
    mutationFn: ({ id, input }: { id: string; input: MoveTaskInput }) =>
      moveTask(id, input),
    onMutate: cancelTasksQuery,
    onSettled: invalidateAgendaQuery,
  });
}

export function useFocusTask() {
  return useMutation({
    mutationFn: (id: string) => focusTask(id),
    onMutate: cancelTasksQuery,
  });
}

export function useUnfocusTask() {
  return useMutation({
    mutationFn: () => unfocusTask(),
    onMutate: cancelTasksQuery,
  });
}

export function useCreateTask() {
  return useMutation({
    mutationFn: (input: NewTaskInput) => createTask(input),
    onMutate: cancelTasksQuery,
    onSettled: invalidateAgendaQuery,
  });
}

export function useUpdateTask() {
  return useMutation({
    mutationFn: ({ id, input }: { id: string; input: UpdateTaskInput }) =>
      updateTask(id, input),
    onMutate: cancelTasksQuery,
    // A Board rename changes the title Home embeds — re-read the stream so
    // the agenda row does not show the old one until `staleTime` expires.
    onSettled: invalidateAgendaQuery,
  });
}

export function useDeleteTask() {
  return useMutation({
    mutationFn: (id: string) => deleteTask(id),
    onMutate: cancelTasksQuery,
    onSettled: invalidateAgendaQuery,
  });
}

export function useRunTaskAction() {
  return useMutation({
    mutationFn: ({
      id,
      action,
    }: {
      id: string;
      action: 'start' | 'stop' | 'pause' | 'complete' | 'discard';
    }) => runTaskAction(id, action),
    onMutate: cancelTasksQuery,
    onSettled: invalidateAgendaQuery,
  });
}
