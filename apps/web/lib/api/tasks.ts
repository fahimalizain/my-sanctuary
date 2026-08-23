import { fetchJson } from './client';
import { buildClassifyUrl } from '@/app/hooks/classify-url';
import type {
  ClassifyResponse,
  DeleteTaskResponse,
  FocusTaskResponse,
  MoveTaskInput,
  MoveTaskResponse,
  NewTaskInput,
  TaskResponse,
  TasksResponse,
  UpdateTaskInput,
} from '@/app/types';

export function listTasks(): Promise<TasksResponse> {
  return fetchJson<TasksResponse>('/api/tasks');
}

export function createTask(input: NewTaskInput): Promise<TaskResponse> {
  return fetchJson<TaskResponse>('/api/tasks', {
    method: 'POST',
    body: input,
  });
}

export function updateTask(
  id: string,
  input: UpdateTaskInput,
): Promise<TaskResponse> {
  return fetchJson<TaskResponse>(`/api/tasks/${id}`, {
    method: 'PATCH',
    body: input,
  });
}

export function deleteTask(id: string): Promise<DeleteTaskResponse> {
  return fetchJson<DeleteTaskResponse>(`/api/tasks/${id}`, {
    method: 'DELETE',
  });
}

export function moveTask(
  id: string,
  input: MoveTaskInput,
): Promise<MoveTaskResponse> {
  return fetchJson<MoveTaskResponse>(`/api/tasks/${id}/move`, {
    method: 'POST',
    body: input,
  });
}

/** The timer verbs — the server dispatches the ADR 0002 transition matrix. */
export function runTaskAction(
  id: string,
  action: 'start' | 'stop' | 'pause' | 'complete' | 'discard',
): Promise<MoveTaskResponse> {
  return fetchJson<MoveTaskResponse>(`/api/tasks/${id}/${action}`, {
    method: 'POST',
  });
}

export function focusTask(id: string): Promise<FocusTaskResponse> {
  return fetchJson<FocusTaskResponse>(`/api/tasks/${id}/focus`, {
    method: 'POST',
  });
}

export function unfocusTask(): Promise<FocusTaskResponse> {
  return fetchJson<FocusTaskResponse>('/api/focus', { method: 'DELETE' });
}

/** Advisory title→category preview (`GET /api/tasks/classify`). The URL is
 *  built by `buildClassifyUrl` (kept free of `@/lib/api` so it stays
 *  unit-testable); the optional signal is forwarded for abortable callers. */
export function classifyTask(
  title: string,
  categoryId: string | null,
  opts?: { signal?: AbortSignal },
): Promise<ClassifyResponse> {
  return fetchJson<ClassifyResponse>(buildClassifyUrl(title, categoryId), {
    signal: opts?.signal,
  });
}
