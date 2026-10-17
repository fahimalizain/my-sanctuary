import { fetchJson } from './client';
import type {
  AgendaItemResponse,
  AgendaResponse,
  MoveAgendaItemInput,
  MoveDayToTodayResponse,
  RescheduleAgendaItemInput,
} from '@/app/types';

/** One window of the agenda stream (ADR 0004 amendment, ADR 0007). Both bounds
 *  omitted → `GET /api/agenda` with no query at all: the server reads its own
 *  civil today and returns `anchor_date … today + 6` plus the `backlog`
 *  array. Both set → the inclusive history window (max 31 days): an inverted
 *  (`to < from`) or oversized one still 400s, but a `to` on or after
 *  `anchor_date` is clamped to the day before the anchor, and a window that
 *  then has `from > to` is an empty 200, not an error. A history read's
 *  `backlog` is still empty. Never sends `?date=` — the old single-day read
 *  is gone, and the browser must never compute "today" itself. */
export function getAgendaRange(
  from?: string,
  to?: string,
): Promise<AgendaResponse> {
  if (!from || !to) return fetchJson<AgendaResponse>('/api/agenda');
  return fetchJson<AgendaResponse>(
    `/api/agenda?from=${encodeURIComponent(from)}&to=${encodeURIComponent(to)}`,
  );
}

export function moveAgendaItem(
  id: string,
  input: MoveAgendaItemInput,
): Promise<AgendaItemResponse> {
  return fetchJson<AgendaItemResponse>(`/api/agenda/items/${id}/move`, {
    method: 'POST',
    body: input,
  });
}

export function rescheduleAgendaItem(
  id: string,
  input: RescheduleAgendaItemInput,
): Promise<AgendaItemResponse> {
  return fetchJson<AgendaItemResponse>(`/api/agenda/items/${id}/reschedule`, {
    method: 'POST',
    body: input,
  });
}

/** Sweep a **past** day's still-pending rows onto civil today, in one tap
 *  (`?date >= today` 400s). The response carries only the rows that moved, in
 *  the order they were appended to today's pile. Rows that cannot move
 *  (done/skipped/in-progress occurrences, terminal or soft-deleted tasks,
 *  leftovers of a deleted routine, orphans) never fail the call — they simply
 *  stay put. */
export function moveDayToToday(date: string): Promise<MoveDayToTodayResponse> {
  return fetchJson<MoveDayToTodayResponse>(
    `/api/agenda/days/${encodeURIComponent(date)}/move-to-today`,
    { method: 'POST' },
  );
}
