// Home's dnd-kit collision strategy (ADR 0011 § Agenda). The empty Waiting
// target is a strip inside the day header: it sits adjacent to the day pile's
// droppable, so `closestCenter` alone can prefer the strip for a drop aimed at
// the top of the pile. Empty Waiting targets are therefore POINTER-ONLY: they
// win only when the pointer is actually inside the strip. Every other
// droppable (the piles, rows, warehouses and non-empty Waiting sections) keeps
// the normal closest-centre behaviour.
//
// The strip lives in a sticky header, whose stored dnd-kit rect would drift as
// the stream scrolls. Empty targets are hit-tested against the node's LIVE
// viewport rect instead of `droppableRects`, and only while the pointer is
// inside the stream's visible scroll box — a scrolled-out sticky header can
// still report a box beyond the scroller's edge.

import {
  closestCenter,
  type Collision,
  type CollisionDetection,
} from '@dnd-kit/core';

type CollisionArgs = Parameters<CollisionDetection>[0];
type DroppableContainer = CollisionArgs['droppableContainers'][number];

/** True for the transient, empty Waiting drop zone `AgendaDay` renders while
 *  a task row is being dragged. */
function isEmptyWaitingTarget(container: DroppableContainer): boolean {
  const data = container.data.current;
  return data?.waiting === true && data?.empty === true;
}

function pointInside(
  point: { x: number; y: number },
  rect: Pick<DOMRect, 'left' | 'right' | 'top' | 'bottom'>,
): boolean {
  return (
    point.x >= rect.left &&
    point.x <= rect.right &&
    point.y >= rect.top &&
    point.y <= rect.bottom
  );
}

/** Is the pointer inside the target's CURRENT visible box? Reading the DOM
 *  directly keeps a sticky header's target aligned while the stream scrolls;
 *  the scroller's box rejects headers clipped just past either edge. */
function pointerInsideVisibleNode(
  container: DroppableContainer,
  point: { x: number; y: number },
  streamScroller: HTMLElement | null,
): boolean {
  const node = container.node.current;
  if (!node || !streamScroller) return false;
  const rect = node.getBoundingClientRect();
  if (!pointInside(point, rect)) return false;
  return pointInside(point, streamScroller.getBoundingClientRect());
}

/** Resolve one dnd-kit collision frame. `streamScroller` is Home's own
 *  `overflow-y-auto` container; passing it also makes the empty-strip rule
 *  testable without dnd-kit's measuring internals. */
export function agendaCollisionDetection(
  args: CollisionArgs,
  streamScroller: HTMLElement | null,
): Collision[] {
  const emptyWaiting = args.droppableContainers.filter(isEmptyWaitingTarget);
  const point = args.pointerCoordinates;
  if (point && emptyWaiting.length > 0) {
    const waitingHits: Collision[] = emptyWaiting
      .filter((container) =>
        pointerInsideVisibleNode(container, point, streamScroller),
      )
      .map((container) => ({ id: container.id }));
    if (waitingHits.length > 0) return waitingHits;
  }
  // Never let an empty Waiting strip win by proximity. Dropping just below
  // the header must stay a drop on the day pile (or the row under the
  // pointer), not become a hand-off.
  const rest =
    emptyWaiting.length === 0
      ? args.droppableContainers
      : args.droppableContainers.filter(
          (container) => !isEmptyWaitingTarget(container),
        );
  return closestCenter({ ...args, droppableContainers: rest });
}
