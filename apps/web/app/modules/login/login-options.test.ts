import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  browserTimeZone,
  resolveLoginOptions,
  PROVIDERS_FALLBACK_NOTICE,
} from './login-options';

test('both providers enabled: both controls, no notice', () => {
  assert.deepEqual(resolveLoginOptions({ google: true, standalone: true }), {
    google: true,
    standalone: true,
  });
});

test('standalone only: no Google button', () => {
  assert.deepEqual(resolveLoginOptions({ google: false, standalone: true }), {
    google: false,
    standalone: true,
  });
});

test('google only: no username/password form', () => {
  assert.deepEqual(resolveLoginOptions({ google: true, standalone: false }), {
    google: true,
    standalone: false,
  });
});

test('neither: nothing is rendered (the "no sign-in method" case)', () => {
  assert.deepEqual(resolveLoginOptions({ google: false, standalone: false }), {
    google: false,
    standalone: false,
  });
});

test('providers fetch error: falls back to Google-only with a notice', () => {
  const options = resolveLoginOptions(undefined, new Error('boom'));
  assert.equal(options.google, true);
  assert.equal(options.standalone, false);
  assert.equal(options.fallbackNotice, PROVIDERS_FALLBACK_NOTICE);
});

test('providers fetch error: a stale/partial payload does not weaken the fallback', () => {
  const options = resolveLoginOptions(
    { google: false, standalone: true },
    'nope',
  );
  assert.equal(options.google, true);
  assert.equal(options.standalone, false);
  assert.ok(options.fallbackNotice);
});

test('no providers loaded and no error: renders nothing', () => {
  assert.deepEqual(resolveLoginOptions(undefined), {
    google: false,
    standalone: false,
  });
  assert.deepEqual(resolveLoginOptions(null), {
    google: false,
    standalone: false,
  });
});

test('non-boolean provider values are not treated as enabled', () => {
  const options = resolveLoginOptions({
    google: 'yes',
    standalone: 1,
  } as unknown as { google: boolean; standalone: boolean });
  assert.equal(options.google, false);
  assert.equal(options.standalone, false);
});

test('browserTimeZone: returns a non-empty zone or undefined, never throws', () => {
  const zone = browserTimeZone();
  if (zone !== undefined) {
    assert.equal(typeof zone, 'string');
    assert.ok(zone.length > 0);
  }
});

test('browserTimeZone: a broken Intl yields undefined (registration still works)', () => {
  const intl = Intl as unknown as { DateTimeFormat: unknown };
  const original = intl.DateTimeFormat;
  intl.DateTimeFormat = () => {
    throw new Error('no zone here');
  };
  try {
    assert.equal(browserTimeZone(), undefined);
  } finally {
    intl.DateTimeFormat = original;
  }
});
