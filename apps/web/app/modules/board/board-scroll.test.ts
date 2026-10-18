import { test } from 'node:test';
import assert from 'node:assert/strict';
import { pickVisibleColumn } from './board-scroll';

// The board's real column order (board-model COLUMNS).
const order = ['OPEN', 'PLANNED', 'IN_PROGRESS', 'COMPLETED', 'DISCARDED'];

// ── pickVisibleColumn ───────────────────────────────────────────────────

test('pickVisibleColumn: all zeros → null', () => {
  assert.equal(
    pickVisibleColumn(
      {
        OPEN: 0,
        PLANNED: 0,
        IN_PROGRESS: 0,
        COMPLETED: 0,
        DISCARDED: 0,
      },
      order,
    ),
    null,
  );
});

test('pickVisibleColumn: empty order → null', () => {
  assert.equal(pickVisibleColumn({ OPEN: 1 }, []), null);
});

test('pickVisibleColumn: a single positive ratio → that id', () => {
  assert.equal(pickVisibleColumn({ PLANNED: 0.4 }, order), 'PLANNED');
});

test('pickVisibleColumn: the higher ratio wins over a lower one', () => {
  assert.equal(
    pickVisibleColumn({ OPEN: 0.15, PLANNED: 0.9 }, order),
    'PLANNED',
  );
  // Order does not matter: the second column simply has more area in view.
  assert.equal(
    pickVisibleColumn({ IN_PROGRESS: 1, COMPLETED: 0.2 }, order),
    'IN_PROGRESS',
  );
});

test('pickVisibleColumn: a tie keeps the earlier id in order', () => {
  assert.equal(pickVisibleColumn({ PLANNED: 0.5, OPEN: 0.5 }, order), 'OPEN');
  assert.equal(
    pickVisibleColumn({ DISCARDED: 0.5, COMPLETED: 0.5 }, order),
    'COMPLETED',
  );
});

test('pickVisibleColumn: an id present in ratios but absent from order is ignored', () => {
  assert.equal(pickVisibleColumn({ GHOST: 1, OPEN: 0.2 }, order), 'OPEN');
  assert.equal(pickVisibleColumn({ GHOST: 1 }, order), null);
});

test('pickVisibleColumn: a missing id is treated as 0', () => {
  // `PLANNED` is absent from the map, so `OPEN` at any positive ratio wins.
  assert.equal(pickVisibleColumn({ OPEN: 0.01 }, order), 'OPEN');
  // …and a map of only zero-valued ids stays at null.
  assert.equal(pickVisibleColumn({ COMPLETED: 0 }, order), null);
});
