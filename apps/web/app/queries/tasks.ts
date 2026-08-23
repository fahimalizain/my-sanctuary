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

export function tasksQueryOptions() {
  return queryOptions({
    queryKey: queryKeys.tasks.all,
    queryFn: listTasks,
  });
}

export function useTasksQuery(opts?: {
  enabled?: boolean;
  refetchInterval?: number | false;
  refetchOnWindowFocus?: boolean;
}) {
  return useQuery({
    ...tasksQueryOptions(),
    enabled: opts?.enabled ?? true,
    refetchInterval: opts?.refetchInterval,
    refetchOnWindowFocus: opts?.refetchOnWindowFocus,
  });
}

/** Drop-in replacement for `setTasks` / `setTasks(prev => …)`.
 *  Writes `queryKeys.tasks.all` as `{ tasks }`. */
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
// ──────────────────────────────────────────

async function cancelTasksQuery(): Promise<void> {
  await queryClient.cancelQueries({ queryKey: queryKeys.tasks.all });
}

export function useMoveTask() {
  return useMutation({
    mutationFn: ({ id, input }: { id: string; input: MoveTaskInput }) =>
      moveTask(id, input),
    onMutate: cancelTasksQuery,
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
  });
}

export function useUpdateTask() {
  return useMutation({
    mutationFn: ({ id, input }: { id: string; input: UpdateTaskInput }) =>
      updateTask(id, input),
    onMutate: cancelTasksQuery,
  });
}

export function useDeleteTask() {
  return useMutation({
    mutationFn: (id: string) => deleteTask(id),
    onMutate: cancelTasksQuery,
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
  });
}
