import { queryOptions, useMutation, useQuery } from '@tanstack/react-query';
import { getMe, logout } from '@/lib/api';
import { queryKeys } from './keys';
import { queryClient } from '@/lib/queryClient';
import { stopBackgroundSync } from '@/app/background-sync/start-background-sync';

// Session through TanStack Query: `['auth', 'me']` is owned by this module.
// Logout clears the entire cache (`queryClient.clear()`) so the next session
// can never see the previous user's lists/tasks/agenda.

export function meQueryOptions() {
  return queryOptions({
    queryKey: queryKeys.auth.me(),
    queryFn: getMe,
  });
}

export function useMeQuery() {
  return useQuery(meQueryOptions());
}

export function useLogout() {
  return useMutation({
    mutationFn: logout,
    // Destroy the background observers BEFORE `onSettled`'s
    // `queryClient.clear()`. `clear()` notifies mounted observers, which
    // refetch and can repopulate the cache after the clear — and `user` only
    // flips to null in `onSettled`, so unmounting on that is too late.
    onMutate: () => {
      stopBackgroundSync();
    },
    onSettled: () => {
      queryClient.clear();
      // Re-seed so AuthGuard does not flash a loading refetch of /auth/me
      // and so a failed logout HTTP still matches today's "always clear
      // local session" behavior.
      queryClient.setQueryData(queryKeys.auth.me(), { user: null });
    },
  });
}
