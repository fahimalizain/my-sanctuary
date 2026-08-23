import { fetchJson } from './client';
import type {
  DeleteListResponse,
  NewListInput,
  TaskListResponse,
  TaskListsResponse,
  UpdateListInput,
} from '@/app/types';

export function listLists(): Promise<TaskListsResponse> {
  return fetchJson<TaskListsResponse>('/api/lists');
}

export function createList(input: NewListInput): Promise<TaskListResponse> {
  return fetchJson<TaskListResponse>('/api/lists', {
    method: 'POST',
    body: input,
  });
}

export function updateList(
  id: string,
  input: UpdateListInput,
): Promise<TaskListResponse> {
  return fetchJson<TaskListResponse>(`/api/lists/${id}`, {
    method: 'PATCH',
    body: input,
  });
}

export function deleteList(id: string): Promise<DeleteListResponse> {
  return fetchJson<DeleteListResponse>(`/api/lists/${id}`, {
    method: 'DELETE',
  });
}