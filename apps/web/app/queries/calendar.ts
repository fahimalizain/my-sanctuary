import {
  keepPreviousData,
  queryOptions,
  useMutation,
  useQuery,
} from '@tanstack/react-query';
import {
  createCalendarEvent,
  deleteCalendarEvent,
  listCalendarEvents,
  listCalendars,
  updateCalendarEvent,
} from '@/lib/api';
import type {
  CalendarEvent,
  CalendarEventsResponse,
  PatchCalendarEventInput,
} from '@/app/types';
import { queryKeys } from './keys';
import { queryClient } from '@/lib/queryClient';

// Same split as lists.ts: React-free `queryOptions` factory + hooks.
// Query's signal is forwarded so a month change aborts the in-flight
// request for the previous range instead of racing it.
// placeholderData keeps the prior range painted while a strip rebase
// fetches the new window — avoids a full-grid "Loading events" flash.

export function calendarEventsQueryOptions(timeMin: string, timeMax: string) {
  return queryOptions({
    queryKey: queryKeys.calendar.events(timeMin, timeMax),
    queryFn: ({ signal }) => listCalendarEvents({ timeMin, timeMax, signal }),
    placeholderData: keepPreviousData,
  });
}

export function useCalendarEventsQuery(timeMin: string, timeMax: string) {
  return useQuery(calendarEventsQueryOptions(timeMin, timeMax));
}

export function calendarsQueryOptions() {
  return queryOptions({
    queryKey: queryKeys.calendar.calendars(),
    queryFn: listCalendars,
  });
}

export function useCalendarsQuery(opts?: { enabled?: boolean }) {
  return useQuery({
    ...calendarsQueryOptions(),
    enabled: opts?.enabled ?? true,
  });
}

/** Prefix for every range-keyed events query (not calendars). */
const calendarEventsQueryKey = [...queryKeys.calendar.all, 'events'] as const;

/**
 * Cancel in-flight events refetches so they cannot resolve over an optimistic
 * overlay mid-write. Does not cancel `['calendar','calendars']`.
 */
export async function cancelCalendarEventsQuery(): Promise<void> {
  await queryClient.cancelQueries({ queryKey: calendarEventsQueryKey });
}

/**
 * Patch `event` into every cached events query (replace by id or append).
 * Preserves each entry's `source`. Used after a successful PATCH so the
 * durable cache matches the server without a full invalidate.
 */
export function upsertCalendarEventInCache(event: CalendarEvent): void {
  queryClient.setQueriesData<CalendarEventsResponse>(
    { queryKey: calendarEventsQueryKey },
    (old) => {
      if (!old?.events) return old;
      const idx = old.events.findIndex((e) => e.id === event.id);
      const events =
        idx >= 0
          ? old.events.map((e) => (e.id === event.id ? event : e))
          : [...old.events, event];
      return { ...old, events };
    },
  );
}

export function useCreateCalendarEvent() {
  return useMutation({
    mutationFn: createCalendarEvent,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.calendar.all });
    },
  });
}

// Update is thin like tasks.ts: cancel in-flight events queries on mutate,
// no invalidate on success. The session owns optimistic paint (overlay) and
// merges the authoritative row via upsertCalendarEventInCache.
export function useUpdateCalendarEvent() {
  return useMutation({
    mutationFn: ({
      id,
      input,
    }: {
      id: string;
      input: PatchCalendarEventInput;
    }) => updateCalendarEvent(id, input),
    onMutate: cancelCalendarEventsQuery,
  });
}

export function useDeleteCalendarEvent() {
  return useMutation({
    mutationFn: (id: string) => deleteCalendarEvent(id),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.calendar.all });
    },
  });
}
