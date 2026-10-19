import {
  AuthorizationError,
  CimdFetchError,
  type AuthRequest,
} from '@cloudflare/workers-oauth-provider';
import {
  applyPageHeaders,
  renderConsentPage,
  renderErrorPage,
} from './consent-page';
import { getAuthorizationServer, SCOPES, type Env } from './server';
import type { SessionUser } from './validate';

/**
 * `GET /oauth/authorize` and `POST /oauth/authorize`: the application-owned half of the
 * authorization server.
 *
 * A GET validates the request, finds the user through the public Worker's session, and shows a
 * consent page carrying a single-use handle. The POST that page submits consumes that handle,
 * writes the grant with the signed-in user as its props, and redirects back to the client.
 * Everything protocol-owned (discovery, the token endpoint, revocation) stays in the library,
 * and `getAuthorizationServer()` memoises it across requests.
 */

/**
 * The scope granted when a client asks for nothing this server can offer: none of the catalogue,
 * or only names this deployment never advertised. Least privilege is the safe default — the MCP
 * resource server answers `403 insufficient_scope` with the scopes an operation really needs, so
 * a client that wants more re-authorizes and gets a second consent page.
 */
export const DEFAULT_SCOPE = 'mcp:read';

/**
 * The scopes to offer on the consent page: what the client asked for, narrowed to this server's
 * catalogue, in catalogue order. The page lets the user tick a subset of these, and
 * `approveConsent()` accepts only scopes this server supports.
 */
export function consentScopes(requested: readonly string[]): string[] {
  const offered = SCOPES.filter((scope) => requested.includes(scope));
  return offered.length > 0 ? offered : [DEFAULT_SCOPE];
}

/** Route the application-owned authorization endpoint; it is not the library's to serve. */
export function handleAuthorizeRequest(
  request: Request,
  env: Env,
): Promise<Response> {
  if (request.method === 'GET') return handleAuthorizeGet(request, env);
  if (request.method === 'POST') return handleAuthorizePost(request, env);
  return Promise.resolve(methodNotAllowed(request.method));
}

/** The endpoint serves a page and the page's own submission, nothing else. */
function methodNotAllowed(method: string): Response {
  return renderErrorPage({
    status: 405,
    title: 'Method not allowed',
    message: `${method} is not supported by this endpoint.`,
    headers: { Allow: 'GET, POST' },
  });
}

/**
 * `GET /oauth/authorize`
 *
 * The request is validated before anything else, so a bad one never bounces through Google: the
 * user is only asked to sign in once this server knows which client it would be signing in for.
 * Only then is the session read, and only a signed-in user sees the consent page.
 */
async function handleAuthorizeGet(
  request: Request,
  env: Env,
): Promise<Response> {
  const oauth = getAuthorizationServer(env).getOAuthApi(env);
  try {
    const authRequest = await oauth.parseAuthRequest(request);

    const session = await lookupSession(request, env);
    if (session.status === 'unavailable') return upstreamErrorPage();
    if (session.status === 'logged-out') return loginRedirect(request);

    // The consent page offers the scopes this server supports, so the stored request carries the
    // same narrowed list: it is what the page renders and what the POST approves.
    const offered: AuthRequest = {
      ...authRequest,
      scope: consentScopes(authRequest.scope),
    };
    // describeConsent() resolves the client first, so a client ID metadata document that cannot
    // be fetched throws before beginConsent() leaves a transaction in KV.
    const details = await oauth.describeConsent(offered);
    const consent = await oauth.beginConsent(offered);

    applyPageHeaders(consent.headers);
    return new Response(
      renderConsentPage(details, consent.handle, session.user.email),
      { status: 200, headers: consent.headers },
    );
  } catch (error) {
    return authorizationErrorResponse(error, requestPath(request));
  }
}

/**
 * `POST /oauth/authorize`
 *
 * The handle is single-use, ten-minute, and bound to the browser that opened the page, which is
 * what makes the form unforgeable; the `Origin` check below covers the case a browser does not:
 * a cross-origin form can carry the handle's cookie and a decision without being same-site.
 * The session is read again, because the page may have sat open while the user signed out. The
 * scopes are whatever the user ticked, and the library accepts only ones it supports.
 */
async function handleAuthorizePost(
  request: Request,
  env: Env,
): Promise<Response> {
  const retryPath = requestPath(request);

  const origin = request.headers.get('Origin');
  if (origin !== null && origin !== new URL(request.url).origin) {
    return renderErrorPage({
      status: 403,
      title: 'Request blocked',
      message: 'This page was submitted from another site.',
    });
  }

  const session = await lookupSession(request, env);
  if (session.status === 'unavailable') return upstreamErrorPage();
  if (session.status === 'logged-out') {
    return renderErrorPage({
      status: 400,
      title: 'Your session ended',
      message: 'Sign in again, then start again from the app.',
      retryPath,
    });
  }

  const form = await readForm(request);
  if (!form) {
    return renderErrorPage({
      status: 400,
      title: 'This page could not be read',
      message: 'Start again from the app.',
      retryPath,
    });
  }

  const handle = form.get('handle');
  if (typeof handle !== 'string' || handle === '') {
    return renderErrorPage({
      status: 400,
      title: 'This page expired',
      message: 'Start again from the app.',
      retryPath,
    });
  }

  const oauth = getAuthorizationServer(env).getOAuthApi(env);
  const { id, email, name, picture } = session.user;
  try {
    if (form.get('decision') !== 'approve') {
      const denied = await oauth.denyConsent(request, handle);
      return new Response(null, { status: 302, headers: denied.headers });
    }

    // The ticks are the user's to edit, so the library checks every value against the server's
    // catalogue before the request is consumed; an unsupported one is a 400 below.
    const approved = await oauth.approveConsent(request, handle, {
      scope: form.getAll('scope').map(String),
    });
    // Leaving `revokeExistingGrants` at its default is deliberate: the user's earlier grants for
    // this client and resource go away once this one is stored, so re-authorizing replaces the
    // previous grant instead of leaving two of them live.
    const client = await oauth.lookupClient(approved.request.clientId);
    const { redirectTo } = await oauth.completeAuthorization({
      request: approved.request,
      userId: id,
      // Advisory metadata for the grant list. The name is client-supplied either way, and the
      // consent page is where it is shown as unverified.
      metadata: { clientName: client?.clientName },
      scope: approved.request.scope,
      props: { id, email, name, picture },
    });

    approved.headers.set('Location', redirectTo);
    return new Response(null, { status: 302, headers: approved.headers });
  } catch (error) {
    return authorizationErrorResponse(error, retryPath);
  }
}

/** What the public Worker's `GET /auth/me` told this server about the visitor. */
type SessionLookup =
  | { status: 'user'; user: SessionUser }
  | { status: 'logged-out' }
  | { status: 'unavailable' };

/**
 * Read the signed-in session from the public Worker, the only component that knows about
 * cookies. The incoming `Cookie` header is forwarded untouched, which is safe on a service
 * binding: nothing about the session is interpreted here, and the response is narrowed to the
 * four fields below before it is used.
 *
 * A non-2xx response, unparsable JSON, or a user object without an `id` and an `email` is an
 * upstream this Worker cannot trust — reported as `unavailable` rather than as a logged-out
 * user, so a broken public Worker cannot silently sign someone out mid-flow.
 */
async function lookupSession(
  request: Request,
  env: Env,
): Promise<SessionLookup> {
  const headers = new Headers();
  const cookie = request.headers.get('Cookie');
  if (cookie !== null) headers.set('Cookie', cookie);

  const response = await env.APP.fetch(
    new Request(new URL('/auth/me', request.url), { method: 'GET', headers }),
  );
  if (!response.ok) return { status: 'unavailable' };

  let body: unknown;
  try {
    body = await response.json();
  } catch {
    return { status: 'unavailable' };
  }
  if (typeof body !== 'object' || body === null || Array.isArray(body)) {
    return { status: 'unavailable' };
  }

  const user = (body as Record<string, unknown>)['user'];
  if (user === null || user === undefined) return { status: 'logged-out' };

  const parsed = parseSessionUser(user);
  return parsed ? { status: 'user', user: parsed } : { status: 'unavailable' };
}

/**
 * The four fields a session user must have. `id` and `email` name the grant's owner, so a
 * response without them is a malformed upstream; `name` and `picture` are only ever displayed
 * and may be empty strings.
 */
function parseSessionUser(value: unknown): SessionUser | undefined {
  if (typeof value !== 'object' || value === null) return undefined;
  const { id, email, name, picture } = value as Record<string, unknown>;
  if (typeof id !== 'string' || id === '') return undefined;
  if (typeof email !== 'string' || email === '') return undefined;
  if (typeof name !== 'string' || typeof picture !== 'string') return undefined;
  return { id, email, name, picture };
}

/**
 * Send a logged-out visitor through the public Worker's Google login, and back here afterwards.
 * `next` is this request's path and query, percent-encoded the way the login route expects; it
 * re-validates it before using it, and drops it when it is not a same-origin path.
 */
function loginRedirect(request: Request): Response {
  const url = new URL(request.url);
  const next = encodeURIComponent(`${url.pathname}${url.search}`);
  return new Response(null, {
    status: 302,
    headers: {
      Location: `/auth/google?next=${next}`,
      'Cache-Control': 'no-store',
    },
  });
}

/**
 * Translate a library error into a response, per what each one proves:
 *
 * - a redirect only when `parseAuthRequest()` validated the client's exact redirect URI: the
 *   error then carries the code, the client's `state` and this server's `iss`;
 * - a local page for every other expected failure, because the client or its redirect URI is
 *   not trusted, or the client's metadata document could not be resolved;
 * - anything else is a bug or an outage, and surfaces as a 500.
 */
function authorizationErrorResponse(
  error: unknown,
  retryPath: string,
): Response {
  if (error instanceof AuthorizationError && error.redirectTo) {
    return new Response(null, {
      status: 302,
      headers: { Location: error.redirectTo, 'Cache-Control': 'no-store' },
    });
  }
  if (error instanceof AuthorizationError || error instanceof CimdFetchError) {
    return renderErrorPage({
      status: 400,
      title: 'This authorization did not complete',
      message:
        error instanceof AuthorizationError
          ? error.description
          : 'The app could not be verified. Start again from the app.',
      retryPath,
    });
  }
  throw error;
}

/** The public Worker's `/auth/me` did not answer: a session cannot be established or confirmed. */
function upstreamErrorPage(): Response {
  return renderErrorPage({
    status: 502,
    title: 'Sign-in is unavailable',
    message: 'Sanctuary could not check your session. Please try again.',
  });
}

/** This URL's own path and query, as a link target that restarts the same authorization. */
function requestPath(request: Request): string {
  const url = new URL(request.url);
  return `${url.pathname}${url.search}`;
}

/** Parse the consent form, or `undefined` when the request is not a form at all. */
async function readForm(request: Request): Promise<FormData | undefined> {
  try {
    return await request.formData();
  } catch {
    return undefined;
  }
}
