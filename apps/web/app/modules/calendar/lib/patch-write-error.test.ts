// The write-error banner has exactly one subscriber (the calendar page), so
// these tests only pin the single slot and the null-clears-it contract.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  reportPatchWriteError,
  setPatchWriteErrorListener,
} from './patch-write-error';

test('reportPatchWriteError forwards base and error to the listener', () => {
  const seen: Array<{ base: string; err: unknown }> = [];
  const err = new Error('boom');
  setPatchWriteErrorListener((base, e) => seen.push({ base, err: e }));

  reportPatchWriteError("Couldn't save event", err);

  setPatchWriteErrorListener(null);
  assert.deepEqual(seen, [{ base: "Couldn't save event", err }]);
});

test('setting null clears the slot', () => {
  let calls = 0;
  setPatchWriteErrorListener(() => {
    calls += 1;
  });
  setPatchWriteErrorListener(null);

  reportPatchWriteError('base', new Error('late failure'));

  assert.equal(calls, 0, 'the unmounted page must not be notified');
});

test('a second listener replaces the first (single slot, not a list)', () => {
  const first: string[] = [];
  const second: string[] = [];
  setPatchWriteErrorListener((base) => first.push(base));
  setPatchWriteErrorListener((base) => second.push(base));

  reportPatchWriteError('base', null);

  setPatchWriteErrorListener(null);
  assert.deepEqual(first, []);
  assert.deepEqual(second, ['base']);
});

test('reporting with no listener is a no-op', () => {
  setPatchWriteErrorListener(null);
  assert.doesNotThrow(() => reportPatchWriteError('base', new Error('x')));
});
