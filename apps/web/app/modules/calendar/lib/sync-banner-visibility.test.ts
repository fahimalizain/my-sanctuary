import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { SyncHealthBanner } from './sync-health';
import { syncBannerControls } from './sync-banner-visibility';

function banner(overrides: Partial<SyncHealthBanner>): SyncHealthBanner {
  return {
    kind: 'fetch_error',
    message: 'Could not load events',
    showRetry: true,
    showReconnect: false,
    ...overrides,
  };
}

test('a standalone user keeps a real fetch_error Retry', () => {
  assert.deepEqual(syncBannerControls(banner({}), true), {
    retry: true,
    reconnect: false,
  });
});

test('a standalone user never sees the Google reconnect link', () => {
  assert.deepEqual(
    syncBannerControls(
      banner({ kind: 'authorization_required', showReconnect: true }),
      true,
    ),
    { retry: true, reconnect: false },
  );
});

test('a Google user keeps both controls', () => {
  assert.deepEqual(
    syncBannerControls(
      banner({ kind: 'authorization_required', showReconnect: true }),
      false,
    ),
    { retry: true, reconnect: true },
  );
});

test('the banner still decides whether Retry shows', () => {
  assert.deepEqual(syncBannerControls(banner({ showRetry: false }), false), {
    retry: false,
    reconnect: false,
  });
});
