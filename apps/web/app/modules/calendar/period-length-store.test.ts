// The store reads localStorage and holds module-level listeners, so the DOM has
// to exist before the module is imported — installDom() at the top, dynamic
// import in `before`.

import { afterEach, before, test } from 'node:test';
import assert from 'node:assert/strict';
import { installDom } from './components/install-dom';

installDom();

type Store = typeof import('./period-length-store');

let DAYS_PER_PERIOD: number;
let PERIOD_LENGTH_STORAGE_KEY: string;
let getPeriodLength: Store['getPeriodLength'];
let setPeriodLength: Store['setPeriodLength'];
let subscribePeriodLength: Store['subscribePeriodLength'];

before(async () => {
  const layout = await import('./lib/week-layout');
  DAYS_PER_PERIOD = layout.DAYS_PER_PERIOD;
  PERIOD_LENGTH_STORAGE_KEY = layout.PERIOD_LENGTH_STORAGE_KEY;
  const store = await import('./period-length-store');
  getPeriodLength = store.getPeriodLength;
  setPeriodLength = store.setPeriodLength;
  subscribePeriodLength = store.subscribePeriodLength;
});

// The store is a module singleton for the whole file. Reset it so a later test
// never inherits this one's value, and drop the key so the *next* read falls back
// to the default rather than to a value this file wrote.
afterEach(() => {
  setPeriodLength(DAYS_PER_PERIOD);
  window.localStorage.removeItem(PERIOD_LENGTH_STORAGE_KEY);
});

// Every case in one test: the store only re-reads storage while no in-session
// set has happened, so the read path has to be exercised before the setter tests
// below (there is no reset back to "unread" by design).
test('getPeriodLength: reads the stored value, clamped, else the default', () => {
  window.localStorage.removeItem(PERIOD_LENGTH_STORAGE_KEY);
  assert.equal(getPeriodLength(), DAYS_PER_PERIOD, 'missing key → 7');

  window.localStorage.setItem(PERIOD_LENGTH_STORAGE_KEY, '3');
  assert.equal(getPeriodLength(), 3);

  window.localStorage.setItem(PERIOD_LENGTH_STORAGE_KEY, '9');
  assert.equal(getPeriodLength(), DAYS_PER_PERIOD, 'above the max → 7');

  window.localStorage.setItem(PERIOD_LENGTH_STORAGE_KEY, '0');
  assert.equal(getPeriodLength(), 1, 'below the min → 1');

  window.localStorage.setItem(PERIOD_LENGTH_STORAGE_KEY, 'nope');
  assert.equal(getPeriodLength(), DAYS_PER_PERIOD, 'unparsable → 7');
});

test('setPeriodLength: notifies once, persists, and stays silent on a repeat', () => {
  const notified: number[] = [];
  const unsubscribe = subscribePeriodLength(() => {
    notified.push(getPeriodLength());
  });

  setPeriodLength(3);
  assert.equal(getPeriodLength(), 3);
  assert.equal(
    window.localStorage.getItem(PERIOD_LENGTH_STORAGE_KEY),
    '3',
    'persisted',
  );
  assert.deepEqual(notified, [3], 'one notification, carrying the new value');

  setPeriodLength(3);
  assert.deepEqual(notified, [3], 'an unchanged value notifies nobody');
  assert.equal(getPeriodLength(), 3);

  unsubscribe();
});

test('subscribePeriodLength: an unsubscribed listener is never called again', () => {
  let notifications = 0;
  const unsubscribe = subscribePeriodLength(() => {
    notifications += 1;
  });

  setPeriodLength(4);
  assert.equal(notifications, 1);

  unsubscribe();
  setPeriodLength(5);
  assert.equal(notifications, 1, 'dropped listener stays dropped');
  assert.equal(getPeriodLength(), 5, 'the set still applied');
});

test('setPeriodLength: a failed storage write still applies in-session', () => {
  // jsdom's Storage ignores an own property on the instance, so the stub goes on
  // the prototype (verified: assignment there is configurable and writable).
  const proto = window.Storage.prototype;
  const original = proto.setItem;
  const stored = window.localStorage.getItem(PERIOD_LENGTH_STORAGE_KEY);
  proto.setItem = () => {
    throw new Error('QuotaExceededError');
  };

  try {
    assert.doesNotThrow(() => setPeriodLength(2));
    assert.equal(getPeriodLength(), 2, 'the getter still moves');
  } finally {
    proto.setItem = original;
  }

  assert.equal(
    window.localStorage.getItem(PERIOD_LENGTH_STORAGE_KEY),
    stored,
    'a failed write leaves the persisted value alone',
  );
});
