import { describe, expect, it } from 'vitest';
import { callWorker, json, postJson, postRaw, RESOURCE } from './helpers';

const VALIDATE_PATH = '/internal/validate';

describe('POST /internal/validate', () => {
  it('rejects an unknown token with invalid_token', async () => {
    const response = await postJson(VALIDATE_PATH, {
      token: 'not-a-real-token',
      resource: RESOURCE,
    });

    expect(response.status).toBe(401);
    expect(response.headers.get('content-type')).toContain('application/json');
    expect(await json(response)).toEqual({ error: 'invalid_token' });
  });

  it('rejects a resource this server does not declare', async () => {
    const response = await postJson(VALIDATE_PATH, {
      token: 'not-a-real-token',
      resource: 'https://evil.example/mcp',
    });

    expect(response.status).toBe(401);
    expect(await json(response)).toEqual({ error: 'invalid_token' });
  });

  it('rejects malformed JSON with invalid_request', async () => {
    const response = await postRaw(VALIDATE_PATH, '{ not json');

    expect(response.status).toBe(400);
    expect(await json(response)).toEqual({ error: 'invalid_request' });
  });

  it('rejects a body without the required fields', async () => {
    const missingResource = await postJson(VALIDATE_PATH, {
      token: 'not-a-real-token',
    });
    const emptyToken = await postJson(VALIDATE_PATH, {
      token: '',
      resource: RESOURCE,
    });

    expect(missingResource.status).toBe(400);
    expect(await json(missingResource)).toEqual({ error: 'invalid_request' });
    expect(emptyToken.status).toBe(400);
    expect(await json(emptyToken)).toEqual({ error: 'invalid_request' });
  });

  it('answers 405 for a non-POST method', async () => {
    const response = await callWorker(VALIDATE_PATH, { method: 'GET' });

    expect(response.status).toBe(405);
    expect(response.headers.get('allow')).toBe('POST');
    expect(await json(response)).toEqual({ error: 'invalid_request' });
  });

  it('hides the internal surface from requests that came through the public proxy', async () => {
    const response = await postJson(
      VALIDATE_PATH,
      { token: 'not-a-real-token', resource: RESOURCE },
      { 'x-sanctuary-proxy': 'oauth' },
    );

    expect(response.status).toBe(404);
  });

  it('answers 404 for any other internal path', async () => {
    const response = await callWorker('/internal/other', { method: 'POST' });

    expect(response.status).toBe(404);
  });

  it('still serves public paths that carry the proxy header', async () => {
    // The public proxy stamps the header on every request it forwards, so the guard above must
    // only ever apply to the internal surface.
    const metadata = await callWorker(
      '/.well-known/oauth-authorization-server',
      {
        headers: { 'x-sanctuary-proxy': 'oauth' },
      },
    );
    const authorize = await callWorker('/oauth/authorize', {
      headers: { 'x-sanctuary-proxy': 'oauth' },
    });

    expect(metadata.status).toBe(200);
    // The authorization endpoint answers an incomplete request itself, rather than the library
    // turning it away: it is routed before the library.
    expect(authorize.status).toBe(400);
  });
});
