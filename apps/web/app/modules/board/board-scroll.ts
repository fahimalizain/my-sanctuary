// Pure scroll-state helpers for the board. No React, no DOM — unit-tested
// with node:test (same contract as board-filters.ts).

/** Column id with the highest intersection ratio, or null when every ratio
 *  is 0 / missing. Ties keep the earlier id in `order` (strict `>`).
 *  Ids in `ratios` that are not in `order` are ignored.
 *
 *  Why a ratio MAP rather than the current observer batch: below md a
 *  snapped column covers most of the scroller while the next one only
 *  peeks in, and IntersectionObserver only reports targets that crossed a
 *  threshold. Picking the winner from the raw batch would let a 0.15 peek
 *  win over a 0.9 column that did not change this frame, so the caller
 *  merges each batch into a cumulative map and picks here. Ignoring ids
 *  outside `order` keeps a stray/removed column from ever taking the
 *  highlight. */
export function pickVisibleColumn(
  ratios: Readonly<Record<string, number>>,
  order: readonly string[],
): string | null {
  let picked: string | null = null;
  let best = 0;
  for (const id of order) {
    const ratio = ratios[id] ?? 0;
    // Strict `>`: on a tie the earlier column in `order` keeps the
    // highlight, which matches the left-to-right reading of the board.
    if (ratio > best) {
      picked = id;
      best = ratio;
    }
  }
  return picked;
}
