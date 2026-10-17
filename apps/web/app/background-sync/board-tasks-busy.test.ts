import { afterEach, test } from 'node:test';
import assert from 'node:assert/strict';
import {
  getBoardTasksBusy,
  setBoardTasksBusy,
  subscribeBoardTasksBusy,
} from './board-tasks-busy';

afterEach(() => {
  setBoardTasksBusy(false);
});

test('set true notifies once; setting true again is a no-op', () => {
  let calls = 0;
  const unsubscribe = subscribeBoardTasksBusy(() => calls++);

  setBoardTasksBusy(true);
  assert.equal(calls, 1);
  setBoardTasksBusy(true);
  assert.equal(calls, 1);

  unsubscribe();
});

test('set false notifies; unsubscribe stops notifications', () => {
  let calls = 0;
  const unsubscribe = subscribeBoardTasksBusy(() => calls++);

  setBoardTasksBusy(true);
  setBoardTasksBusy(false);
  assert.equal(calls, 2);

  unsubscribe();
  setBoardTasksBusy(true);
  assert.equal(calls, 2);

  setBoardTasksBusy(false);
});

test('getter matches the last set value', () => {
  setBoardTasksBusy(true);
  assert.equal(getBoardTasksBusy(), true);
  setBoardTasksBusy(false);
  assert.equal(getBoardTasksBusy(), false);
});
