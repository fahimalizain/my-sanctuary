import { SELF } from 'cloudflare:test';
import { expect } from 'vitest';

/** The issuer tests run against — the production value pinned in vitest.config.ts. */
export const ISSUER = 'https://my-sanctuary.fahimalizain.com';

/** The one protected resource the test runtime registers: production's MCP endpoint. */
export const RESOURCE = `${ISSUER}/mcp`;

/**
 * Call the Worker the way the public proxy does: the original public URL, delivered over a
 * service binding. Routing, issuer checks and metadata URLs therefore see exactly what
 * production sees.
 */
export function callWorker(
  path: string,
  init?: RequestInit,
): Promise<Response> {
  return SELF.fetch(`${ISSUER}${path}`, init);
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
 * support starts with. Slice 2b continues from here (authorize → token → validate).
 */
export async function registerClient(
  overrides: Record<string, unknown> = {},
): Promise<RegisteredClient> {
  const response = await postJson('/oauth/register', {
    redirect_uris: ['https://client.example/callback'],
    token_endpoint_auth_method: 'none',
    client_name: 'Test Client',
    ...overrides,
  });
  expect(response.status).toBe(201);
  return json<RegisteredClient>(response);
}
