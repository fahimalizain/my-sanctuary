import { QueryClient } from '@tanstack/react-query';

/**
 * Singleton QueryClient with locked defaults.
 *
 * `refetchInterval` is per-query (Board only) and is not set globally.
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
