import { test } from 'node:test';
import assert from 'node:assert/strict';
import { ApiError, parseApiError } from './error';

function jsonResponse(body: unknown, status: number): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

test('ApiError extends Error and carries status', () => {
  const err = new ApiError('nope', 400);
  assert.ok(err instanceof Error);
  assert.equal(err.message, 'nope');
  assert.equal(err.status, 400);
});

test('parseApiError: JSON {"error": "nope"} + 400 → message nope, status 400', async () => {
  const err = await parseApiError(jsonResponse({ error: 'nope' }, 400));
  assert.ok(err instanceof ApiError);
  assert.equal(err.message, 'nope');
  assert.equal(err.status, 400);
});

test('parseApiError: non-JSON body → generic status message', async () => {
  const err = await parseApiError(
    new Response('Internal Server Error', { status: 500 }),
  );
  assert.ok(err instanceof ApiError);
  assert.equal(err.message, 'Request failed with status 500');
  assert.equal(err.status, 500);
});

test('parseApiError: JSON without an error key → generic status message', async () => {
  const err = await parseApiError(jsonResponse({ message: 'nope' }, 400));
  assert.equal(err.message, 'Request failed with status 400');
  assert.equal(err.status, 400);
});

test('parseApiError: error value that is not a string → generic status message', async () => {
  const err = await parseApiError(jsonResponse({ error: { code: 1 } }, 400));
  assert.equal(err.message, 'Request failed with status 400');
  assert.equal(err.status, 400);
});
