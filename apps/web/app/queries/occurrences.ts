import { useMutation } from '@tanstack/react-query';
import {
  completeOccurrence,
  pauseOccurrence,
  reopenOccurrence,
  skipOccurrence,
  startOccurrence,
  updateOccurrence,
} from '@/lib/api';
import type { UpdateOccurrenceInput } from '@/app/types';
import { cancelAgendaQuery } from './agenda';

// Occurrence write mutations — thin wrappers, no optimistic paint, no
// invalidation (mirrors tasks.ts / agenda.ts). `onMutate` cancels Home's
// agenda day-stream so an in-flight refetch (window focus, another tab's
// mutation) can never resolve over the page's optimistic cache mid-write. The
// page owns the optimistic paint (chip flip) and merges the authoritative
// occurrence on success.

export function useCompleteOccurrence() {
  return useMutation({
    mutationFn: ({ id }: { id: string }) => completeOccurrence(id),
    onMutate: cancelAgendaQuery,
  });
}

export function useSkipOccurrence() {
  return useMutation({
    mutationFn: ({ id }: { id: string }) => skipOccurrence(id),
    onMutate: cancelAgendaQuery,
  });
}

export function useReopenOccurrence() {
  return useMutation({
    mutationFn: ({ id }: { id: string }) => reopenOccurrence(id),
    onMutate: cancelAgendaQuery,
  });
}

export function useStartOccurrence() {
  return useMutation({
    mutationFn: ({ id }: { id: string }) => startOccurrence(id),
    onMutate: cancelAgendaQuery,
  });
}

export function usePauseOccurrence() {
  return useMutation({
    mutationFn: ({ id }: { id: string }) => pauseOccurrence(id),
    onMutate: cancelAgendaQuery,
  });
}

export function useUpdateOccurrence() {
  return useMutation({
    mutationFn: ({ id, input }: { id: string; input: UpdateOccurrenceInput }) =>
      updateOccurrence(id, input),
    onMutate: cancelAgendaQuery,
  });
}
