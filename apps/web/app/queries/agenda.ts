import { useRef } from 'react';
import { useInfiniteQuery, useMutation } from '@tanstack/react-query';
import type { InfiniteData } from '@tanstack/react-query';
import {
  getAgendaRange,
  moveAgendaItem,
  rescheduleAgendaItem,
} from '@/lib/api';
import type {
  AgendaResponse,
  MoveAgendaItemInput,
  RescheduleAgendaItemInput,
} from '@/app/types';
import { queryKeys } from './keys';
import { queryClient } from '@/lib/queryClient';
import {
  nextStreamPageParam,
  previousHistoryWindow,
} from '@/app/modules/home/agenda-helpers';

// Home reads ONE infinite stream of day windows (ADR 0004 amendment — slice
// 5, ADR 0007 — slice 6): page 0 is the server's default read (no query at
// all, so the browser never computes today) — `anchor_date … today+6` plus the
// `backlog` array. `getPreviousPageParam` walks history in 7-day windows down
// to `earliest_date`, and `getNextPageParam` walks FORWARD from page 0 —
// TanStack re-runs an infinite query page by page from its stored params, so
// every page must be able to name the one after it (see the param union below)
// or the walk stops and today's page drops out of the cache. Every write on
// the page edits this cache in place (optimistic paint) and `cancelAgendaQuery`
// protects it from an in-flight refetch — there is no per-date key left.

/** One stream page's param — the two reads `getAgendaRange` answers to: the
 *  DEFAULT page (`{ kind: 'default' }` — the GET omits `from`/`to` entirely)
 *  and a 7-day HISTORY window (`to < anchor_date`). The union exists because a
 *  refetch walks FORWARD from page 0: it re-reads `pageParams[0]` and then asks
 *  `getNextPageParam` for each following page, so a history page has to be
 *  able to name the no-params read. A missing param would end the walk there
 *  and drop the default page — today, today+6 and the whole backlog — from the
 *  cache. */
export type AgendaStreamPageParam =
  | { kind: 'default' }
  | { kind: 'history'; from: string; to: string };

/** The no-params read as a param: `initialPageParam`, never `undefined`, so
 *  the refetch walk always has a page 0 to start from. */
const DEFAULT_PAGE_PARAM: AgendaStreamPageParam = { kind: 'default' };

/** The whole stream as React Query caches it (`pages[0]` is the earliest
 *  loaded window — previous pages are prepended on the left). */
export type AgendaStreamData = InfiniteData<
  AgendaResponse,
  AgendaStreamPageParam
>;

export function useAgendaStream() {
  // How many pages the NEXT refetch reloads — `oldPages.length` in TanStack's
  // forward walk. Assigned during render, so during a refetch it still holds
  // the OLD count (the component has not re-rendered yet): exactly the budget
  // `nextStreamPageParam` needs. A ref, never `query.data`: the cache is
  // already in flux while `getNextPageParam` runs.
  const pageBudgetRef = useRef(1);
  const query = useInfiniteQuery({
    queryKey: queryKeys.agenda.stream(),
    initialPageParam: DEFAULT_PAGE_PARAM,
    queryFn: ({ pageParam }) =>
      pageParam.kind === 'history'
        ? getAgendaRange(pageParam.from, pageParam.to)
        : getAgendaRange(),
    // History ends at the oldest row the server knows about, and the window
    // always ends before the anchor (`previousHistoryWindow`) — so the walk
    // back down is the server's own paging rule.
    getPreviousPageParam: (_firstPage, allPages) => {
      const window = previousHistoryWindow(allPages);
      if (!window) return undefined;
      const param: AgendaStreamPageParam = {
        kind: 'history',
        from: window.from,
        to: window.to,
      };
      return param;
    },
    // A refetch's forward walk: it names every page after page 0 again, the
    // default read last (`nextStreamPageParam` — never a read past the
    // anchor, and never a walk that forgets today).
    getNextPageParam: (lastPage, allPages) =>
      nextStreamPageParam(lastPage, allPages.length, pageBudgetRef.current),
  });
  pageBudgetRef.current = query.data?.pages.length ?? pageBudgetRef.current;
  return query;
}

// Agenda write mutations — thin wrappers, no optimistic paint (mirrors
// tasks.ts / occurrences.ts). `onMutate` cancels the stream so an in-flight
// refetch (window focus, another tab's mutation) can never resolve over the
// page's optimistic paint mid-write. The page owns the optimistic paint
// (`agenda-helpers.ts`) and merges the authoritative row on success.
// `reschedule` additionally re-reads the stream on settle: it is the one
// write that can target a day NO loaded page covers, where Home drops the
// row from the cache and only the server's page can paint it back.

/** Cancels Home's whole day-stream, wherever it currently spans. */
export async function cancelAgendaQuery(): Promise<void> {
  await queryClient.cancelQueries({ queryKey: queryKeys.agenda.stream() });
}

export function useMoveAgendaItem() {
  return useMutation({
    mutationFn: ({ id, input }: { id: string; input: MoveAgendaItemInput }) =>
      moveAgendaItem(id, input),
    onMutate: cancelAgendaQuery,
  });
}

export function useRescheduleAgendaItem() {
  return useMutation({
    mutationFn: ({
      id,
      input,
    }: {
      id: string;
      input: RescheduleAgendaItemInput;
    }) => rescheduleAgendaItem(id, input),
    onMutate: cancelAgendaQuery,
    // Fired, never awaited: `mutateAsync` must not wait out every loaded
    // page's refetch (Home's callers roll back on failure, which would then
    // write an old snapshot over the fresh server data).
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.agenda.all });
    },
  });
}
