import { queryOptions, useMutation, useQuery } from '@tanstack/react-query';
import {
  createRoutine,
  deleteRoutine,
  listRoutines,
  updateRoutine,
} from '@/lib/api';
import type { UpdateRoutineInput } from '@/app/types';
import { queryKeys } from './keys';
import { queryClient } from '@/lib/queryClient';

// `queryOptions` keeps the factory React-free so the query key/queryFn pair
// can be reused and tested without a component. Hooks below wrap it.

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
    },
  });
}

export function useUpdateRoutine() {
  return useMutation({
    mutationFn: ({ id, input }: { id: string; input: UpdateRoutineInput }) =>
      updateRoutine(id, input),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.routines.all });
    },
  });
}

export function useDeleteRoutine() {
  return useMutation({
    mutationFn: (id: string) => deleteRoutine(id),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.routines.all });
    },
  });
}