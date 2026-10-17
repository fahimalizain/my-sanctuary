import { useInfiniteQuery, useMutation, useQuery } from '@tanstack/react-query';
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
  historyPageZero,
  previousHistoryWindow,
} from '@/app/modules/home/agenda-helpers';

// Home reads TWO agenda caches (ADR 0004 amendment — slice 5, ADR 0007 —
// slice 6), so a library refetch can never drop today:
//
//  - `useAgendaCurrent` is the server's default read (no query at all, so the
//    browser never computes today): `anchor_date … today+6` plus the `backlog`
//    array. A plain query — nothing about it depends on paging.
//  - `useAgendaHistory` is the walk back behind the anchor, one 7-day window
//    per page, keyed by the anchor it walks behind. Page 0 is the anchor's eve
//    week (`historyPageZero`) and `getNextPageParam` APPENDS older weeks, so
//    page 0 stays the eve window for the life of this key and TanStack's
//    refetch loop (page 0 re-read from its stored param, every later page
//    named by `getNextPageParam`) re-reads every loaded week without ever
//    dropping the one that abuts the anchor.
//
// Every write on the page edits the two caches in place through
// `readAgendaPages` / `writeAgendaPages` (optimistic paint) and
// `cancelAgendaQuery` protects them from an in-flight refetch — there is no
// per-date key.

/** Home's default read: `anchor_date … today+6` plus the whole backlog. */
export function useAgendaCurrent() {
  return useQuery({
    queryKey: queryKeys.agenda.current(),
    queryFn: () => getAgendaRange(),
  });
}

/** One history page's param — the inclusive window it was read with. */
type AgendaHistoryPageParam = { from: string; to: string };

/** Home's history walk behind `anchor`: 7-day windows ending the anchor's eve
 *  and stepping further back. `earliest` is the server's `earliest_date` — the
 *  back-scroll floor — and `enabled` is the caller's own gate (Home opens
 *  history only once the user reaches the top of the stream). The query also
 *  stays off when nothing is older than the anchor. */
export function useAgendaHistory(
  anchor: string,
  earliest: string,
  enabled: boolean,
) {
  const pageZero = historyPageZero(anchor, earliest);
  return useInfiniteQuery({
    queryKey: queryKeys.agenda.history(anchor),
    enabled: enabled && pageZero != null,
    // Page 0: the week ending the anchor's eve, floored at `earliest`. The
    // fallback only ever applies while the query is disabled — there is no
    // valid anchor/earliest yet, so nothing fetches.
    initialPageParam: pageZero ?? { from: anchor, to: anchor },
    queryFn: ({ pageParam }) => getAgendaRange(pageParam.from, pageParam.to),
    // Older weeks are APPENDED (page 0 stays the eve window), each one the
    // week before the oldest loaded date — `undefined` ends the walk.
    getNextPageParam: (_lastPage, allPages) => previousHistoryWindow(allPages),
    // Never prepend. TanStack rebuilds a refetch from `pageParams[0]` and then
    // `getNextPageParam`: with prepended pages page 0 would be the OLDEST week
    // and a refetch would keep only that one, dropping the eve window that
    // abuts the anchor. Appending keeps page 0's param stable for the life of
    // this key, and the UI still loads older weeks: at the top of the stream it
    // calls `fetchNextPage`.
    getPreviousPageParam: () => undefined,
  });
}

/** Home's agenda cache as the optimistic edits see it: the history pages and
 *  the default read in one list, earliest first (`readAgendaPages`).
 *  `pageParams` mirrors each page's own `from`/`to` — the helpers never read
 *  them, and `writeAgendaPages` never replaces the history query's stored
 *  params. */
export type AgendaPages = {
  pages: AgendaResponse[];
  pageParams: AgendaHistoryPageParam[];
};

function historyCache(
  anchor: string,
): InfiniteData<AgendaResponse, AgendaHistoryPageParam> | undefined {
  return queryClient.getQueryData<
    InfiniteData<AgendaResponse, AgendaHistoryPageParam>
  >(queryKeys.agenda.history(anchor));
}

/** Both agenda caches concatenated for a reader (Home's cache plumbing): the
 *  history pages (they may be missing — the walk is lazy) and, when it has
 *  resolved, the default read. Sorted by `from` ascending, so a reader sees
 *  the days in order whichever cache a page came from. `undefined` when
 *  neither cache holds anything. */
export function readAgendaPages(anchor: string): AgendaPages | undefined {
  const history = historyCache(anchor);
  const current = queryClient.getQueryData<AgendaResponse>(
    queryKeys.agenda.current(),
  );
  const pages = [...(history?.pages ?? []), ...(current ? [current] : [])];
  if (pages.length === 0) return undefined;
  pages.sort((a, b) => (a.from < b.from ? -1 : a.from > b.from ? 1 : 0));
  return {
    pages,
    pageParams: pages.map((page) => ({ from: page.from, to: page.to })),
  };
}

/** Writes an optimistic edit back into the two caches (Home's cache
 *  plumbing). The page that reaches the anchor (`to >= anchor`) is the default
 *  read and goes to `agenda.current()`; every other page is one history week
 *  and is matched back into the history query by its `from`.
 *
 *  History is written back in the QUERY's own page order and its `pageParams`
 *  are left untouched: `pageParams[0]` must stay the eve window, or the next
 *  refetch would rebuild the walk from the oldest week and lose the week that
 *  abuts the anchor. Pages the updater left alone stay as they are, and a
 *  history cache is never created here — an edit can only move rows between
 *  pages that already exist. */
export function writeAgendaPages(
  anchor: string,
  next: AgendaPages | undefined,
): void {
  if (!next) return;
  const current = queryClient.getQueryData<AgendaResponse>(
    queryKeys.agenda.current(),
  );
  if (!current) return;
  const freshCurrent = next.pages.find((page) => page.to >= anchor);
  if (freshCurrent && freshCurrent !== current) {
    queryClient.setQueryData<AgendaResponse>(
      queryKeys.agenda.current(),
      freshCurrent,
    );
  }
  const history = historyCache(anchor);
  if (!history || history.pages.length === 0) return;
  const touched = new Map(
    next.pages
      .filter((page) => page !== freshCurrent)
      .map((page) => [page.from, page]),
  );
  let changed = false;
  const pages = history.pages.map((page) => {
    const updated = touched.get(page.from);
    if (!updated || updated === page) return page;
    changed = true;
    return updated;
  });
  if (!changed) return;
  queryClient.setQueryData(queryKeys.agenda.history(anchor), {
    pages,
    pageParams: history.pageParams,
  });
}

// Agenda write mutations — thin wrappers, no optimistic paint (mirrors
// tasks.ts / occurrences.ts). `onMutate` cancels both agenda caches so an
// in-flight refetch (window focus, another tab's mutation) can never resolve
// over the page's optimistic paint mid-write. The page owns the optimistic
// paint (`agenda-helpers.ts`) and merges the authoritative row on success.
// `reschedule` additionally re-reads the agenda on settle: it is the one write
// that can target a day NO loaded page covers, where Home drops the row from
// the cache and only the server's page can paint it back.

/** Cancels Home's whole agenda read: the default window and every loaded
 *  history week (`agenda.all` is the prefix of both keys). */
export async function cancelAgendaQuery(): Promise<void> {
  await queryClient.cancelQueries({ queryKey: queryKeys.agenda.all });
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
