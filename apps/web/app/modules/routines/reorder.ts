// Ordering math for /routines drag-and-drop.
//
// Nothing here touches React or `@dnd-kit/*`: the page hands the drop ids in and
// the mutation paints the resulting id list onto the cached rows, so the same
// rules are exercised by a plain node test.
//
// Only standing `sort_order` moves. Already-materialized Home days are
// intentionally left alone (ADR 0004) — that is why the reorder mutation never
// invalidates the `['agenda']` stream.

import type { RoutineRecord } from '@/app/types';

/** Drop result. `null` means do nothing: missing over id, same id, or either
 *  id not in the list (a stale id must never throw — a drag can outlive a
 *  refresh). */
export function planRoutineReorder(
  routines: readonly RoutineRecord[],
  activeId: string,
  overId: string | null | undefined,
): { ids: string[]; routines: RoutineRecord[] } | null {
  if (overId == null || overId === activeId) return null;
  const from = routines.findIndex((routine) => routine.id === activeId);
  const to = routines.findIndex((routine) => routine.id === overId);
  if (from === -1 || to === -1) return null;

  // Same move as dnd-kit's `arrayMove`: lift the active row out, splice it back
  // in at the over row's index.
  const moved = [...routines];
  const [active] = moved.splice(from, 1);
  moved.splice(to, 0, active);

  // Ranks are positional from here on, even when the inputs were 10/20/30 — the
  // endpoint stores the submitted order, not a merged rank. An item already at
  // its index keeps its object identity so React can skip the row.
  const ranked = moved.map((routine, index) =>
    routine.sort_order === index ? routine : { ...routine, sort_order: index },
  );

  return { ids: ranked.map((routine) => routine.id), routines: ranked };
}

/** Paint a submitted id list onto the cached routines.
 *
 *  Unknown ids are skipped. Routines not named in `ids` stay, appended after
 *  the named ones in their original order — a stale or partial payload can
 *  never hide a row the list is still showing. `sort_order` becomes the index.
 *  Does not mutate the input. */
export function routinesInIdOrder(
  routines: readonly RoutineRecord[],
  ids: readonly string[],
): RoutineRecord[] {
  const byId = new Map<string, RoutineRecord>();
  for (const routine of routines) {
    // First occurrence wins: ids are unique server-side, so this only guards
    // against a duplicated cache entry.
    if (!byId.has(routine.id)) byId.set(routine.id, routine);
  }

  const ordered: RoutineRecord[] = [];
  const used = new Set<string>();
  for (const id of ids) {
    if (used.has(id)) continue;
    const routine = byId.get(id);
    if (!routine) continue;
    used.add(id);
    ordered.push(routine);
  }
  for (const routine of routines) {
    if (used.has(routine.id)) continue;
    used.add(routine.id);
    ordered.push(routine);
  }

  return ordered.map((routine, index) =>
    routine.sort_order === index ? routine : { ...routine, sort_order: index },
  );
}
