import { useEffect } from 'react';
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
import { addCivilDays } from '@/app/modules/routines/rrule-preview';

// Home reads ONE infinite stream of day windows (ADR 0004 amendment — slice
// 5): the first page is the server's anchor week (no query at all, so the
// browser never computes today), `getPreviousPageParam` walks history in
// 7-day windows down to `earliest_date`, `getNextPageParam` walks the
// unbounded future. Every write on the page edits this cache in place
// (optimistic paint) and `cancelAgendaQuery` protects it from an in-flight
// refetch — there is no per-date key left.

/** One stream page: 7 inclusive civil days, or `undefined` for the very first
 *  page (the anchor week — the GET that omits `from`/`to` entirely). */
export type AgendaStreamPageParam = { from: string; to: string };

/** The whole stream as React Query caches it (`pages[0]` is the earliest
 *  loaded window — previous pages are prepended on the left). */
export type AgendaStreamData = InfiniteData<
  AgendaResponse,
  AgendaStreamPageParam | undefined
>;

/** Days per stream window: one week forward and one week back per fetch. */
const AGENDA_PAGE_DAYS = 7;

export function useAgendaStream() {
  const query = useInfiniteQuery({
    queryKey: queryKeys.agenda.stream(),
    // The anchor page is the API's own default read: no `from`/`to` at all.
    initialPageParam: undefined as AgendaStreamPageParam | undefined,
    queryFn: ({ pageParam }) =>
      pageParam
        ? getAgendaRange(pageParam.from, pageParam.to)
        : getAgendaRange(),
    // History ends at the oldest row the server knows about.
    getPreviousPageParam: (firstPage) =>
      firstPage.from <= firstPage.earliest_date
        ? undefined
        : {
            from: addCivilDays(firstPage.from, -AGENDA_PAGE_DAYS),
            to: addCivilDays(firstPage.from, -1),
          },
    // The future never ends — keep offering the next week.
    getNextPageParam: (lastPage) => ({
      from: addCivilDays(lastPage.to, 1),
      to: addCivilDays(lastPage.to, AGENDA_PAGE_DAYS),
    }),
  });
  const data = query.data;

  // Pin the anchor page's param once it has actually come back. While it is
  // still `undefined`, a v5 refetch reuses "the server picks the anchor" and
  // recomputes `anchor_date`, so any invalidation can slide the whole loaded
  // span (Oct 10–23 becomes Oct 17–30). Rewriting the param to the window
  // THAT page answered makes every later refetch re-request it; the loaded
  // dates then stay put even when `anchor_date` in the payload moves.
  // Not necessarily index 0 — a prepend moves the anchor page to the right.
  // Pinned exactly once: with no `undefined` param left there is nothing to
  // do, so this cannot loop.
  useEffect(() => {
    if (!data) return;
    const current = queryClient.getQueryData<AgendaStreamData>(
      queryKeys.agenda.stream(),
    );
    if (!current) return;
    const index = current.pageParams.findIndex((param) => param == null);
    if (index < 0) return;
    const page = current.pages[index];
    if (!page?.from || !page?.to) return;
    queryClient.setQueryData<AgendaStreamData>(queryKeys.agenda.stream(), {
      ...current,
      pageParams: current.pageParams.map((param, i) =>
        i === index ? { from: page.from, to: page.to } : param,
      ),
    });
  }, [data]);

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
