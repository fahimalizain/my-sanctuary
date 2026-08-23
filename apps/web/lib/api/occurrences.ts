import { fetchJson } from './client';
import type {
  OccurrenceActionResponse,
  OccurrenceResponse,
  UpdateOccurrenceInput,
} from '@/app/types';

export function updateOccurrence(
  id: string,
  input: UpdateOccurrenceInput,
): Promise<OccurrenceResponse> {
  return fetchJson<OccurrenceResponse>(`/api/occurrences/${id}`, {
    method: 'PATCH',
    body: input,
  });
}

export function completeOccurrence(id: string): Promise<OccurrenceResponse> {
  return fetchJson<OccurrenceResponse>(`/api/occurrences/${id}/complete`, {
    method: 'POST',
  });
}

export function skipOccurrence(id: string): Promise<OccurrenceResponse> {
  return fetchJson<OccurrenceResponse>(`/api/occurrences/${id}/skip`, {
    method: 'POST',
  });
}

export function startOccurrence(
  id: string,
): Promise<OccurrenceActionResponse> {
  return fetchJson<OccurrenceActionResponse>(`/api/occurrences/${id}/start`, {
    method: 'POST',
  });
}