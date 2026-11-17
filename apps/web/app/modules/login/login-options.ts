// The login page's visibility rules, kept pure so the provider combinations
// are unit-testable without a DOM (the test runner only globs `*.test.ts`).
//
// The server owns the truth (`GET /auth/providers`): we never infer a method
// from build env. The one client-side decision is the read-failure fallback —
// behave exactly like the pre-standalone page (Google button only) so a
// broken providers endpoint cannot lock a working Google deployment out.

import type { AuthProviders } from '@/lib/api';

export interface LoginOptions {
  google: boolean;
  standalone: boolean;
  /** Set only when the providers read failed and the Google-only default is
   *  in force, so the page can say why the form is missing. */
  fallbackNotice?: string;
}

export const GOOGLE_ONLY_FALLBACK: AuthProviders = {
  google: true,
  standalone: false,
};

export const PROVIDERS_FALLBACK_NOTICE =
  'Could not load the available sign-in methods. Showing Google sign-in.';

/**
 * Resolve which sign-in controls to paint.
 *
 * - `providersError` set → Google-only fallback with a notice.
 * - `providers` missing (still loading, or an empty response) → nothing.
 * - otherwise → the server's answer, with extra/unknown fields ignored.
 */
export function resolveLoginOptions(
  providers: AuthProviders | null | undefined,
  providersError?: unknown,
): LoginOptions {
  if (providersError !== undefined && providersError !== null) {
    return {
      google: GOOGLE_ONLY_FALLBACK.google,
      standalone: GOOGLE_ONLY_FALLBACK.standalone,
      fallbackNotice: PROVIDERS_FALLBACK_NOTICE,
    };
  }

  if (!providers) {
    return { google: false, standalone: false };
  }

  return {
    google: providers.google === true,
    standalone: providers.standalone === true,
  };
}

/**
 * The browser's IANA zone for registration (`Intl.DateTimeFormat`), or
 * `undefined` when the runtime cannot say. The server then applies its
 * configured default, so registration must never fail over this.
 */
export function browserTimeZone(): string | undefined {
  try {
    const zone = Intl.DateTimeFormat().resolvedOptions().timeZone;
    return zone ? zone : undefined;
  } catch {
    return undefined;
  }
}
