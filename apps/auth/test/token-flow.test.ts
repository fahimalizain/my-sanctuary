import {
  createExecutionContext,
  env,
  waitOnExecutionContext,
} from 'cloudflare:test';
import { afterEach, describe, expect, it, vi } from 'vitest';
import worker from '../src/index';
import { TEST_USER } from './app-mock';
import {
  advertisedPath,
  exchangeCode,
  grantAccess,
  ISSUER,
  json,
  pkce,
  postForm,
  registerClient,
  RESOURCE,
  validateToken,
  type TokenSet,
} from './helpers';

/** Sign in, consent, exchange: a live grant with tokens for the one registered resource. */
async function grantTokens(
  scopes: string[] = ['mcp:read'],
): Promise<{ clientId: string; tokens: TokenSet }> {
  const client = await registerClient();
  const { verifier, challenge } = await pkce();
  const { code } = await grantAccess({
    clientId: client.client_id,
    challenge,
    scopes,
  });
  const tokens = await exchangeCode({
    code,
    verifier,
    clientId: client.client_id,
  });
  return { clientId: client.client_id, tokens };
}

describe('the authorization code flow', () => {
  it('grants the approved scopes and carries the user as the grant props', async () => {
    const client = await registerClient();
    const { verifier, challenge } = await pkce();

    const { redirect, code } = await grantAccess({
      clientId: client.client_id,
      challenge,
      scopes: ['mcp:read', 'mcp:write', 'offline_access'],
    });

    expect(redirect.origin + redirect.pathname).toBe(
      'https://client.example/callback',
    );
    expect(redirect.searchParams.get('state')).toBe('state-1');
    expect(redirect.searchParams.get('iss')).toBe(ISSUER);

    const tokens = await exchangeCode({
      code,
      verifier,
      clientId: client.client_id,
    });

    expect(tokens.token_type).toBe('bearer');
    expect(tokens.scope).toBe('mcp:read mcp:write offline_access');
    // The library's default access-token lifetime, and the window the expiry test advances past.
    expect(tokens.expires_in).toBe(3600);
    expect(tokens.refresh_token).toBeTruthy();
    expect(tokens.resource).toBe(RESOURCE);

    const validated = await validateToken(tokens.access_token);
    expect(validated.status).toBe(200);
    expect(await json(validated)).toEqual({
      userId: TEST_USER.id,
      scope: ['mcp:read', 'mcp:write', 'offline_access'],
      props: {
        id: TEST_USER.id,
        email: TEST_USER.email,
        name: TEST_USER.name,
        picture: TEST_USER.picture,
      },
    });
  });

  it('issues a new access token for a refresh, and it validates', async () => {
    const { clientId, tokens } = await grantTokens();

    const response = await postForm('/oauth/token', {
      grant_type: 'refresh_token',
      refresh_token: tokens.refresh_token!,
      client_id: clientId,
      resource: RESOURCE,
    });

    expect(response.status).toBe(200);
    const refreshed = await json<TokenSet>(response);
    expect(refreshed.access_token).not.toBe(tokens.access_token);

    const validated = await validateToken(refreshed.access_token);
    expect(validated.status).toBe(200);
    expect(await json<{ scope: string[] }>(validated)).toMatchObject({
      scope: ['mcp:read'],
    });
  });

  it('revokes at the advertised endpoint, after which the token stops validating', async () => {
    const { clientId, tokens } = await grantTokens();
    const revocationPath = await advertisedPath('revocation_endpoint');

    const revoked = await postForm(revocationPath, {
      token: tokens.access_token,
      client_id: clientId,
    });
    expect(revoked.status).toBe(200);

    const validated = await validateToken(tokens.access_token);
    expect(validated.status).toBe(401);
    expect(await json(validated)).toEqual({ error: 'invalid_token' });
  });
});

describe('token audiences', () => {
  it('refuses a token that was minted for a different resource', async () => {
    const { tokens } = await grantTokens();
    const other = `${ISSUER}/other`;

    // This Worker declares one resource in production; a token is only ever valid for the
    // resource it was minted for, which is what keeps two of them apart. The handler is called
    // directly so the test can hand it an environment that declares both.
    const twoResources = {
      ...env,
      MCP_RESOURCES: `${RESOURCE},${other}`,
    } as Cloudflare.Env;
    const context = createExecutionContext();

    const response = await worker.fetch(
      new Request(`${ISSUER}/internal/validate`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ token: tokens.access_token, resource: other }),
      }),
      twoResources,
      context,
    );
    await waitOnExecutionContext(context);

    expect(response.status).toBe(401);
    expect(await response.json()).toEqual({ error: 'invalid_token' });
  });
});

describe('token lifetime', () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it('stops validating an access token once the clock passes its lifetime', async () => {
    const { tokens } = await grantTokens();
    const expiry = Date.now() + (tokens.expires_in + 1) * 1000;

    expect((await validateToken(tokens.access_token)).status).toBe(200);

    // Only `Date` is faked: the request still travels the real path to the Worker, because the
    // worker under test and the test file share the isolate.
    vi.useFakeTimers({ toFake: ['Date'] });
    vi.setSystemTime(expiry);

    const expired = await validateToken(tokens.access_token);
    expect(expired.status).toBe(401);
    expect(await json(expired)).toEqual({ error: 'invalid_token' });

    // Back at the real time the same token validates again, which shows the rejection was the
    // stored expiry compared against the clock — not the token's own record disappearing.
    vi.useRealTimers();
    expect((await validateToken(tokens.access_token)).status).toBe(200);
  });
});
