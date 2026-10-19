import { SELF } from 'cloudflare:test';
import { expect } from 'vitest';
import { sessionCookie } from './app-mock';

/** The issuer tests run against — the production value pinned in vitest.config.ts. */
export const ISSUER = 'https://my-sanctuary.fahimalizain.com';

/** The one protected resource the test runtime registers: production's MCP endpoint. */
export const RESOURCE = `${ISSUER}/mcp`;

/** The redirect URI `registerClient()` registers, on a host that is not loopback. */
export const REDIRECT_URI = 'https://client.example/callback';

/**
 * Call the Worker the way the public proxy does: the original public URL, delivered over a
 * service binding. Routing, issuer checks and metadata URLs therefore see exactly what
 * production sees.
 *
 * Redirects are not followed, so a test sees the 302 the Worker sent — and the client redirect
 * it points at — instead of the browser's next hop.
 */
export function callWorker(
  path: string,
  init?: RequestInit,
): Promise<Response> {
  return SELF.fetch(`${ISSUER}${path}`, { redirect: 'manual', ...init });
}

/** POST a JSON body, as MCP clients and the internal proxy do. */
export function postJson(
  path: string,
  body: unknown,
  headers?: Record<string, string>,
): Promise<Response> {
  return callWorker(path, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...headers },
    body: JSON.stringify(body),
  });
}

/** POST a raw body, for requests that must not be valid JSON. */
export function postRaw(
  path: string,
  body: string,
  headers?: Record<string, string>,
): Promise<Response> {
  return callWorker(path, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...headers },
    body,
  });
}

/** POST a form body, which is the only encoding the OAuth endpoints accept (RFC 6749 §3.2). */
export function postForm(
  path: string,
  body: Record<string, string>,
  headers?: Record<string, string>,
): Promise<Response> {
  return callWorker(path, {
    method: 'POST',
    headers: {
      'content-type': 'application/x-www-form-urlencoded',
      ...headers,
    },
    body: new URLSearchParams(body).toString(),
  });
}

export async function json<T>(response: Response): Promise<T> {
  return (await response.json()) as T;
}

export interface RegisteredClient {
  client_id: string;
  client_name: string;
  redirect_uris: string[];
  token_endpoint_auth_method: string;
  grant_types: string[];
  response_types: string[];
}

/**
 * Register a public client through RFC 7591 DCR — the flow a client without client-ID metadata
 * support starts with, and the one a registered client's authorization continues from.
 */
export async function registerClient(
  overrides: Record<string, unknown> = {},
): Promise<RegisteredClient> {
  const response = await postJson('/oauth/register', {
    redirect_uris: [REDIRECT_URI],
    token_endpoint_auth_method: 'none',
    client_name: 'Test Client',
    ...overrides,
  });
  expect(response.status).toBe(201);
  return json<RegisteredClient>(response);
}

/** A PKCE code verifier and its S256 challenge (RFC 7636). */
export async function pkce(): Promise<{
  verifier: string;
  challenge: string;
}> {
  const verifier = base64url(crypto.getRandomValues(new Uint8Array(32)));
  const digest = await crypto.subtle.digest(
    'SHA-256',
    new TextEncoder().encode(verifier),
  );
  return { verifier, challenge: base64url(new Uint8Array(digest)) };
}

export interface AuthorizeOptions {
  clientId: string;
  /** The PKCE S256 challenge; omitted, the request carries no PKCE parameters at all. */
  challenge?: string;
  redirectUri?: string;
  scope?: string[];
  state?: string;
  resource?: string;
}

/** The authorization request URL, as the browser reaches it through the public proxy. */
export function authorizePath(options: AuthorizeOptions): string {
  const query = new URLSearchParams({
    response_type: 'code',
    client_id: options.clientId,
    redirect_uri: options.redirectUri ?? REDIRECT_URI,
    scope: (options.scope ?? ['mcp:read']).join(' '),
    state: options.state ?? 'state-1',
    resource: options.resource ?? RESOURCE,
  });
  if (options.challenge !== undefined) {
    query.set('code_challenge', options.challenge);
    query.set('code_challenge_method', 'S256');
  }
  return `/oauth/authorize?${query.toString()}`;
}

export interface ConsentPage {
  response: Response;
  html: string;
  /** The single-use handle the form carries back. */
  handle: string;
  /** The binding cookie the page set, in the form a browser sends it back. */
  pair: string;
  /** What the browser posts back: the session cookie plus the binding cookie. */
  cookie: string;
}

/** Open the consent page as a signed-in browser, and read the handle out of the form. */
export async function openConsent(
  path: string,
  session: string,
): Promise<ConsentPage> {
  const response = await callWorker(path, { headers: { Cookie: session } });
  const html = await response.text();
  const handle = /name="handle" value="([^"]*)"/.exec(html)?.[1];
  expect(handle, 'the consent page carries a handle').toBeTruthy();
  const pair = cookiePair(response);
  return {
    response,
    html,
    handle: handle!,
    pair,
    cookie: `${session}; ${pair}`,
  };
}

export interface ConsentSubmission {
  path: string;
  page: ConsentPage;
  decision?: 'approve' | 'deny';
  scopes?: string[];
  /** Replaces the `Cookie` header the page was opened with, in whole or in part. */
  cookie?: string;
  origin?: string;
}

/** Submit the consent form the way the browser does, with the cookies it holds. */
export function submitConsent(
  submission: ConsentSubmission,
): Promise<Response> {
  const form = new URLSearchParams();
  form.append('handle', submission.page.handle);
  form.append('decision', submission.decision ?? 'approve');
  for (const scope of submission.scopes ?? []) form.append('scope', scope);

  const headers: Record<string, string> = {
    cookie: submission.cookie ?? submission.page.cookie,
  };
  if (submission.origin !== undefined) headers['origin'] = submission.origin;

  return callWorker(submission.path, {
    method: 'POST',
    headers: {
      'content-type': 'application/x-www-form-urlencoded',
      ...headers,
    },
    body: form.toString(),
  });
}

export interface Grant {
  path: string;
  page: ConsentPage;
  /** The client's redirect URI, as returned: `code`, the client's `state` and this server's `iss`. */
  redirect: URL;
  code: string;
}

/** Sign in, consent, approve: the whole authorization request, up to the client's redirect. */
export async function grantAccess(
  options: AuthorizeOptions & { session?: string; scopes?: string[] },
): Promise<Grant> {
  const path = authorizePath(options);
  const page = await openConsent(path, options.session ?? sessionCookie());
  expect(page.response.status).toBe(200);

  const response = await submitConsent({
    path,
    page,
    scopes: options.scopes ?? ['mcp:read'],
  });
  expect(response.status).toBe(302);
  const location = response.headers.get('location');
  expect(location, 'the approval redirects to the client').toBeTruthy();
  const redirect = new URL(location!);
  return { path, page, redirect, code: redirect.searchParams.get('code')! };
}

export interface TokenSet {
  access_token: string;
  token_type: string;
  expires_in: number;
  scope: string;
  refresh_token?: string;
  resource?: string;
}

/** Exchange an authorization code for tokens, as a public client with PKCE does. */
export async function exchangeCode(options: {
  code: string;
  verifier: string;
  clientId: string;
  redirectUri?: string;
  resource?: string;
}): Promise<TokenSet> {
  const response = await postForm('/oauth/token', {
    grant_type: 'authorization_code',
    code: options.code,
    code_verifier: options.verifier,
    client_id: options.clientId,
    redirect_uri: options.redirectUri ?? REDIRECT_URI,
    resource: options.resource ?? RESOURCE,
  });
  expect(response.status, await response.clone().text()).toBe(200);
  return json<TokenSet>(response);
}

/** Validate a token the way the `/mcp` handler does, and return the raw response. */
export function validateToken(
  token: string,
  resource: string = RESOURCE,
): Promise<Response> {
  return postJson('/internal/validate', { token, resource });
}

/** The URL of the endpoint the authorization server metadata advertises for `key`. */
export async function advertisedPath(key: string): Promise<string> {
  const metadata = await json<Record<string, string>>(
    await callWorker('/.well-known/oauth-authorization-server'),
  );
  expect(metadata[key], `metadata advertises ${key}`).toBeTruthy();
  return new URL(metadata[key]).pathname;
}

/** The first cookie the response sets, in the form a browser sends it back. */
function cookiePair(response: Response): string {
  const [setCookie] = response.headers.getSetCookie();
  expect(setCookie, 'the response sets a cookie').toBeTruthy();
  return setCookie.split(';')[0].trim();
}

function base64url(bytes: Uint8Array): string {
  let binary = '';
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary)
    .replace(/\+/g, '-')
    .replace(/\//g, '_')
    .replace(/=+$/, '');
}
