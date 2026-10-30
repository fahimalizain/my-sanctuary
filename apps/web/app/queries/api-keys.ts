import { queryOptions, useMutation, useQuery } from '@tanstack/react-query';
import { createApiKey, listApiKeys, revokeApiKey } from '@/lib/api';
import { queryKeys } from './keys';
import { queryClient } from '@/lib/queryClient';

// `apiKeys.all` is the only key: a revoke removes a row and a create adds
// one, so every write invalidates and re-reads rather than patching the
// cache. The create response must never reach that cache — it carries the
// one-time `secret` — hence the mutation's `gcTime: 0`.

export function apiKeysQueryOptions() {
  return queryOptions({
    queryKey: queryKeys.apiKeys.all,
    queryFn: listApiKeys,
  });
}

export function useApiKeysQuery(opts?: { enabled?: boolean }) {
  return useQuery({
    ...apiKeysQueryOptions(),
    enabled: opts?.enabled ?? true,
  });
}

export function useCreateApiKey() {
  return useMutation({
    mutationFn: createApiKey,
    // Nothing from this response is worth keeping: the caller reveals
    // `secret` once and the list refetches the new row without it.
    gcTime: 0,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.apiKeys.all });
    },
  });
}

export function useRevokeApiKey() {
  return useMutation({
    mutationFn: (id: string) => revokeApiKey(id),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.apiKeys.all });
    },
  });
}
