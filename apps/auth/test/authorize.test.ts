import { describe, expect, it } from 'vitest';
import { consentScopes, DEFAULT_SCOPE } from '../src/authorize';
import {
  APP_FAILURE_SESSION,
  MALFORMED_SESSION,
  SESSION_COOKIE,
  TEST_USER,
  sessionCookie,
} from './app-mock';
import {
  authorizePath,
  callWorker,
  exchangeCode,
  grantAccess,
  ISSUER,
  json,
  openConsent,
  pkce,
  registerClient,
  submitConsent,
  validateToken,
} from './helpers';

/** A registered client with a fresh PKCE key pair: the fixture most of these tests need. */
async function clientAndPkce(overrides: Record<string, unknown> = {}) {
  const client = await registerClient(overrides);
  const { verifier, challenge } = await pkce();
  return { client, verifier, challenge };
}

describe('GET /oauth/authorize', () => {
  it('sends a logged-out visitor to Google login with this request as the way back', async () => {
    const { client, challenge } = await clientAndPkce();
    const path = authorizePath({
      clientId: client.client_id,
      challenge,
      scope: ['mcp:read', 'offline_access'],
    });

    const response = await callWorker(path);

    expect(response.status).toBe(302);
    const location = response.headers.get('location');
    expect(location, 'the login redirect is a relative Location').toBeTruthy();
    expect(location!.startsWith('/auth/google?next=')).toBe(true);

    // The login route validates `next` again when it comes back, so this value has to stay a
    // same-origin path — and has to be this exact request, or the user consents to somebody
    // else's authorization.
    const next = decodeURIComponent(
      location!.replace('/auth/google?next=', ''),
    );
    expect(next).toBe(path);
    expect(next.startsWith('/')).toBe(true);
    expect(next.startsWith('//')).toBe(false);
    expect(next).not.toMatch(/[\\\s]/);
    expect(new URL(location!, ISSUER).origin).toBe(ISSUER);
  });

  it('renders an unknown client_id locally instead of redirecting', async () => {
    const { challenge } = await pkce();

    const response = await callWorker(
      authorizePath({ clientId: 'not-a-registered-client', challenge }),
    );

    expect(response.status).toBe(400);
    expect(response.headers.get('location')).toBeNull();
    expect(response.headers.get('content-type')).toBe(
      'text/html; charset=utf-8',
    );
    expect(response.headers.get('content-security-policy')).toContain(
      "default-src 'none'",
    );
  });

  it('renders a redirect URI the client did not register locally', async () => {
    const { client, challenge } = await clientAndPkce();

    const response = await callWorker(
      authorizePath({
        clientId: client.client_id,
        challenge,
        redirectUri: 'https://evil.example/callback',
      }),
    );

    expect(response.status).toBe(400);
    expect(response.headers.get('location')).toBeNull();
  });

  it('redirects back to the client once its redirect URI is validated', async () => {
    const { client } = await clientAndPkce();

    // A public client must use PKCE and this request omits it: an expected failure the client is
    // trusted to hear about, unlike the two above.
    const response = await callWorker(
      authorizePath({ clientId: client.client_id }),
    );

    expect(response.status).toBe(302);
    const redirect = new URL(response.headers.get('location')!);
    expect(redirect.origin + redirect.pathname).toBe(
      'https://client.example/callback',
    );
    expect(redirect.searchParams.get('error')).toBe('invalid_request');
    expect(redirect.searchParams.get('state')).toBe('state-1');
    expect(redirect.searchParams.get('iss')).toBe(ISSUER);
  });

  it('shows the client, the destination host, the user and the scopes', async () => {
    const { client, challenge } = await clientAndPkce({
      client_name: '<script>alert(1)</script>',
    });

    const page = await openConsent(
      authorizePath({
        clientId: client.client_id,
        challenge,
        scope: ['mcp:read', 'mcp:write'],
      }),
      sessionCookie(),
    );

    expect(page.response.status).toBe(200);
    expect(page.html).not.toContain('<script>alert(1)</script>');
    expect(page.html).toContain('&#60;script&#62;alert(1)&#60;/script&#62;');
    expect(page.html).toContain(
      'This app registered itself; its name is not verified.',
    );
    expect(page.html).toContain('client.example');
    expect(page.html).toContain(TEST_USER.email);
    expect(page.html).toContain('Read your tasks, agenda and calendar');
    expect(page.html).toContain('value="mcp:read"');
    expect(page.html).toContain('value="mcp:write"');
    expect(page.html).not.toContain('value="offline_access"');
  });

  it("carries the library's binding cookie and anti-framing headers", async () => {
    const { client, challenge } = await clientAndPkce();

    const page = await openConsent(
      authorizePath({ clientId: client.client_id, challenge }),
      sessionCookie(),
    );

    const [setCookie] = page.response.headers.getSetCookie();
    expect(setCookie).toMatch(/^__Host-oauth-consent-[0-9a-f]{16}=/);
    expect(setCookie).toContain('HttpOnly');
    expect(setCookie).toContain('Secure');
    expect(page.response.headers.get('x-frame-options')).toBe('DENY');
    expect(page.response.headers.get('content-security-policy')).toContain(
      "frame-ancestors 'none'",
    );
    expect(page.response.headers.get('content-security-policy')).toContain(
      "default-src 'none'",
    );
    expect(page.response.headers.get('cache-control')).toBe('no-store');
    expect(page.response.headers.get('content-type')).toBe(
      'text/html; charset=utf-8',
    );
  });

  it('offers a read scope when the client asks for nothing the catalogue has', async () => {
    const { client, challenge } = await clientAndPkce();

    const page = await openConsent(
      authorizePath({
        clientId: client.client_id,
        challenge,
        scope: ['profile', 'mcp:admin'],
      }),
      sessionCookie(),
    );

    expect(page.html).toContain('value="mcp:read"');
    expect(page.html).not.toContain('mcp:admin');
    expect(page.html).not.toContain('profile');
  });

  it('reports a failing session lookup as an upstream failure', async () => {
    const { client, challenge } = await clientAndPkce();

    const response = await callWorker(
      authorizePath({ clientId: client.client_id, challenge }),
      { headers: { Cookie: `${SESSION_COOKIE}=${APP_FAILURE_SESSION}` } },
    );

    expect(response.status).toBe(502);
    expect(response.headers.get('content-type')).toBe(
      'text/html; charset=utf-8',
    );
    expect(await response.text()).toContain('Sign-in is unavailable');
  });

  it('reports a session without a usable identity as an upstream failure', async () => {
    const { client, challenge } = await clientAndPkce();

    const response = await callWorker(
      authorizePath({ clientId: client.client_id, challenge }),
      { headers: { Cookie: `${SESSION_COOKIE}=${MALFORMED_SESSION}` } },
    );

    // A user the grant cannot be keyed by is a broken upstream, not a signed-out visitor: a 302
    // to Google here would loop the user through a login that changes nothing.
    expect(response.status).toBe(502);
    expect(response.headers.get('location')).toBeNull();
  });

  it('answers 405 with the methods it serves', async () => {
    const response = await callWorker('/oauth/authorize', { method: 'DELETE' });

    expect(response.status).toBe(405);
    expect(response.headers.get('allow')).toBe('GET, POST');
  });
});

describe('POST /oauth/authorize', () => {
  it('denies the request with access_denied and the client state', async () => {
    const { client, challenge } = await clientAndPkce();
    const path = authorizePath({
      clientId: client.client_id,
      challenge,
      state: 'denied-state',
    });
    const page = await openConsent(path, sessionCookie());

    const response = await submitConsent({
      path,
      page,
      decision: 'deny',
      scopes: ['mcp:read'],
    });

    expect(response.status).toBe(302);
    const redirect = new URL(response.headers.get('location')!);
    expect(redirect.origin + redirect.pathname).toBe(
      'https://client.example/callback',
    );
    expect(redirect.searchParams.get('error')).toBe('access_denied');
    expect(redirect.searchParams.get('state')).toBe('denied-state');
    expect(redirect.searchParams.get('code')).toBeNull();
  });

  it('rejects a form that arrived without the binding cookie', async () => {
    const { client, challenge } = await clientAndPkce();
    const path = authorizePath({ clientId: client.client_id, challenge });
    const page = await openConsent(path, sessionCookie());

    const response = await submitConsent({
      path,
      page,
      cookie: sessionCookie(),
      scopes: ['mcp:read'],
    });

    expect(response.status).toBe(400);
    expect(response.headers.get('location')).toBeNull();
  });

  it('rejects a handle that was already used', async () => {
    const { client, challenge } = await clientAndPkce();
    const path = authorizePath({ clientId: client.client_id, challenge });
    const page = await openConsent(path, sessionCookie());

    const first = await submitConsent({ path, page, scopes: ['mcp:read'] });
    const replay = await submitConsent({ path, page, scopes: ['mcp:read'] });

    expect(first.status).toBe(302);
    expect(replay.status).toBe(400);
  });

  it('rejects a form posted from another origin, and accepts this one', async () => {
    const { client, challenge } = await clientAndPkce();
    const path = authorizePath({ clientId: client.client_id, challenge });
    const page = await openConsent(path, sessionCookie());

    const foreign = await submitConsent({
      path,
      page,
      origin: 'https://evil.example',
      scopes: ['mcp:read'],
    });
    // The rejected post never consumed the handle, so the browser that opened the page still can.
    const sameOrigin = await submitConsent({
      path,
      page,
      origin: ISSUER,
      scopes: ['mcp:read'],
    });

    expect(foreign.status).toBe(403);
    expect(foreign.headers.get('location')).toBeNull();
    expect(sameOrigin.status).toBe(302);
  });

  it('rejects an approval from a browser whose session ended', async () => {
    const { client, challenge } = await clientAndPkce();
    const path = authorizePath({ clientId: client.client_id, challenge });
    const page = await openConsent(path, sessionCookie());

    const response = await submitConsent({
      path,
      page,
      cookie: `${sessionCookie('signed-out')}; ${page.pair}`,
      scopes: ['mcp:read'],
    });

    expect(response.status).toBe(400);
    expect(await response.text()).toContain('Your session ended');
  });

  it('rejects a scope outside the catalogue', async () => {
    const { client, challenge } = await clientAndPkce();
    const path = authorizePath({ clientId: client.client_id, challenge });
    const page = await openConsent(path, sessionCookie());

    const response = await submitConsent({
      path,
      page,
      scopes: ['mcp:read', 'mcp:admin'],
    });

    expect(response.status).toBe(400);
  });

  it('reports a failing session lookup as an upstream failure', async () => {
    const { client, challenge } = await clientAndPkce();
    const path = authorizePath({ clientId: client.client_id, challenge });
    const page = await openConsent(path, sessionCookie());

    const response = await submitConsent({
      path,
      page,
      cookie: `${SESSION_COOKIE}=${APP_FAILURE_SESSION}; ${page.pair}`,
      scopes: ['mcp:read'],
    });

    expect(response.status).toBe(502);
  });
});

describe('consentScopes', () => {
  it('offers the requested scopes in catalogue order, without duplicates', () => {
    expect(consentScopes(['offline_access', 'mcp:read', 'mcp:read'])).toEqual([
      'mcp:read',
      'offline_access',
    ]);
  });

  it('falls back to the least-privilege default when nothing is on offer', () => {
    expect(consentScopes([])).toEqual([DEFAULT_SCOPE]);
    expect(consentScopes(['profile', 'email'])).toEqual([DEFAULT_SCOPE]);
    expect(DEFAULT_SCOPE).toBe('mcp:read');
  });
});

describe('the default scope, granted', () => {
  it('writes the default scope into the grant when the client asked for another', async () => {
    const { client, verifier, challenge } = await clientAndPkce();

    const { code } = await grantAccess({
      clientId: client.client_id,
      challenge,
      scope: ['profile'],
    });
    const tokens = await exchangeCode({
      code,
      verifier,
      clientId: client.client_id,
    });
    const validated = await json<{ scope: string[] }>(
      await validateToken(tokens.access_token),
    );

    expect(tokens.scope).toBe(DEFAULT_SCOPE);
    expect(validated.scope).toEqual([DEFAULT_SCOPE]);
  });
});
