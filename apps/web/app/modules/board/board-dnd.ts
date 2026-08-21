import { closestCenter, pointerWithin } from '@dnd-kit/core';
import type { CollisionDetection } from '@dnd-kit/core';

// After the board columns stretch to the leftover viewport height (f5e07c8,
// 3c154e3), `closestCorners` snaps to a neighboring card whose corners are
// closer than the tall empty body under the pointer — a drop aimed at column
// A's empty space can land on a card in column B. Pointer-inside wins here;
// only when the pointer sits in a gap (between columns, or outside them) do
// we fall back to the nearest *column* — never a card.
export type BoardDroppableType = 'task' | 'column';

/** Which pointer-within hit wins: the nearest card (insert at that index),
 *  else the nearest column (append), else nothing. Type wins over array
 *  order — a card nested inside its column beats the column itself. */
export function pickPointerHit(
  hits: ReadonlyArray<{ id: string; type?: BoardDroppableType }>,
): string | null {
  const taskHit = hits.find((hit) => hit.type === 'task');
  if (taskHit) return taskHit.id;
  const columnHit = hits.find((hit) => hit.type === 'column');
  return columnHit ? columnHit.id : null;
}

export const boardCollisionDetection: CollisionDetection = (args) => {
  const pointerHits = pointerWithin(args);
  if (pointerHits.length === 0) {
    // Pointer in a gap or off the board: nearest column only — a card can
    // never win from outside, that is the cross-column snap bug.
    return closestCenter({
      ...args,
      droppableContainers: args.droppableContainers.filter(
        (container) => container.data.current?.type === 'column',
      ),
    });
  }

  // Annotate each hit with its droppable type. The container rides along on
  // the collision itself — the containers array has no `.get` lookup.
  const hits: Array<{ id: string; type?: BoardDroppableType }> =
    pointerHits.map((hit) => ({
      id: String(hit.id),
      type: hit.data?.droppableContainer?.data.current?.type,
    }));

  const picked = pickPointerHit(hits);
  if (picked === null) return pointerHits;
  // Only the picked collision may win — the raw list's first entry could
  // otherwise be a column while the pointer actually sits on its card.
  return pointerHits.filter((hit) => String(hit.id) === picked);
};
