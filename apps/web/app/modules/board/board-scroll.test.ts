import { test } from 'node:test';
import assert from 'node:assert/strict';
import { pickVisibleColumn, scrollEdges } from './board-scroll';

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

// ── scrollEdges ─────────────────────────────────────────────────────────

test('scrollEdges: content that fits exactly → no edges', () => {
  // A short list must not grow a shadow just because it fails to divide
  // evenly: scrollTop 0 and no overflow either way.
  assert.deepEqual(scrollEdges(0, 400, 400), { top: false, bottom: false });
});

test('scrollEdges: content shorter than the port → no edges', () => {
  assert.deepEqual(scrollEdges(0, 400, 250), { top: false, bottom: false });
});

test('scrollEdges: overflowing at the top → bottom only', () => {
  assert.deepEqual(scrollEdges(0, 300, 900), { top: false, bottom: true });
});

test('scrollEdges: scrolled to the end → top only', () => {
  assert.deepEqual(scrollEdges(600, 300, 900), { top: true, bottom: false });
});

test('scrollEdges: scrolled in the middle → both edges', () => {
  assert.deepEqual(scrollEdges(300, 300, 900), { top: true, bottom: true });
});

test('scrollEdges: subpixel scrollTop stays under the epsilon', () => {
  assert.deepEqual(scrollEdges(0.5, 300, 900), { top: false, bottom: true });
});

test('scrollEdges: within 1px of the end counts as unscrolled', () => {
  // 199 + 300 = 499, one pixel short of 500 — subpixel rounding, not
  // clipped content.
  assert.deepEqual(scrollEdges(199, 300, 500), { top: true, bottom: false });
});

test('scrollEdges: non-finite measurements → no edges', () => {
  assert.deepEqual(scrollEdges(NaN, 300, 900), { top: false, bottom: false });
  assert.deepEqual(scrollEdges(0, Infinity, 900), {
    top: false,
    bottom: false,
  });
});
