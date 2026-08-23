import { queryOptions, useQuery } from '@tanstack/react-query';
import { listCalendarEvents, listCalendars } from '@/lib/api';
import { queryKeys } from './keys';

// Same split as lists.ts: React-free `queryOptions` factory + hooks.
// Query's signal is forwarded so a month change aborts the in-flight
// request for the previous range instead of racing it.

export function calendarEventsQueryOptions(timeMin: string, timeMax: string) {
  return queryOptions({
    queryKey: queryKeys.calendar.events(timeMin, timeMax),
    queryFn: ({ signal }) =>
      listCalendarEvents({ timeMin, timeMax, signal }),
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