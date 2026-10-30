import { fetchJson } from './client';
import type {
  ApiKeysResponse,
  CreateApiKeyResponse,
  DeleteApiKeyResponse,
  NewApiKeyInput,
} from '@/app/types';

export function listApiKeys(): Promise<ApiKeysResponse> {
  return fetchJson<ApiKeysResponse>('/api/keys');
}

export function createApiKey(
  input: NewApiKeyInput,
): Promise<CreateApiKeyResponse> {
  return fetchJson<CreateApiKeyResponse>('/api/keys', {
    method: 'POST',
    body: input,
  });
}

export function revokeApiKey(id: string): Promise<DeleteApiKeyResponse> {
  return fetchJson<DeleteApiKeyResponse>(
    `/api/keys/${encodeURIComponent(id)}`,
    { method: 'DELETE' },
  );
}
