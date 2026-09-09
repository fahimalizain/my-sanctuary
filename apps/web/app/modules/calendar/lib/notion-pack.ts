// Notion Calendar timed-event packing (cascade: right-peek + left-steal).
// Spec: notion-event-packing.md — worked examples 1–6 are normative.

/** Start-delta threshold (minutes) for half vs full right-peek / left-steal. */
export const PACK_STEAL_MINUTES = 30;
/** Day-column gutter reserved when left-stealing: steal = f - PACK_PEEK_GUTTER. */
export const PACK_PEEK_GUTTER = 0.05;

/** Zero / invalid duration → end = start + ε (minutes). */
const DURATION_EPS = 0.001;

export interface PackInput {
  id: string;
  startMin: number;
  endMin: number;
}

export interface PackResult {
  id: string;
  leftPercent: number;
  widthPercent: number;
  layerIndex: number;
  leftPixels: number;
}

type Column = {
  colIndex: number;
  items: PackInput[];
};

function effectiveEnd(startMin: number, endMin: number): number {
  return endMin > startMin ? endMin : startMin + DURATION_EPS;
}

/** Half-open [start, end) with zero-duration ε. */
export function intervalsOverlap(
  aStart: number,
  aEnd: number,
  bStart: number,
  bEnd: number,
): boolean {
  const aE = effectiveEnd(aStart, aEnd);
  const bE = effectiveEnd(bStart, bEnd);
  return aStart < bE && bStart < aE;
}

function eventsOverlap(a: PackInput, b: PackInput): boolean {
  return intervalsOverlap(a.startMin, a.endMin, b.startMin, b.endMin);
}

function durationMin(e: PackInput): number {
  return e.endMin - e.startMin;
}

/** G9 seed order: earlier start, then longer duration, then stable. */
function sortSeedOrder(events: PackInput[]): PackInput[] {
  return [...events].sort((a, b) => {
    if (a.startMin !== b.startMin) return a.startMin - b.startMin;
    const d = durationMin(b) - durationMin(a);
    if (d !== 0) return d;
    return 0;
  });
}

function flatItems(columns: Column[]): PackInput[] {
  const out: PackInput[] = [];
  for (const col of columns) {
    for (const item of col.items) out.push(item);
  }
  return out;
}

function findColumnIndex(columns: Column[], event: PackInput): number {
  for (let i = 0; i < columns.length; i++) {
    if (columns[i].items.includes(event)) return i;
  }
  return -1;
}

function columnHasOverlap(col: Column, event: PackInput): boolean {
  return col.items.some(
    (other) => other !== event && eventsOverlap(event, other),
  );
}

function firstOverlapping(
  col: Column,
  event: PackInput,
): PackInput | undefined {
  return col.items.find(
    (other) => other !== event && eventsOverlap(event, other),
  );
}

function overlappingInColumn(col: Column, event: PackInput): PackInput[] {
  return col.items.filter(
    (other) => other !== event && eventsOverlap(event, other),
  );
}

function packCluster(cluster: PackInput[]): PackResult[] {
  // 2. Seed order (G9)
  const seedOrder = sortSeedOrder(cluster);

  // 3–4. Ribbons / merge — Sanctuary defaults: no-op
  const remaining = seedOrder;

  // 5. One column per event
  let columns: Column[] = remaining.map((event, i) => ({
    colIndex: i,
    items: [event],
  }));

  // 6. Left-compact (MOVE into first free earlier column)
  const compactOrder = flatItems(columns);
  for (const e of compactOrder) {
    const homeIdx = findColumnIndex(columns, e);
    if (homeIdx <= 0) continue;
    for (let i = 0; i < homeIdx; i++) {
      if (!columnHasOverlap(columns[i], e)) {
        const home = columns[homeIdx];
        const at = home.items.indexOf(e);
        if (at >= 0) home.items.splice(at, 1);
        columns[i].items.push(e);
        break;
      }
    }
  }

  // 7. Drop empty columns; reindex 0..nCols-1
  columns = columns
    .filter((c) => c.items.length > 0)
    .map((c, i) => ({ colIndex: i, items: c.items }));

  const nCols = columns.length;
  if (nCols === 0) return [];

  // 8. Right-expand (COPY into contiguous later free columns)
  const expandOrder = flatItems(columns).slice().reverse();
  for (const e of expandOrder) {
    const homeIdx = findColumnIndex(columns, e);
    if (homeIdx < 0 || homeIdx === nCols - 1) continue;
    for (let i = homeIdx + 1; i < nCols; i++) {
      if (columnHasOverlap(columns[i], e)) break;
      columns[i].items.push(e);
    }
  }

  // 9. Percents — unique events in startMin order; shared stolen[]
  const unique = sortByStartMin([...new Set(flatItems(columns))]);
  const stolen: PackInput[] = [];
  const f = 1 / nCols;
  const results: PackResult[] = [];

  for (const e of unique) {
    const occupied = columns
      .filter((c) => c.items.includes(e))
      .map((c) => c.colIndex)
      .sort((a, b) => a - b);

    const first = occupied[0]!;
    const last = occupied[occupied.length - 1]!;

    let leftPercent = first / nCols;
    let widthPercent = occupied.length / nCols;
    const layerIndex = first + 1;

    // Right peek — every later column, no early break
    for (let c = last + 1; c < nCols; c++) {
      const other = firstOverlapping(columns[c], e);
      if (other) {
        if (other.startMin - e.startMin <= PACK_STEAL_MINUTES) {
          widthPercent += f * 0.5;
        } else {
          widthPercent += f;
        }
      }
    }

    // Left steal — nearest first; break on any overlap after considering steal
    if (first > 0) {
      for (let c = first - 1; c >= 0; c--) {
        const overlapping = overlappingInColumn(columns[c], e);
        if (
          overlapping.length === 1 &&
          (e.startMin - overlapping[0].startMin > PACK_STEAL_MINUTES ||
            stolen.includes(overlapping[0]))
        ) {
          const steal = f - PACK_PEEK_GUTTER;
          leftPercent -= steal;
          widthPercent += steal;
          stolen.push(e);
        }
        if (overlapping.length > 0) break;
      }
    }

    results.push({
      id: e.id,
      leftPercent,
      widthPercent,
      layerIndex,
      leftPixels: 0,
    });
  }

  return results;
}

function sortByStartMin(events: PackInput[]): PackInput[] {
  return [...events].sort((a, b) => {
    if (a.startMin !== b.startMin) return a.startMin - b.startMin;
    return 0;
  });
}

/**
 * Notion cascade packing for one civil day.
 * Caller clamps each event to the day before packing.
 */
export function packDayEvents(items: PackInput[]): PackResult[] {
  if (items.length === 0) return [];

  // Call-site order: startMin ascending (stable)
  const sorted = sortByStartMin(items);

  // 1. Cluster — greedy connected components
  const clusters: PackInput[][] = [];
  for (const item of sorted) {
    let placed = false;
    for (const cluster of clusters) {
      if (cluster.some((other) => eventsOverlap(item, other))) {
        cluster.push(item);
        placed = true;
        break;
      }
    }
    if (!placed) clusters.push([item]);
  }

  const results: PackResult[] = [];
  for (const cluster of clusters) {
    results.push(...packCluster(cluster));
  }
  return results;
}
