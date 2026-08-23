import { queryOptions, useMutation, useQuery } from '@tanstack/react-query';
import {
  createAgendaItem,
  deleteAgendaItem,
  getAgenda,
  moveAgendaItem,
  rescheduleAgendaItem,
} from '@/lib/api';
import type {
  AgendaItemRecord,
  AgendaResponse,
  MoveAgendaItemInput,
  NewAgendaItemInput,
  RescheduleAgendaItemInput,
} from '@/app/types';
import { queryKeys } from './keys';
import { queryClient } from '@/lib/queryClient';

// Same split as tasks.ts: React-free `queryOptions` factory + hooks. Each
// viewed date is its own key (`['agenda', date]`); an empty/undefined date
// maps to the `['agenda', 'today']` sentinel whose GET omits `?date=` so the
// server reads its own civil today (ADR 0004 — the browser never computes the
// default Home date). After the first load Home copies the sentinel payload
// onto the civil-today key and re-queries that key — a cache hit, never a
// second GET.

export function agendaQueryOptions(date?: string) {
  return queryOptions({
    queryKey: queryKeys.agenda.byDate(date),
    queryFn: () => getAgenda(date || undefined), // omit ?date= when empty
  });
}

export function useAgendaQuery(date: string) {
  // date === '' → sentinel key ['agenda','today'] + getAgenda() with no date
  return useQuery(agendaQueryOptions(date || undefined));
}

/** Drop-in replacement for the page's `setItems` / `setItems(prev => …)` —
 *  writes the viewed date's key as `{ items, today, time_zone }`. */
export function setAgendaItems(
  date: string,
  updater:
    | AgendaItemRecord[]
    | ((prev: AgendaItemRecord[]) => AgendaItemRecord[]),
): void {
  const key = queryKeys.agenda.byDate(date || undefined);
  queryClient.setQueryData<AgendaResponse>(key, (old) => {
    const prev = old?.items ?? [];
    const next = typeof updater === 'function' ? updater(prev) : updater;
    return {
      items: next,
      today: old?.today ?? '',
      time_zone: old?.time_zone ?? '',
    };
  });
}

// Agenda write mutations — thin wrappers, no optimistic paint, no
// invalidation (mirrors tasks.ts). `onMutate` cancels the VIEWED date's
// query so an in-flight refetch (window focus, another tab's mutation) can
// never resolve over the page's optimistic cache mid-write. The page owns
// the optimistic paint and merges the authoritative row on success — the
// `date` passed alongside the input is only used for the cancel.

async function cancelAgendaQuery(date: string): Promise<void> {
  await queryClient.cancelQueries({
    queryKey: queryKeys.agenda.byDate(date || undefined),
  });
}

export function useMoveAgendaItem() {
  return useMutation({
    mutationFn: ({
      id,
      input,
    }: {
      id: string;
      input: MoveAgendaItemInput;
      date: string;
    }) => moveAgendaItem(id, input),
    onMutate: ({ date }) => cancelAgendaQuery(date),
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
      date: string;
    }) => rescheduleAgendaItem(id, input),
    onMutate: ({ date }) => cancelAgendaQuery(date),
  });
}

export function useCreateAgendaItem() {
  return useMutation({
    mutationFn: ({ input }: { input: NewAgendaItemInput; date: string }) =>
      createAgendaItem(input),
    onMutate: ({ date }) => cancelAgendaQuery(date),
  });
}

export function useDeleteAgendaItem() {
  return useMutation({
    mutationFn: ({ id }: { id: string; date: string }) => deleteAgendaItem(id),
    onMutate: ({ date }) => cancelAgendaQuery(date),
  });
}