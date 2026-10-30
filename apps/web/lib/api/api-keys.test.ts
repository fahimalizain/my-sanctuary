// `lib/api/client.ts` reads the Vite `__API_BASE_URL__` define as a bare
// global at import time, and static ESM imports hoist — so the global is set
// first and the module graph is dynamic-imported in `before` (same
// constraint as `app/queries/range-gc.test.ts`).

import { afterEach, before, test } from 'node:test';
import assert from 'node:assert/strict';
import { ApiError } from './error';

(globalThis as { __API_BASE_URL__?: string }).__API_BASE_URL__ = '';

interface FetchCall {
  url: string;
  init: RequestInit;
}

let calls: FetchCall[] = [];
let responseBody: unknown = {};
let responseStatus = 200;
let api: typeof import('./api-keys');

before(async () => {
  (globalThis as { fetch: typeof fetch }).fetch = ((
    input: RequestInfo | URL,
    init?: RequestInit,
  ) => {
    calls.push({ url: String(input), init: init ?? {} });
    return Promise.resolve(
      new Response(JSON.stringify(responseBody), {
        status: responseStatus,
        headers: { 'Content-Type': 'application/json' },
      }),
    );
  }) as typeof fetch;

  api = await import('./api-keys');
});

afterEach(() => {
  calls = [];
  responseBody = {};
  responseStatus = 200;
});

function onlyCall(): FetchCall {
  assert.equal(calls.length, 1, 'exactly one request');
  return calls[0];
}

function jsonBody(call: FetchCall): unknown {
  const raw = call.init.body;
  assert.equal(typeof raw, 'string', 'body is a JSON string');
  return JSON.parse(raw as string);
}

test('listApiKeys: GET /api/keys with the session cookie', async () => {
  const keys = [
    {
      id: 'k1',
      name: 'Scripts',
      key_prefix: 'sk_ab12',
      expires_on: '2026-12-01',
      expires_at: '2026-12-01T00:00:00Z',
      last_used_at: null,
      created_at: '2026-11-01T09:00:00Z',
    },
  ];
  responseBody = { keys };

  const result = await api.listApiKeys();

  const call = onlyCall();
  assert.equal(call.url, '/api/keys');
  assert.equal(call.init.method, undefined, 'GET carries no explicit method');
  assert.equal(call.init.credentials, 'include');
  assert.equal(call.init.body, undefined);
  assert.deepEqual(result, { keys });
});

test('createApiKey: POST /api/keys with the JSON body and the secret response', async () => {
  const key = {
    id: 'k2',
    name: 'CI',
    key_prefix: 'sk_cd34',
    expires_on: '2026-12-01',
    expires_at: '2026-12-01T00:00:00Z',
    last_used_at: null,
    created_at: '2026-11-01T09:00:00Z',
  };
  responseBody = { key, secret: 'sk_cd34_plaintext' };

  const result = await api.createApiKey({
    name: 'CI',
    expires_on: '2026-12-01',
  });

  const call = onlyCall();
  assert.equal(call.url, '/api/keys');
  assert.equal(call.init.method, 'POST');
  assert.equal(call.init.credentials, 'include');
  assert.deepEqual(call.init.headers, { 'Content-Type': 'application/json' });
  assert.deepEqual(jsonBody(call), { name: 'CI', expires_on: '2026-12-01' });
  assert.deepEqual(result, { key, secret: 'sk_cd34_plaintext' });
});

test('revokeApiKey: DELETE /api/keys/:id with the id percent-encoded', async () => {
  responseBody = { success: true };

  const result = await api.revokeApiKey('a/b c');

  const call = onlyCall();
  assert.equal(call.url, '/api/keys/a%2Fb%20c');
  assert.equal(call.init.method, 'DELETE');
  assert.equal(call.init.credentials, 'include');
  assert.equal(call.init.body, undefined);
  assert.deepEqual(result, { success: true });
});

test('a non-ok response throws the ApiError envelope message', async () => {
  responseStatus = 404;
  responseBody = { error: 'API key not found' };

  await assert.rejects(
    () => api.revokeApiKey('missing'),
    (err: unknown) => {
      assert.ok(err instanceof ApiError);
      assert.equal(err.message, 'API key not found');
      assert.equal(err.status, 404);
      return true;
    },
  );
});
