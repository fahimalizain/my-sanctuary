import { queryOptions, useMutation, useQuery } from '@tanstack/react-query';
import { getMe, logout } from '@/lib/api';
import { queryKeys } from './keys';
import { queryClient } from '@/lib/queryClient';

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
    onSettled: () => {
      queryClient.clear();
      // Re-seed so AuthGuard does not flash a loading refetch of /auth/me
      // and so a failed logout HTTP still matches today's "always clear
      // local session" behavior.
      queryClient.setQueryData(queryKeys.auth.me(), { user: null });
    },
  });
}