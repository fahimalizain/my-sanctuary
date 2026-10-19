import { getAuthorizationServer, parseResources, type Env } from './server';

/**
 * Header slice 3's public proxy sets on every request it forwards from the internet, overwriting
 * whatever a client sent. `/internal/*` is reachable only over the `AUTH` service binding, so a
 * request that still carries this header came in through the public Worker: answer 404 so the
 * internal surface stays invisible even if the route is ever exposed by mistake.
 */
export const PROXY_HEADER = 'x-sanctuary-proxy';

/** The whole `/internal/*` namespace is private to the service binding. */
export const INTERNAL_PREFIX = '/internal';

/** Token validation for resource servers (`POST`, called by the `/mcp` handler). */
export const VALIDATE_PATH = '/internal/validate';

/**
 * The `props` slice 2b stores with a grant via `completeAuthorization()`: the SessionUser fields
 * the public Worker's `/auth/me` returns. Only these four are ever sent back over
 * `/internal/validate`, so a future addition to the stored props is not exposed by accident.
 */
export interface SessionUser {
  id: string;
  email: string;
  name: string;
  picture: string;
}

interface ValidateRequestBody {
  token: string;
  resource: string;
}

/**
 * `POST /internal/validate`
 *
 * Request: `{ "token": "<bearer>", "resource": "<canonical resource URL>" }`
 * Response: `200 { userId, scope, props }` where `props` is the SessionUser above, or
 * `401 { error: "invalid_token" }` for anything the library will not vouch for:
 * unknown, expired, or revoked tokens, a resource this server does not declare, and a token
 * minted for a different resource (the library checks the audience).
 * Malformed input is `400 { error: "invalid_request" }`, a non-POST method is 405.
 */
export async function handleValidateRequest(
  request: Request,
  env: Env,
): Promise<Response> {
  if (request.method !== 'POST') {
    return json({ error: 'invalid_request' }, 405, { Allow: 'POST' });
  }

  const body = await readJsonObject(request);
  if (!body) return json({ error: 'invalid_request' }, 400);

  const { token, resource } = body as Partial<ValidateRequestBody>;
  if (!isNonEmptyString(token) || !isNonEmptyString(resource)) {
    return json({ error: 'invalid_request' }, 400);
  }

  // `validateToken()` throws for a resource that is not registered, so reject unknown targets
  // here first: an unknown resource must read as an invalid token, never as a server error.
  if (!parseResources(env.MCP_RESOURCES).includes(resource)) {
    return json({ error: 'invalid_token' }, 401);
  }

  const validated = await getAuthorizationServer(
    env,
  ).validateToken<SessionUser>(resource, token, env);
  if (!validated) return json({ error: 'invalid_token' }, 401);

  return json(
    {
      userId: validated.userId,
      scope: validated.scope,
      props: toSessionUser(validated.props),
    },
    200,
  );
}

function json(body: unknown, status: number, headers?: HeadersInit): Response {
  return Response.json(body, { status, headers });
}

/** Parse a JSON object body, or `undefined` for anything else (malformed, not an object, ...). */
async function readJsonObject(
  request: Request,
): Promise<Record<string, unknown> | undefined> {
  try {
    const body: unknown = await request.json();
    if (typeof body !== 'object' || body === null || Array.isArray(body))
      return undefined;
    return body as Record<string, unknown>;
  } catch {
    return undefined;
  }
}

function isNonEmptyString(value: unknown): value is string {
  return typeof value === 'string' && value !== '';
}

/**
 * Reduce a grant's decrypted props to the four promised fields. Props written by another flow
 * cannot be guessed at, so a missing field becomes an empty string rather than `undefined` —
 * `JSON.stringify` would silently drop the key and change the response shape.
 */
function toSessionUser(props: unknown): SessionUser {
  const source = (props ?? {}) as Record<string, unknown>;
  return {
    id: stringField(source['id']),
    email: stringField(source['email']),
    name: stringField(source['name']),
    picture: stringField(source['picture']),
  };
}

function stringField(value: unknown): string {
  return typeof value === 'string' ? value : '';
}
