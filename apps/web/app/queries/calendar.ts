import {
  keepPreviousData,
  queryOptions,
  useMutation,
  useQuery,
  type QueryClient,
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
import {
  applyCalendarEventRemove,
  applyCalendarEventUpsert,
} from './calendar-events-cache';
import { queryKeys } from './keys';
import { queryClient, RANGE_QUERY_GC_TIME_MS } from '@/lib/queryClient';
import {
  nextCalendarStamp,
  stampCalendarServerReadWindow,
} from '@/app/modules/calendar/lib/calendar-server-reads';

export {
  applyCalendarEventRemove,
  applyCalendarEventUpsert,
} from './calendar-events-cache';

// Same split as lists.ts: React-free `queryOptions` factory + hooks.
// Query's signal is forwarded so a month change aborts the in-flight
// request for the previous range instead of racing it.
// placeholderData keeps the prior range painted while a strip rebase
// fetches the new window — avoids a full-grid "Loading events" flash.
// `gcTime` is per-query (not a client default): only this range-keyed window
// is worth keeping warm after its last observer leaves.

export function calendarEventsQueryOptions(timeMin: string, timeMax: string) {
  return queryOptions({
    queryKey: queryKeys.calendar.events(timeMin, timeMax),
    // Stamp the read when the request STARTS: the local change layer only
    // retires acknowledged intent on a read that began after the ack (a GET
    // that began earlier can carry a row that predates the write). The stamp
    // hangs off the window, not the rows, so structural sharing can keep row
    // identity across identical refetches.
    queryFn: async ({ signal }) => {
      const stamp = nextCalendarStamp();
      const data = await listCalendarEvents({ timeMin, timeMax, signal });
      stampCalendarServerReadWindow(timeMin, timeMax, stamp);
      return data;
    },
    placeholderData: keepPreviousData,
    gcTime: RANGE_QUERY_GC_TIME_MS,
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
 * Preserves each entry's `sync` and `source`. Used after a successful PATCH
 * so the durable cache matches the server without a full invalidate.
 */
export function upsertCalendarEventInCache(event: CalendarEvent): void {
  queryClient.setQueriesData<CalendarEventsResponse>(
    { queryKey: calendarEventsQueryKey },
    (old) => applyCalendarEventUpsert(old, event),
  );
}

/**
 * Drop `id` from every cached events query. Preserves each entry's `sync`
 * and `source`. Used after a successful DELETE so the durable cache matches
 * the server without a full invalidate.
 */
export function removeCalendarEventFromCache(id: string): void {
  queryClient.setQueriesData<CalendarEventsResponse>(
    { queryKey: calendarEventsQueryKey },
    (old) => applyCalendarEventRemove(old, id),
  );
}

/**
 * Last known server row for `id` across every cached events window. The patch
 * queue outlives the calendar page, so after navigation there is no mounted
 * `events` array to read from — the range-keyed cache is the durable copy and
 * may already hold a patch that landed before React re-rendered. First window
 * that contains the id wins.
 */
export function findCachedCalendarEvent(
  client: QueryClient,
  id: string,
): CalendarEvent | undefined {
  const windows = client.getQueriesData<CalendarEventsResponse>({
    queryKey: calendarEventsQueryKey,
  });
  for (const [, data] of windows) {
    const event = data?.events.find((e) => e.id === id);
    if (event) return event;
  }
  return undefined;
}

// Create is thin like update: cancel in-flight events queries on mutate,
// no invalidate on success. The session owns optimistic paint (temp overlay)
// and merges the authoritative row via upsertCalendarEventInCache.
export function useCreateCalendarEvent() {
  return useMutation({
    mutationFn: createCalendarEvent,
    onMutate: cancelCalendarEventsQuery,
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

// Delete: cancel in-flight events queries on mutate, no invalidate on success.
// Session paints via queue.remove and drops the row via removeCalendarEventFromCache.
export function useDeleteCalendarEvent() {
  return useMutation({
    mutationFn: (id: string) => deleteCalendarEvent(id),
    onMutate: cancelCalendarEventsQuery,
  });
}
