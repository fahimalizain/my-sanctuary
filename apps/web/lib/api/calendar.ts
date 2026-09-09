import { fetchJson } from './client';
import type {
  CalendarEventsResponse,
  CalendarsResponse,
  CreateEventResponse,
  DeleteCalendarEventResponse,
  NewCalendarEventInput,
  PatchCalendarEventInput,
} from '@/app/types';

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

export function createCalendarEvent(
  input: NewCalendarEventInput,
): Promise<CreateEventResponse> {
  return fetchJson<CreateEventResponse>('/api/calendar/events', {
    method: 'POST',
    body: input,
  });
}

export function updateCalendarEvent(
  id: string,
  input: PatchCalendarEventInput,
): Promise<CreateEventResponse> {
  return fetchJson<CreateEventResponse>(`/api/calendar/events/${id}`, {
    method: 'PATCH',
    body: input,
  });
}

export function deleteCalendarEvent(
  id: string,
): Promise<DeleteCalendarEventResponse> {
  return fetchJson<DeleteCalendarEventResponse>(`/api/calendar/events/${id}`, {
    method: 'DELETE',
  });
}
