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
  return useInfiniteQuery({
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
}

// Agenda write mutations — thin wrappers, no optimistic paint, no
// invalidation (mirrors tasks.ts / occurrences.ts). `onMutate` cancels the
// stream so an in-flight refetch (window focus, another tab's mutation) can
// never resolve over the page's optimistic paint mid-write. The page owns the
// optimistic paint (`agenda-helpers.ts`) and merges the authoritative row on
// success.

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
  });
}
