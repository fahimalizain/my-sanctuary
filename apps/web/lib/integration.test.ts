import { test } from 'node:test';
import assert from 'node:assert/strict';
import { integrationType, isStandaloneUser } from './integration';

test('integrationType: standalone is standalone', () => {
  assert.equal(
    integrationType({ integration_type: 'standalone' }),
    'standalone',
  );
  assert.equal(isStandaloneUser({ integration_type: 'standalone' }), true);
});

test('integrationType: google is google', () => {
  assert.equal(integrationType({ integration_type: 'google' }), 'google');
  assert.equal(isStandaloneUser({ integration_type: 'google' }), false);
});

test('integrationType: a missing field (old cached /auth/me) reads as google', () => {
  assert.equal(integrationType({}), 'google');
  assert.equal(isStandaloneUser({}), false);
});

test('integrationType: a signed-out user reads as google', () => {
  assert.equal(integrationType(null), 'google');
  assert.equal(isStandaloneUser(null), false);
});
