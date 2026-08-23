import { fetchJson } from './client';
import type {
  NewRoutineInput,
  RoutineResponse,
  RoutinesResponse,
  UpdateRoutineInput,
} from '@/app/types';

export function listRoutines(): Promise<RoutinesResponse> {
  return fetchJson<RoutinesResponse>('/api/routines');
}

export function createRoutine(
  input: NewRoutineInput,
): Promise<RoutineResponse> {
  return fetchJson<RoutineResponse>('/api/routines', {
    method: 'POST',
    body: input,
  });
}

export function updateRoutine(
  id: string,
  input: UpdateRoutineInput,
): Promise<RoutineResponse> {
  return fetchJson<RoutineResponse>(`/api/routines/${id}`, {
    method: 'PATCH',
    body: input,
  });
}

export function deleteRoutine(id: string): Promise<void> {
  return fetchJson<void>(`/api/routines/${id}`, { method: 'DELETE' });
}
