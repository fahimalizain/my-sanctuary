import { fetchJson } from './client';
import type {
  NewRoutineInput,
  ReorderRoutinesResponse,
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

/** The body must list every living routine id exactly once (400 otherwise). */
export function reorderRoutines(
  ids: string[],
): Promise<ReorderRoutinesResponse> {
  return fetchJson<ReorderRoutinesResponse>('/api/routines/reorder', {
    method: 'POST',
    body: { ids },
  });
}
