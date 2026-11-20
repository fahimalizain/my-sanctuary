import { queryOptions, useQuery } from '@tanstack/react-query';
import { getPlannedBoard } from '@/lib/api';
import type { PlannedBoardResponse } from '@/app/types';
import { queryKeys } from './keys';
import { queryClient } from '@/lib/queryClient';

// The Board's date-grouped Planned column (ADR 0014). The key lives under
// `agenda.all` on purpose: every task/agenda mutation already invalidates that
// prefix, so this read refreshes with Home's stream instead of needing its own
// invalidation list. React-free `queryOptions` factory + hook, like tasks.ts.

export function boardPlannedQueryOptions() {
  return queryOptions({
    queryKey: queryKeys.agenda.boardPlanned(),
    queryFn: getPlannedBoard,
  });
}

export function usePlannedBoardQuery(opts?: {
  enabled?: boolean;
  /** The Board pauses this while a drag or move is in flight, so a focus
   *  refetch cannot overwrite the optimistic Planned rows mid-write. */
  refetchOnWindowFocus?: boolean;
}) {
  return useQuery({
    ...boardPlannedQueryOptions(),
    enabled: opts?.enabled ?? true,
    refetchOnWindowFocus: opts?.refetchOnWindowFocus,
  });
}

/** The cached `GET /api/board/planned` payload, or undefined before the first
 *  read resolves. */
export function readPlannedBoardCache(): PlannedBoardResponse | undefined {
  return queryClient.getQueryData<PlannedBoardResponse>(
    queryKeys.agenda.boardPlanned(),
  );
}

/** Writes the Planned Board cache. Accepts a whole payload or an updater over
 *  the current one; the updater receives `undefined` when nothing is cached
 *  and may return it to leave the cache empty (e.g. a write arriving before
 *  the first read). */
export function setPlannedBoardCache(
  updater:
    | PlannedBoardResponse
    | ((
        prev: PlannedBoardResponse | undefined,
      ) => PlannedBoardResponse | undefined),
): void {
  queryClient.setQueryData<PlannedBoardResponse>(
    queryKeys.agenda.boardPlanned(),
    (old) => (typeof updater === 'function' ? updater(old) : updater),
  );
}
