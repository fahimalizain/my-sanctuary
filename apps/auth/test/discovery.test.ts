import { describe, expect, it } from 'vitest';
import { callWorker, ISSUER, json } from './helpers';

describe('GET /.well-known/oauth-authorization-server', () => {
  it('advertises the issuer and the endpoints the public proxy forwards', async () => {
    const response = await callWorker(
      '/.well-known/oauth-authorization-server',
    );

    expect(response.status).toBe(200);
    expect(response.headers.get('content-type')).toContain('application/json');

    const metadata = await json<Record<string, unknown>>(response);
    expect(metadata['issuer']).toBe(ISSUER);
    expect(metadata['authorization_endpoint']).toBe(
      `${ISSUER}/oauth/authorize`,
    );
    expect(metadata['token_endpoint']).toBe(`${ISSUER}/oauth/token`);
    expect(metadata['registration_endpoint']).toBe(`${ISSUER}/oauth/register`);
    expect(metadata['revocation_endpoint']).toBe(`${ISSUER}/oauth/token`);
  });

  it('advertises the MCP client features: PKCE S256, the scope catalogue and CIMD', async () => {
    const response = await callWorker(
      '/.well-known/oauth-authorization-server',
    );
    const metadata = await json<Record<string, unknown>>(response);

    expect(metadata['code_challenge_methods_supported']).toContain('S256');
    expect(metadata['code_challenge_methods_supported']).not.toContain('plain');
    expect(metadata['scopes_supported']).toEqual([
      'mcp:read',
      'mcp:write',
      'offline_access',
    ]);
    expect(metadata['client_id_metadata_document_supported']).toBe(true);
    expect(metadata['response_types_supported']).toEqual(['code']);
    expect(metadata['grant_types_supported']).toContain('refresh_token');
  });
});
