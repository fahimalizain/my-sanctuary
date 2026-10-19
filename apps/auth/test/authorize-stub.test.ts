import { describe, expect, it } from 'vitest';
import { callWorker } from './helpers';

describe('GET /oauth/authorize', () => {
  // Placeholder for slice 2b, which replaces the stub with the session lookup and consent page.
  it('is not implemented yet', async () => {
    const response = await callWorker('/oauth/authorize');

    expect(response.status).toBe(501);
    expect(await response.text()).toBe('not implemented');
  });
});
