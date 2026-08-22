import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  MOUSE_DND_DISTANCE_PX,
  TOUCH_DND_DELAY_MS,
  TOUCH_DND_TOLERANCE_PX,
  applyDragOverPreview,
  cloneBoardItems,
  columnOf,
  dropInsertIndex,
  pickPointerHit,
} from './board-dnd';
import type { BoardColumnItems } from './board-dnd';
import type { TaskStatus } from '../../types';
import { TERMINAL_COLUMN_CAP } from './board-model';

/** Full 5-key board — tests pass only the columns they care about. */
function board(
  entries: Partial<Record<TaskStatus, string[]>>,
): BoardColumnItems {
  return {
    OPEN: [],
    PLANNED: [],
    IN_PROGRESS: [],
    COMPLETED: [],
    DISCARDED: [],
    ...entries,
  };
}

/** A full Done/Discarded pile: `c-0` … `c-19`. */
function cappedPile(): string[] {
  return Array.from({ length: TERMINAL_COLUMN_CAP }, (_, i) => `c-${i}`);
}

// ── DnD activation constants (ADR 0002 § DnD) ───────────────────────────

test('MOUSE_DND_DISTANCE_PX keeps a click a click and a drag a drag', () => {
  assert.equal(MOUSE_DND_DISTANCE_PX, 8);
});

test('TOUCH_DND_DELAY_MS lets a swipe pan before a hold lifts the card', () => {
  assert.equal(TOUCH_DND_DELAY_MS, 250);
});

test('TOUCH_DND_TOLERANCE_PX cancels the lift once the finger moves', () => {
  assert.equal(TOUCH_DND_TOLERANCE_PX, 8);
});

// ── pickPointerHit (which pointer-within hit wins the drop) ──────────────

test('pickPointerHit: no hits resolve to null', () => {
  assert.equal(pickPointerHit([]), null);
});

test('pickPointerHit: only a column resolves to that column', () => {
  assert.equal(
    pickPointerHit([{ id: 'column:OPEN', type: 'column' }]),
    'column:OPEN',
  );
});

test('pickPointerHit: a card beats its parent column (insert, not append)', () => {
  assert.equal(
    pickPointerHit([
      { id: 'task-1', type: 'task' },
      { id: 'column:OPEN', type: 'column' },
    ]),
    'task-1',
  );
});

test('pickPointerHit: a card beats a column even when listed after it', () => {
  assert.equal(
    pickPointerHit([
      { id: 'column:OPEN', type: 'column' },
      { id: 'task-1', type: 'task' },
    ]),
    'task-1',
  );
});

test('pickPointerHit: two cards resolve to the first (nearest) card', () => {
  assert.equal(
    pickPointerHit([
      { id: 'task-1', type: 'task' },
      { id: 'task-2', type: 'task' },
    ]),
    'task-1',
  );
});

test('pickPointerHit: only unknown / missing types resolve to null', () => {
  assert.equal(pickPointerHit([{ id: 'task-1' }]), null);
  assert.equal(pickPointerHit([{ id: 'task-1' }, { id: 'task-2' }]), null);
});

test('pickPointerHit: a column wins over an unknown-type hit', () => {
  assert.equal(
    pickPointerHit([{ id: 'task-1' }, { id: 'column:OPEN', type: 'column' }]),
    'column:OPEN',
  );
});

// ── columnOf (which preview column holds an id) ─────────────────────────

test('columnOf: reports the column holding the id', () => {
  const items = board({ OPEN: ['o-1'], PLANNED: ['p-1'] });
  assert.equal(columnOf(items, 'p-1'), 'PLANNED');
});

test('columnOf: an id in no column resolves to null', () => {
  assert.equal(columnOf(board({ OPEN: ['o-1'] }), 'zzz'), null);
});

// ── cloneBoardItems (the drag-start snapshot) ────────────────────────────

test('cloneBoardItems: deep-equal values, fresh array references', () => {
  const items = board({ OPEN: ['o-1'], PLANNED: ['p-1'] });
  const clone = cloneBoardItems(items);
  assert.deepEqual(clone, items);
  assert.notEqual(clone, items);
  assert.notEqual(clone.OPEN, items.OPEN);
});

test('cloneBoardItems: mutating the clone leaves the original intact', () => {
  const items = board({ OPEN: ['o-1'] });
  const clone = cloneBoardItems(items);
  clone.OPEN.push('x');
  clone.PLANNED.push('y');
  assert.deepEqual(items.OPEN, ['o-1']);
  assert.deepEqual(items.PLANNED, []);
});

// ── applyDragOverPreview (the live cross-column ghost policy) ───────────

test('preview: same-column hover over a sibling card is a no-op (same reference)', () => {
  // Sortable CSS transforms already show that motion; rewriting would flicker.
  const items = board({ OPEN: ['o-1', 'o-2', 'o-3'] });
  assert.equal(applyDragOverPreview(items, 'o-1', 'o-2'), items);
  assert.equal(applyDragOverPreview(items, 'o-3', 'o-1'), items);
});

test('preview: same-column hover on the column body appends the mover to the tail', () => {
  const items = board({ OPEN: ['o-1', 'o-2', 'o-3'] });
  const next = applyDragOverPreview(items, 'o-1', 'column:OPEN');
  assert.notEqual(next, items);
  assert.deepEqual(next.OPEN, ['o-2', 'o-3', 'o-1']);
});

test('preview: same-column hover on the column body with mover already last is a no-op', () => {
  const items = board({ OPEN: ['o-2', 'o-3', 'o-1'] });
  assert.equal(applyDragOverPreview(items, 'o-1', 'column:OPEN'), items);
});

test('preview: hovering the own ghost rewrites nothing', () => {
  const items = board({ OPEN: ['o-1', 'o-2'] });
  assert.equal(applyDragOverPreview(items, 'o-1', 'o-1'), items);
});

test('preview: an unknown over task id rewrites nothing', () => {
  const items = board({ OPEN: ['o-1'] });
  assert.equal(applyDragOverPreview(items, 'o-1', 'not-a-task'), items);
});

test('preview: cross-column over a dest card inserts at that card', () => {
  const items = board({ OPEN: ['o-1'], PLANNED: ['p-1', 'p-2'] });
  const next = applyDragOverPreview(items, 'o-1', 'p-1');
  assert.deepEqual(next.OPEN, []);
  assert.deepEqual(next.PLANNED, ['o-1', 'p-1', 'p-2']);
});

test('preview: cross-column over a dest column body appends', () => {
  const items = board({ OPEN: ['o-1'], PLANNED: ['p-1', 'p-2'] });
  const next = applyDragOverPreview(items, 'o-1', 'column:PLANNED');
  assert.deepEqual(next.OPEN, []);
  assert.deepEqual(next.PLANNED, ['p-1', 'p-2', 'o-1']);
});

test('preview: second call after a cross (same container + dest card) is a no-op', () => {
  const start = board({ OPEN: ['o-1'], PLANNED: ['p-1', 'p-2'] });
  const crossed = applyDragOverPreview(start, 'o-1', 'p-1');
  // The mover now lives in PLANNED — hovering another card there must not
  // rewrite again.
  assert.equal(applyDragOverPreview(crossed, 'o-1', 'p-1'), crossed);
});

test('preview: hovering back into the source restores the id at that card', () => {
  const start = board({ OPEN: ['o-1', 'o-2', 'o-3'], PLANNED: [] });
  const crossed = applyDragOverPreview(start, 'o-1', 'column:PLANNED');
  const back = applyDragOverPreview(crossed, 'o-1', 'o-3');
  assert.deepEqual(back.OPEN, ['o-2', 'o-1', 'o-3']);
  assert.deepEqual(back.PLANNED, []);
});

test('preview: Done insert at head evicts the old tail id, never the mover', () => {
  const items = board({ OPEN: ['m-1'], COMPLETED: cappedPile() });
  const next = applyDragOverPreview(items, 'm-1', 'c-0');
  assert.deepEqual(next.OPEN, []);
  assert.equal(next.COMPLETED.length, TERMINAL_COLUMN_CAP);
  assert.equal(next.COMPLETED[0], 'm-1');
  assert.ok(!next.COMPLETED.includes('c-19'));
  assert.ok(next.COMPLETED.includes('c-18'));
});

test('preview: Done body append keeps 20 with the mover last', () => {
  const items = board({ OPEN: ['m-1'], COMPLETED: cappedPile() });
  const next = applyDragOverPreview(items, 'm-1', 'column:COMPLETED');
  // The mover lands past the cap as the tail; the trim splices out its
  // neighbour (`c-19`) instead of the mover.
  assert.equal(next.COMPLETED.length, TERMINAL_COLUMN_CAP);
  assert.equal(next.COMPLETED[next.COMPLETED.length - 1], 'm-1');
  assert.ok(!next.COMPLETED.includes('c-19'));
});

test('preview: Done insert before the last card still keeps the mover of 20', () => {
  const items = board({ OPEN: ['m-1'], COMPLETED: cappedPile() });
  // Insert at index 19 pushes the pile to 21; the trim pops `c-19`, so the
  // mover ends as the tail and stays in the array.
  const next = applyDragOverPreview(items, 'm-1', 'c-19');
  assert.equal(next.COMPLETED.length, TERMINAL_COLUMN_CAP);
  assert.ok(next.COMPLETED.includes('m-1'));
  assert.ok(!next.COMPLETED.includes('c-19'));
});

// ── dropInsertIndex (destIndex → index into `remaining`) ────────────────

test('dropInsertIndex: column/card append on [mover,A,B] yields remaining.length', () => {
  // Column over passes destShown.length (3), not the preview tail index —
  // hoisted that is 2 = remaining.length → resolveSortOrder appends. Feeding
  // the preview index 2 here would walk back to 1 = "before B".
  assert.equal(dropInsertIndex(3, 0, false), 2);
});

test('dropInsertIndex: self-over preview index passes through (tail append)', () => {
  // Preview [A,B,mover]: the index already counts remaining cards before the
  // mover, so it must stay 2 (append) instead of hoisting to 1.
  assert.equal(dropInsertIndex(2, 0, true), 2);
});

test('dropInsertIndex: self-over unmoved ghost stays on its own slot', () => {
  // BoardPage's same-slot check no-ops this before any performMove.
  assert.equal(dropInsertIndex(0, 0, true), 0);
});

test('dropInsertIndex: card path moving down still walks the index back', () => {
  assert.equal(dropInsertIndex(2, 0, false), 1);
});
