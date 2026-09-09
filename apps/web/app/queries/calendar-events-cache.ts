import type { CalendarEvent, CalendarEventsResponse } from '@/app/types';

/**
 * Pure patcher: replace `event` by id or append. Spreads `old` so `sync` and
 * `source` survive. Returns `old` when there is no events array.
 */
export function applyCalendarEventUpsert(
  old: CalendarEventsResponse | undefined,
  event: CalendarEvent,
): CalendarEventsResponse | undefined {
  if (!old?.events) return old;
  const idx = old.events.findIndex((e) => e.id === event.id);
  const events =
    idx >= 0
      ? old.events.map((e) => (e.id === event.id ? event : e))
      : [...old.events, event];
  return { ...old, events };
}

/**
 * Pure patcher: drop `id` from events. Spreads `old` so `sync` and `source`
 * survive. Returns `old` when missing, or when the id was not present.
 */
export function applyCalendarEventRemove(
  old: CalendarEventsResponse | undefined,
  id: string,
): CalendarEventsResponse | undefined {
  if (!old?.events) return old;
  const events = old.events.filter((e) => e.id !== id);
  if (events.length === old.events.length) return old;
  return { ...old, events };
}
