import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  BOARD_REFRESH_COOLDOWN_MS,
  type BoardRefreshGate,
  shouldRefreshBoard,
} from './board-refresh';

// Baseline gate: visible, idle, last refresh exactly one cooldown ago —
// every test overrides one axis.
function baseGate(overrides: Partial<BoardRefreshGate> = {}): BoardRefreshGate {
  return {
    now: 100_000 + BOARD_REFRESH_COOLDOWN_MS,
    lastRefreshAt: 100_000,
    visible: true,
    busy: false,
    ...overrides,
  };
}

test('shouldRefreshBoard: visible, idle and cooled down → true', () => {
  assert.equal(shouldRefreshBoard(baseGate()), true);
});

test('shouldRefreshBoard: hidden tab never refreshes', () => {
  assert.equal(shouldRefreshBoard(baseGate({ visible: false })), false);
});

test('shouldRefreshBoard: busy board (drag / move / focus in flight) never refreshes', () => {
  assert.equal(shouldRefreshBoard(baseGate({ busy: true })), false);
});

test('shouldRefreshBoard: within the cooldown → false (0ms and 4999ms after last)', () => {
  const last = 100_000;
  assert.equal(
    shouldRefreshBoard(baseGate({ now: last, lastRefreshAt: last })),
    false,
  );
  assert.equal(
    shouldRefreshBoard(baseGate({ now: last + 4_999, lastRefreshAt: last })),
    false,
  );
});

test('shouldRefreshBoard: exactly at the cooldown boundary → true', () => {
  const last = 100_000;
  assert.equal(
    shouldRefreshBoard(
      baseGate({ now: last + BOARD_REFRESH_COOLDOWN_MS, lastRefreshAt: last }),
    ),
    true,
  );
});

test('shouldRefreshBoard: well past the cooldown → true', () => {
  assert.equal(
    shouldRefreshBoard(baseGate({ now: 200_000, lastRefreshAt: 100_000 })),
    true,
  );
});
