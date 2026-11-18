// The empty Waiting strip lives inside the sticky day header, directly above
// the day pile. `closestCenter` alone would let the strip win a drop aimed at
// the top of the pile, so `agendaCollisionDetection` makes empty Waiting
// targets pointer-only. These tests pin that boundary.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { CollisionDetection } from '@dnd-kit/core';
import { agendaCollisionDetection } from './agenda-collision';

type CollisionArgs = Parameters<CollisionDetection>[0];

/** A fake viewport box for Home's stream scroller. */
function scroller(top = 0, height = 200): HTMLElement {
  return {
    getBoundingClientRect: () => rect(top, height),
  } as unknown as HTMLElement;
}

const STREAM = scroller();

function droppable(
  id: string,
  data: Record<string, unknown> = {},
  liveRect?: ReturnType<typeof rect>,
): CollisionArgs['droppableContainers'][number] {
  return {
    id,
    data: { current: data },
    node: {
      current: liveRect ? { getBoundingClientRect: () => liveRect } : null,
    },
  } as unknown as CollisionArgs['droppableContainers'][number];
}

/** A 100px-wide box starting at `top`; only the fields the collision
 *  algorithms read are populated. */
function rect(top: number, height: number) {
  return {
    left: 0,
    top,
    right: 100,
    bottom: top + height,
    width: 100,
    height,
  };
}

function collisionAt(pointerY: number, withEmptyWaiting = true): CollisionArgs {
  const rects = new Map<string, ReturnType<typeof rect>>([
    ['pile', rect(40, 48)],
  ]);
  if (withEmptyWaiting) rects.set('waiting', rect(0, 40));
  return {
    active: { id: 'dragged' } as CollisionArgs['active'],
    collisionRect: {
      left: 0,
      top: pointerY - 5,
      width: 10,
      height: 10,
    } as CollisionArgs['collisionRect'],
    droppableRects: rects as unknown as CollisionArgs['droppableRects'],
    droppableContainers: [
      ...(withEmptyWaiting
        ? [droppable('waiting', { waiting: true, empty: true }, rect(0, 40))]
        : []),
      droppable('pile'),
    ],
    pointerCoordinates: { x: 50, y: pointerY },
  };
}

test('agenda collision: the empty waiting strip wins only under the pointer', () => {
  const firstAt = (y: number) =>
    agendaCollisionDetection(collisionAt(y), STREAM)[0]?.id;

  // Well inside the strip: hand-off.
  assert.equal(firstAt(20), 'waiting');
  // Just inside the pile, outside the strip. `closestCenter` would prefer the
  // strip's centre here; the pointer-only rule keeps the drop on the pile.
  assert.equal(firstAt(41), 'pile');
  // Below everything: the pile still wins.
  assert.equal(firstAt(100), 'pile');
});

test('agenda collision: a strip clipped out of the stream is not a target', () => {
  // The strip's live box contains the pointer, but the scroller's visible box
  // starts below it (a sticky header scrolled just past the top edge). It
  // must not hand off to a day the user cannot see.
  const args = collisionAt(20);
  assert.equal(
    agendaCollisionDetection(args, scroller(100, 100))[0]?.id,
    'pile',
  );
});

test('agenda collision: identical strip and pile centres still favour the pointer', () => {
  // The reviewer's pathological case: both rects share the same centre. Only
  // a pointer actually inside the strip may pick Waiting; anywhere else the
  // pile keeps the drop.
  const args = collisionAt(20);
  args.droppableRects = new Map([
    ['waiting', rect(0, 48)],
    ['pile', rect(0, 48)],
  ]) as unknown as CollisionArgs['droppableRects'];
  assert.equal(agendaCollisionDetection(args, STREAM)[0]?.id, 'waiting');

  args.pointerCoordinates = { x: 50, y: 100 };
  args.collisionRect = {
    left: 0,
    top: 95,
    width: 10,
    height: 10,
  } as CollisionArgs['collisionRect'];
  assert.equal(agendaCollisionDetection(args, STREAM)[0]?.id, 'pile');
});

test('agenda collision: the empty strip uses its live box, not the cached rect', () => {
  // A stale cached rect from before the sticky header moved must not win.
  const args = collisionAt(20);
  args.droppableRects = new Map([
    ['waiting', rect(200, 40)],
    ['pile', rect(40, 48)],
  ]) as unknown as CollisionArgs['droppableRects'];
  assert.equal(agendaCollisionDetection(args, STREAM)[0]?.id, 'waiting');

  // The reverse: the cache still sits under the pointer, but the live header
  // has moved away. The pile keeps the drop.
  args.droppableContainers = args.droppableContainers.map((container) =>
    container.id === 'waiting'
      ? droppable('waiting', { waiting: true, empty: true }, rect(200, 40))
      : container,
  );
  args.droppableRects = new Map([
    ['waiting', rect(0, 40)],
    ['pile', rect(40, 48)],
  ]) as unknown as CollisionArgs['droppableRects'];
  assert.equal(agendaCollisionDetection(args, STREAM)[0]?.id, 'pile');
});

test('agenda collision: null pointer or missing node never hits a waiting strip', () => {
  const noPointer = collisionAt(20);
  noPointer.pointerCoordinates = null;
  assert.equal(agendaCollisionDetection(noPointer, STREAM)[0]?.id, 'pile');

  const noNode = collisionAt(20);
  noNode.droppableContainers = [
    droppable('waiting', { waiting: true, empty: true }),
    droppable('pile'),
  ];
  assert.equal(agendaCollisionDetection(noNode, STREAM)[0]?.id, 'pile');
});

test('agenda collision: non-empty waiting sections stay closest-centre targets', () => {
  const args = collisionAt(41);
  args.droppableContainers = [
    droppable('waiting', { waiting: true, empty: false }),
    droppable('pile'),
  ];
  assert.equal(agendaCollisionDetection(args, STREAM)[0]?.id, 'waiting');
});

test('agenda collision: no waiting target leaves the old centre behaviour', () => {
  assert.equal(
    agendaCollisionDetection(collisionAt(41, false), STREAM)[0]?.id,
    'pile',
  );
});
