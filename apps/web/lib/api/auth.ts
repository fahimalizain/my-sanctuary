import { fetchJson } from './client';

/** How an account signs in. `google` = OAuth + Google Calendar; `standalone`
 *  = local username/password + the local calendar stand-in. */
export type IntegrationType = 'google' | 'standalone';

/** The signed-in user as returned by `GET /auth/me` (the same shape
 *  `lib/auth.tsx` used to type locally — single source of truth now). */
export interface AuthUser {
  id: string;
  email: string;
  name: string;
  picture: string;
  /** Optional on purpose: a cached `/auth/me` from a pre-standalone worker
   *  has no such field. Read it through `integrationType()` (defaults to
   *  `google`) instead of touching it directly. */
  integration_type?: IntegrationType;
}

export interface MeResponse {
  user: AuthUser | null;
}

/** The envelope of the two standalone auth endpoints — a user is always
 *  present on success (unlike `/auth/me`). */
export interface AuthUserResponse {
  user: AuthUser;
}

/** `GET /auth/providers` — which sign-in methods this deployment exposes for
 *  this request host. Computed server-side (env flag + host), never guessed
 *  by the client. */
export interface AuthProviders {
  google: boolean;
  standalone: boolean;
}

export interface StandaloneLoginInput {
  username: string;
  password: string;
}

export interface StandaloneRegisterInput {
  username: string;
  password: string;
  name?: string;
  /** IANA zone; the server validates it and 400s an invalid one. */
  time_zone?: string;
}

export function getMe(): Promise<MeResponse> {
  return fetchJson<MeResponse>('/auth/me');
}

export function getProviders(): Promise<AuthProviders> {
  return fetchJson<AuthProviders>('/auth/providers');
}

export function loginStandalone(
  input: StandaloneLoginInput,
): Promise<AuthUserResponse> {
  return fetchJson<AuthUserResponse>('/auth/standalone/login', {
    method: 'POST',
    body: input,
  });
}

export function registerStandalone(
  input: StandaloneRegisterInput,
): Promise<AuthUserResponse> {
  return fetchJson<AuthUserResponse>('/auth/standalone/register', {
    method: 'POST',
    body: input,
  });
}

export function logout(): Promise<void> {
  return fetchJson<void>('/auth/logout', { method: 'POST' });
}
