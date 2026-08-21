import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  MOUSE_DND_DISTANCE_PX,
  TOUCH_DND_DELAY_MS,
  TOUCH_DND_TOLERANCE_PX,
  pickPointerHit,
} from './board-dnd';

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
