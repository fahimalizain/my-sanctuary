/**
 * The test double for the `APP` service binding: the only part of the public Worker this Worker
 * talks to, and the only place a session comes from. It answers what the real public Worker
 * answers — `GET /auth/me` → `{"user": null}` or `{"user": {id, email, name, picture}}` — and
 * nothing else, so a test that reaches for another path fails loudly instead of silently
 * succeeding against a stub.
 *
 * `vitest.config.ts` wires it in as a function-valued service binding; tests import the constants
 * to act as the browser (which cookie) and to assert on the identity the grant must carry.
 */

/** The public Worker's session cookie name (`SESSION_COOKIE_NAME` in packages/api-core). */
export const SESSION_COOKIE = 'sanctuary-session';

/** The user a request with `Cookie: sanctuary-session=TEST_USER.id` is signed in as. */
export const TEST_USER = {
  id: 'user-1',
  email: 'ada@example.com',
  name: 'Ada Lovelace',
  picture: 'https://example.com/ada.png',
};

/** A session value that makes the double fail, for the "public Worker is down" path. */
export const APP_FAILURE_SESSION = 'unavailable';

/** A session value the double answers with a user that cannot identify anyone. */
export const MALFORMED_SESSION = 'malformed';

/** The cookie header that signs a request in as {@link TEST_USER}. */
export function sessionCookie(userId: string = TEST_USER.id): string {
  return `${SESSION_COOKIE}=${userId}`;
}

/** The `APP` binding itself: a fetch handler Miniflare resolves in-process. */
export function appService(request: Request): Response {
  const url = new URL(request.url);
  if (url.pathname !== '/auth/me' || request.method !== 'GET') {
    return new Response('not found', { status: 404 });
  }

  const session = cookieValue(request.headers.get('Cookie'), SESSION_COOKIE);
  if (session === APP_FAILURE_SESSION) {
    return new Response('upstream is unhappy', { status: 500 });
  }
  if (session === MALFORMED_SESSION) {
    // A 200 the authorization endpoint cannot trust: no `id`, so no grant owner.
    return Response.json({ user: { id: '', email: 'ada@example.com' } });
  }
  if (session === TEST_USER.id) {
    return Response.json({ user: TEST_USER });
  }
  return Response.json({ user: null });
}

/** The value of one cookie in a `Cookie` header, or `undefined` when it is not there. */
function cookieValue(header: string | null, name: string): string | undefined {
  if (!header) return undefined;
  for (const part of header.split(';')) {
    const separator = part.indexOf('=');
    if (separator === -1) continue;
    if (part.substring(0, separator).trim() === name) {
      return part.substring(separator + 1).trim();
    }
  }
  return undefined;
}
