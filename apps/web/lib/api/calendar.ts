import { fetchJson } from './client';
import type { CalendarEventsResponse, CalendarsResponse } from '@/app/types';

export function listCalendarEvents(args: {
  timeMin: string;
  timeMax: string;
  signal?: AbortSignal;
}): Promise<CalendarEventsResponse> {
  const params = new URLSearchParams({
    time_min: args.timeMin,
    time_max: args.timeMax,
  });
  return fetchJson<CalendarEventsResponse>(`/api/calendar/events?${params}`, {
    signal: args.signal,
  });
}

export function listCalendars(): Promise<CalendarsResponse> {
  return fetchJson<CalendarsResponse>('/api/calendar/calendars');
}
