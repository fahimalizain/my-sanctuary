import { QueryClient } from '@tanstack/react-query';

/**
 * Singleton QueryClient with locked defaults.
 *
 * `refetchInterval` is per-query (background sync's always-on tasks observer)
 * and is not set globally.
 */
export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      refetchOnWindowFocus: true,
      refetchOnReconnect: true,
      retry: 1,
    },
  },
});

/** Inactive range/date caches (every calendar events window, agenda history)
 *  outlive the default 5 min so scrolling back or reopening a loaded week is
 *  still a hit. Do not put this on the client defaults — lists/tasks/agenda
 *  current are not range-keyed. gcTime only runs once the last observer leaves. */
export const RANGE_QUERY_GC_TIME_MS = 30 * 60 * 1000;
