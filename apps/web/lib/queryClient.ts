import { QueryClient } from '@tanstack/react-query';

/**
 * Singleton QueryClient with locked defaults.
 *
 * These defaults replace the hand-rolled board 60s + visibility refresh in a
 * later slice; `refetchInterval` stays per-query (Board only) and is not set
 * globally here.
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