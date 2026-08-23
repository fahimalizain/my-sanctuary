import { queryOptions, useQuery } from '@tanstack/react-query';
import { listTasks } from '@/lib/api';
import type { TaskRecord, TasksResponse } from '@/app/types';
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

export function useTasksQuery(opts?: { enabled?: boolean }) {
  return useQuery({
    ...tasksQueryOptions(),
    enabled: opts?.enabled ?? true,
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