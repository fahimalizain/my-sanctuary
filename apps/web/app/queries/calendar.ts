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
import type { PatchCalendarEventInput } from '@/app/types';
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

export function useCreateCalendarEvent() {
  return useMutation({
    mutationFn: createCalendarEvent,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.calendar.all });
    },
  });
}

export function useUpdateCalendarEvent() {
  return useMutation({
    mutationFn: ({
      id,
      input,
    }: {
      id: string;
      input: PatchCalendarEventInput;
    }) => updateCalendarEvent(id, input),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.calendar.all });
    },
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
