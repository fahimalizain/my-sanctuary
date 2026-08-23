import { fetchJson } from './client';
import type {
  AgendaItemResponse,
  AgendaResponse,
  MoveAgendaItemInput,
  NewAgendaItemInput,
  RescheduleAgendaItemInput,
} from '@/app/types';

/** The day's agenda. `date` is optional on purpose — a first Home load omits
 *  `?date=` so the server reads its own civil today (ADR 0004); an undefined
 *  or empty date never appends the query. */
export function getAgenda(date?: string): Promise<AgendaResponse> {
  return fetchJson<AgendaResponse>(
    date ? `/api/agenda?date=${encodeURIComponent(date)}` : '/api/agenda',
  );
}

export function createAgendaItem(
  input: NewAgendaItemInput,
): Promise<AgendaItemResponse> {
  return fetchJson<AgendaItemResponse>('/api/agenda/items', {
    method: 'POST',
    body: input,
  });
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

export function deleteAgendaItem(id: string): Promise<void> {
  return fetchJson<void>(`/api/agenda/items/${id}`, { method: 'DELETE' });
}
