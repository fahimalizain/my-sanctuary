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
import { previousHistoryWindow } from '@/app/modules/home/agenda-helpers';

// Home reads ONE infinite stream of day windows (ADR 0004 amendment — slice
// 5, ADR 0007 — slice 6): the first page is the server's default read (no
// query at all, so the browser never computes today) — `anchor_date … today+6`
// plus the `backlog` array. `getPreviousPageParam` walks history in 7-day
// windows down to `earliest_date`; there is NO forward page, the stream ends
// at today+6. Every write on the page edits this cache in place (optimistic
// paint) and `cancelAgendaQuery` protects it from an in-flight refetch — there
// is no per-date key left.

/** One stream page: 7 inclusive civil days, or `undefined` for the very first
 *  page (the default read — the GET that omits `from`/`to` entirely). */
export type AgendaStreamPageParam = { from: string; to: string };

/** The whole stream as React Query caches it (`pages[0]` is the earliest
 *  loaded window — previous pages are prepended on the left). */
export type AgendaStreamData = InfiniteData<
  AgendaResponse,
  AgendaStreamPageParam | undefined
>;

export function useAgendaStream() {
  return useInfiniteQuery({
    queryKey: queryKeys.agenda.stream(),
    // The default page is the API's own read: no `from`/`to` at all. Its
    // param stays `undefined` FOREVER — never rewritten to `{ from, to }`.
    // Pinning it would turn the next refetch into a history read whose `to`
    // (today+6) is not `< anchor_date`, which the server 400s. A refetch
    // recomputes the anchor instead; that slide is correct.
    initialPageParam: undefined as AgendaStreamPageParam | undefined,
    queryFn: ({ pageParam }) =>
      pageParam
        ? getAgendaRange(pageParam.from, pageParam.to)
        : getAgendaRange(),
    // History ends at the oldest row the server knows about, and the window
    // always ends before the anchor (`previousHistoryWindow`).
    getPreviousPageParam: (_firstPage, allPages) =>
      previousHistoryWindow(allPages),
    // No forward page: today is inside the default window.
    getNextPageParam: () => undefined,
  });
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
