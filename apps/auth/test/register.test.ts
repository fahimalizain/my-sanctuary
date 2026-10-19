import { describe, expect, it } from 'vitest';
import { registerClient } from './helpers';

describe('POST /oauth/register (Dynamic Client Registration, RFC 7591)', () => {
  it('registers a public client and returns its client_id', async () => {
    const client = await registerClient({
      redirect_uris: ['https://client.example/callback'],
      token_endpoint_auth_method: 'none',
      client_name: 'Test',
    });

    expect(client.client_id).toBeTruthy();
    expect(client.client_name).toBe('Test');
    expect(client.redirect_uris).toEqual(['https://client.example/callback']);
    expect(client.token_endpoint_auth_method).toBe('none');
    expect(client.grant_types).toEqual(['authorization_code']);
    expect(client.response_types).toEqual(['code']);
  });
});
