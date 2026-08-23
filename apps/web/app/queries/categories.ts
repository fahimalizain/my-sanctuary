import { queryOptions, useMutation, useQuery } from '@tanstack/react-query';
import {
  createCategory,
  deleteCategory,
  listCategories,
  updateCategory,
} from '@/lib/api';
import type { UpdateCategoryInput } from '@/app/types';
import { queryKeys } from './keys';
import { queryClient } from '@/lib/queryClient';

// Same split as lists.ts: React-free `queryOptions` factory + hooks.

export function categoriesQueryOptions() {
  return queryOptions({
    queryKey: queryKeys.categories.all,
    queryFn: listCategories,
  });
}

export function useCategoriesQuery(opts?: { enabled?: boolean }) {
  return useQuery({
    ...categoriesQueryOptions(),
    enabled: opts?.enabled ?? true,
  });
}

export function useCreateCategory() {
  return useMutation({
    mutationFn: createCategory,
    onSuccess: () => {
      void queryClient.invalidateQueries({
        queryKey: queryKeys.categories.all,
      });
    },
  });
}

export function useUpdateCategory() {
  return useMutation({
    mutationFn: ({ id, input }: { id: string; input: UpdateCategoryInput }) =>
      updateCategory(id, input),
    onSuccess: () => {
      void queryClient.invalidateQueries({
        queryKey: queryKeys.categories.all,
      });
    },
  });
}

export function useDeleteCategory() {
  return useMutation({
    mutationFn: (id: string) => deleteCategory(id),
    onSuccess: () => {
      void queryClient.invalidateQueries({
        queryKey: queryKeys.categories.all,
      });
    },
  });
}