import { queryOptions, useMutation, useQuery } from '@tanstack/react-query';
import {
  createList,
  deleteList,
  listLists,
  updateList,
} from '@/lib/api';
import type { UpdateListInput } from '@/app/types';
import { queryKeys } from './keys';
import { queryClient } from '@/lib/queryClient';

// `queryOptions` keeps the factory React-free so the query key/queryFn pair
// can be reused and tested without a component. Hooks below wrap it.

export function listsQueryOptions() {
  return queryOptions({
    queryKey: queryKeys.lists.all,
    queryFn: listLists,
  });
}

export function useListsQuery(opts?: { enabled?: boolean }) {
  return useQuery({
    ...listsQueryOptions(),
    enabled: opts?.enabled ?? true,
  });
}

export function useCreateList() {
  return useMutation({
    mutationFn: createList,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.lists.all });
    },
  });
}

export function useUpdateList() {
  return useMutation({
    mutationFn: ({ id, input }: { id: string; input: UpdateListInput }) =>
      updateList(id, input),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.lists.all });
    },
  });
}

export function useDeleteList() {
  return useMutation({
    mutationFn: (id: string) => deleteList(id),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.lists.all });
    },
  });
}