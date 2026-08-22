import { closestCenter, pointerWithin } from '@dnd-kit/core';
import type { CollisionDetection } from '@dnd-kit/core';
import type { TaskStatus } from '@/app/types';
// No cycle: board-model never imports this file.
import {
  COLUMN_ID_PREFIX,
  TERMINAL_COLUMN_CAP,
  destIndexInRemaining,
} from './board-model';

/** Task ids per column — the throwaway drag preview shape (`dragItems`).
 *  Committed state stays in `tasks`; this only exists while a drag runs. */
export type BoardColumnItems = Record<TaskStatus, string[]>;

/** Fresh deep-enough copy (each column array sliced) for the drag-start
 *  snapshot — the preview mutates its own arrays, never the committed ones. */
export function cloneBoardItems(items: BoardColumnItems): BoardColumnItems {
  const copy = {} as BoardColumnItems;
  for (const status of Object.keys(items) as TaskStatus[]) {
    copy[status] = [...items[status]];
  }
  return copy;
}

/** Which column array holds `id`, or null when nothing does. */
export function columnOf(
  items: BoardColumnItems,
  id: string,
): TaskStatus | null {
  for (const status of Object.keys(items) as TaskStatus[]) {
    if (items[status].includes(id)) return status;
  }
  return null;
}

function sameIds(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((id, i) => id === b[i]);
}

/** The whole live cross-column preview policy: where the dragged id sits
 *  while hovering, before any drop. Copy-on-write — returns the SAME
 *  reference when nothing changed so React can skip the setState.
 *
 *  Same-container card hovers are deliberately ignored: the sortable CSS
 *  transforms already preview that motion, and rewriting the array per
 *  hovered card flickers (insert/oscillate). */
export function applyDragOverPreview(
  items: BoardColumnItems,
  activeId: string,
  overId: string,
): BoardColumnItems {
  const source = columnOf(items, activeId);
  if (source === null) return items;

  // Resolve the destination container: a prefixed id targets a column body
  // (append); anything else is a card. Hovering the mover's own ghost or an
  // unknown id rewrites nothing.
  let dest: TaskStatus | null = null;
  let overIsCard = false;
  if (overId.startsWith(COLUMN_ID_PREFIX)) {
    const suffix = overId.slice(COLUMN_ID_PREFIX.length);
    if (Object.hasOwn(items, suffix)) dest = suffix as TaskStatus;
  } else if (overId !== activeId) {
    dest = columnOf(items, overId);
    overIsCard = true;
  }
  if (dest === null) return items;

  const sourceList = items[source];
  const destList = items[dest];
  let nextSource = sourceList;
  let nextDest = destList;
  if (source === dest) {
    // One same-container mutation: the column body itself means "below the
    // pile" — append unless the mover already IS last (empty-body hover).
    if (!overIsCard && sourceList[sourceList.length - 1] !== activeId) {
      nextSource = [...sourceList.filter((id) => id !== activeId), activeId];
    }
  } else {
    // Cross-container: lift out of source, drop into dest at the hovered
    // card's slot (dest cannot hold the mover yet).
    nextSource = sourceList.filter((id) => id !== activeId);
    const at = overIsCard ? destList.indexOf(overId) : destList.length;
    nextDest = [...destList];
    nextDest.splice(at < 0 ? nextDest.length : at, 0, activeId);
    // Terminal columns mirror the ADR drop in the PREVIEW only: the mover
    // evicts the old #20 (it stays in committed tasks) and is never
    // evicted itself.
    if (dest === 'COMPLETED' || dest === 'DISCARDED') {
      while (nextDest.length > TERMINAL_COLUMN_CAP) {
        if (nextDest[nextDest.length - 1] === activeId) {
          nextDest.splice(nextDest.length - 2, 1);
        } else {
          nextDest.pop();
        }
      }
    }
  }

  if (sameIds(nextSource, sourceList) && sameIds(nextDest, destList)) {
    return items;
  }
  // Same container: one key — writing [dest] too would clobber nextSource
  // with the untouched destList (they are the same key).
  if (source === dest) {
    return { ...items, [source]: nextSource };
  }
  return { ...items, [source]: nextSource, [dest]: nextDest };
}

/** destIndex from handleDragEnd → index into `remaining` for resolveSortOrder.
 *  A card/column drop yields a committed-destShown index, which
 *  `destIndexInRemaining` must adjust for the mover's old slot. A self-over
 *  (ghost) drop yields a PREVIEW index — already remaining-shaped (how many
 *  previewed cards sit before the mover) — so it passes through untouched:
 *  hoisting it would walk a same-column tail-append back one slot. */
export function dropInsertIndex(
  destIndex: number,
  oldIndex: number,
  overIsSelf: boolean,
): number {
  return overIsSelf ? destIndex : destIndexInRemaining(destIndex, oldIndex);
}

// DnD activation constraints (ADR 0002 § DnD): the sensor constants live
// here so BoardPage, the comments there and the tests below cannot drift.

/** Mouse activation distance (px). A shorter move is a click-to-edit. */
export const MOUSE_DND_DISTANCE_PX = 8;

/** Touch hold (ms) before a card lifts. A swipe under this window pans. */
export const TOUCH_DND_DELAY_MS = 250;

/** If the finger moves more than this (px) during the delay, cancel the drag. */
export const TOUCH_DND_TOLERANCE_PX = 8;

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
