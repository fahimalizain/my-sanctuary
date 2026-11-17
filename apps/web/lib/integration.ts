// How an account is integrated, derived from `/auth/me`'s optional
// `integration_type`. Pure and API-free on purpose: the UI and its unit tests
// must both read a missing field as `google` (an old cached response, or a
// worker that predates standalone mode, must never break the app).

import type { AuthUser, IntegrationType } from '@/lib/api';

/** The minimal shape these helpers need — `useAuth().user` fits, and so does
 *  a bare `{ integration_type }` in tests. */
export type IntegrableUser = Pick<AuthUser, 'integration_type'> | null;

/** `user.integration_type`, defaulting to `google` when absent/unknown. */
export function integrationType(user: IntegrableUser): IntegrationType {
  return user?.integration_type === 'standalone' ? 'standalone' : 'google';
}

/** True for a local username/password account (no Google Calendar behind it). */
export function isStandaloneUser(user: IntegrableUser): boolean {
  return integrationType(user) === 'standalone';
}
