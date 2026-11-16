import { queryOptions, useMutation, useQuery } from '@tanstack/react-query';
import {
  createRoutine,
  deleteRoutine,
  listRoutines,
  reorderRoutines,
  updateRoutine,
} from '@/lib/api';
import type { RoutinesResponse, UpdateRoutineInput } from '@/app/types';
import { routinesInIdOrder } from '@/app/modules/routines/reorder';
import { queryKeys } from './keys';
import { queryClient } from '@/lib/queryClient';

// `queryOptions` keeps the factory React-free so the query key/queryFn pair
// can be reused and tested without a component. Hooks below wrap it.
//
// Routine writes also invalidate Home's `['agenda']` day-stream: creating,
// retitling or deleting a routine changes the auto-seeded occurrences, and a
// delete turns their history read-only — the optimistic agenda paint cannot
// know any of that, so the stream re-reads.
//
// The one exception is reorder: it moves standing `sort_order` only, and the
// materialized agenda rows keep the order they were seeded with (ADR 0004), so
// that hook invalidates `['routines']` alone.

export function routinesQueryOptions() {
  return queryOptions({
    queryKey: queryKeys.routines.all,
    queryFn: listRoutines,
  });
}

export function useRoutinesQuery(opts?: { enabled?: boolean }) {
  return useQuery({
    ...routinesQueryOptions(),
    enabled: opts?.enabled ?? true,
  });
}

export function useCreateRoutine() {
  return useMutation({
    mutationFn: createRoutine,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.routines.all });
      void queryClient.invalidateQueries({ queryKey: queryKeys.agenda.all });
    },
  });
}

export function useUpdateRoutine() {
  return useMutation({
    mutationFn: ({ id, input }: { id: string; input: UpdateRoutineInput }) =>
      updateRoutine(id, input),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.routines.all });
      void queryClient.invalidateQueries({ queryKey: queryKeys.agenda.all });
    },
  });
}

export function useDeleteRoutine() {
  return useMutation({
    mutationFn: (id: string) => deleteRoutine(id),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.routines.all });
      void queryClient.invalidateQueries({ queryKey: queryKeys.agenda.all });
    },
  });
}

// Standing-order reorder: `['routines']` only. Unlike create/update/delete, it
// does NOT invalidate `['agenda']` — already materialized Home rows keep their
// seeded order (ADR 0004), so a stream re-read would change nothing.
export function useReorderRoutines() {
  return useMutation({
    mutationFn: (ids: string[]) => reorderRoutines(ids),
    onMutate: async (ids) => {
      await queryClient.cancelQueries({ queryKey: queryKeys.routines.all });
      const previous = queryClient.getQueryData<RoutinesResponse>(
        queryKeys.routines.all,
      );
      if (previous) {
        queryClient.setQueryData<RoutinesResponse>(queryKeys.routines.all, {
          routines: routinesInIdOrder(previous.routines, ids),
        });
      }
      return { previous };
    },
    onError: (_err, _ids, context) => {
      if (context?.previous) {
        queryClient.setQueryData(queryKeys.routines.all, context.previous);
      }
    },
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.routines.all });
    },
  });
}
