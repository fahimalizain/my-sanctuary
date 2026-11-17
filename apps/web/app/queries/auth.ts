import { queryOptions, useMutation, useQuery } from '@tanstack/react-query';
import {
  getMe,
  getProviders,
  loginStandalone,
  logout,
  registerStandalone,
  type StandaloneLoginInput,
  type StandaloneRegisterInput,
} from '@/lib/api';
import { queryKeys } from './keys';
import { queryClient } from '@/lib/queryClient';
import { stopBackgroundSync } from '@/app/background-sync/start-background-sync';

// Session through TanStack Query: `['auth', 'me']` is owned by this module.
// Logout clears the entire cache (`queryClient.clear()`) so the next session
// can never see the previous user's lists/tasks/agenda. The standalone
// mutations instead *seed* `me` and leave the cache alone — the login page
// full-redirects afterwards, which resets it anyway.

export function meQueryOptions() {
  return queryOptions({
    queryKey: queryKeys.auth.me(),
    queryFn: getMe,
  });
}

export function useMeQuery() {
  return useQuery(meQueryOptions());
}

export function providersQueryOptions() {
  return queryOptions({
    queryKey: queryKeys.auth.providers(),
    queryFn: getProviders,
    // The answer is cheap and request-scoped (env flag + host); a stale
    // "standalone: false" after a local restart would hide the form.
    staleTime: 0,
  });
}

export function useProvidersQuery() {
  return useQuery(providersQueryOptions());
}

// Both mutations resolve to the signed-in user (not the `{ user }` envelope)
// and seed `['auth', 'me']` before resolving, so anything reading `useAuth()`
// between the click and the redirect already sees the new session.

export function useStandaloneLogin() {
  return useMutation({
    mutationFn: async (input: StandaloneLoginInput) => {
      const { user } = await loginStandalone(input);
      return user;
    },
    onSuccess: (user) => {
      queryClient.setQueryData(queryKeys.auth.me(), { user });
    },
  });
}

export function useStandaloneRegister() {
  return useMutation({
    mutationFn: async (input: StandaloneRegisterInput) => {
      const { user } = await registerStandalone(input);
      return user;
    },
    onSuccess: (user) => {
      queryClient.setQueryData(queryKeys.auth.me(), { user });
    },
  });
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
