import { useMutation } from '@tanstack/react-query';
import {
  completeOccurrence,
  skipOccurrence,
  startOccurrence,
  updateOccurrence,
} from '@/lib/api';
import type { UpdateOccurrenceInput } from '@/app/types';
import { queryKeys } from './keys';
import { queryClient } from '@/lib/queryClient';

// Occurrence write mutations — thin wrappers, no optimistic paint, no
// invalidation (mirrors tasks.ts / agenda.ts). `onMutate` cancels the VIEWED
// date's agenda query so an in-flight refetch (window focus, another tab's
// mutation) can never resolve over the page's optimistic cache mid-write. The
// page owns the optimistic paint (chip flip) and merges the authoritative
// occurrence on success — the `date` passed alongside the input is only used
// for the cancel.

async function cancelAgendaQuery(date: string): Promise<void> {
  await queryClient.cancelQueries({
    queryKey: queryKeys.agenda.byDate(date || undefined),
  });
}

export function useCompleteOccurrence() {
  return useMutation({
    mutationFn: ({ id }: { id: string; date: string }) => completeOccurrence(id),
    onMutate: ({ date }) => cancelAgendaQuery(date),
  });
}

export function useSkipOccurrence() {
  return useMutation({
    mutationFn: ({ id }: { id: string; date: string }) => skipOccurrence(id),
    onMutate: ({ date }) => cancelAgendaQuery(date),
  });
}

export function useStartOccurrence() {
  return useMutation({
    mutationFn: ({ id }: { id: string; date: string }) => startOccurrence(id),
    onMutate: ({ date }) => cancelAgendaQuery(date),
  });
}

export function useUpdateOccurrence() {
  return useMutation({
    mutationFn: ({
      id,
      input,
    }: {
      id: string;
      input: UpdateOccurrenceInput;
      date: string;
    }) => updateOccurrence(id, input),
    onMutate: ({ date }) => cancelAgendaQuery(date),
  });
}