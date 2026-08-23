import { fetchJson } from './client';

/** The signed-in user as returned by `GET /auth/me` (the same shape
 *  `lib/auth.tsx` used to type locally — single source of truth now). */
export interface AuthUser {
  id: string;
  email: string;
  name: string;
  picture: string;
}

export interface MeResponse {
  user: AuthUser | null;
}

export function getMe(): Promise<MeResponse> {
  return fetchJson<MeResponse>('/auth/me');
}

export function logout(): Promise<void> {
  return fetchJson<void>('/auth/logout', { method: 'POST' });
}
