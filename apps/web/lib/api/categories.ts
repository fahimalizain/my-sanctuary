import { fetchJson } from './client';
import type {
  CategoriesResponse,
  CategoryResponse,
  DeleteCategoryResponse,
  NewCategoryInput,
  UpdateCategoryInput,
} from '@/app/types';

export function listCategories(): Promise<CategoriesResponse> {
  return fetchJson<CategoriesResponse>('/api/categories');
}

export function createCategory(
  input: NewCategoryInput,
): Promise<CategoryResponse> {
  return fetchJson<CategoryResponse>('/api/categories', {
    method: 'POST',
    body: input,
  });
}

export function updateCategory(
  id: string,
  input: UpdateCategoryInput,
): Promise<CategoryResponse> {
  return fetchJson<CategoryResponse>(`/api/categories/${id}`, {
    method: 'PATCH',
    body: input,
  });
}

export function deleteCategory(id: string): Promise<DeleteCategoryResponse> {
  return fetchJson<DeleteCategoryResponse>(`/api/categories/${id}`, {
    method: 'DELETE',
  });
}