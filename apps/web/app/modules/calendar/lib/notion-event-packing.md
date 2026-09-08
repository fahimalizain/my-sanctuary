# Notion Calendar event packing

Implementation-ready spec for timed (time-grid) and all-day event layout.
Slice 2 must match this document 100% for timed packing. Worked examples are
normative — tests should assert these numbers.

## Source

Reverse-engineered from Notion Calendar desktop bundle
`/tmp/opencode-notion-cal/assets/useStores-CvRKhP7r.js` (timed: `Jme` / `Yme` /
`U9` / `G9` / `W9` / `H9`; all-day: `Xme`; peek threshold `z9 = 30`) and chip CSS
in `App-B44a-2uY.js`. Minified names are not used below.

---

## Inputs and defaults (Sanctuary)

Pack **one civil day at a time**. Caller clamps each event to that day
(`clampMinutesToDay`) before packing.

| Option | Sanctuary default | Notes |
| --- | --- | --- |
| `shouldDisplayItemAsRibbon` | always `false` | Notion: `!!event.holdGroup` only |
| `mergeDisabled` | `true` | No multi-calendar merge |
| `isItemAlwaysOnTop` | always `false` | Notion: drag clones |
| `hiddenDays` | none | Out of scope |
| Month-view snapping | off | Out of scope |

### Input shape (timed)

```ts
type TimedPackInput = {
  id: string;
  startMin: number; // minutes since local midnight
  endMin: number;   // minutes since local midnight
};
```

### Output shape (timed)

```ts
type TimedPackResult = {
  id: string;
  leftPercent: number;  // 0..1 fraction of day column
  widthPercent: number; // 0..1 fraction of day column (may exceed remaining space via peek/steal)
  layerIndex: number;   // 1-based; later columns paint on top
  leftPixels: number;   // ribbon inset; 0 when no ribbons
};
```

---

## Overlap predicate

Intervals are **half-open** `[start, end)`:

```
overlaps(a, b) ⇔ a.start < b.end && b.start < a.end
```

- Touching endpoints do **not** overlap: `[9:00, 10:00)` and `[10:00, 11:00)` → false.
- **Zero / invalid duration:** if `end <= start`, treat as `end = start + ε`
  (Notion extends equal start/end by 1 second before testing; Sanctuary may use
  a small ε such as `0.001` minutes). Point intervals still collide when nested
  inside another interval.
- **Always-on-top:** if either event is always-on-top, they do **not** overlap
  (used for drag clones). Default false → no effect.

---

## Timed packing algorithm

Call-site order: events for **one civil day**, sorted by `startMin` ascending
(stable for ties). Then:

### 1. Cluster

Greedy connected components (first overlapping cluster wins). Equivalent to
interval connected components when pre-sorted by start:

```
clusters = []
for each event e in startMin order:
  find first cluster C where some item in C overlaps e
  if found: append e to C
  else: push new cluster [e]
```

Each cluster is packed independently. Non-overlapping clusters never share
columns or affect each other’s percents.

### 2. Per cluster — sort (column seed order)

Sort cluster events:

1. Earlier `startMin` first
2. Same start → **longer duration** first (`endMin - startMin` descending)
3. Same duration → stable (preserve prior order)

Call this list `seedOrder`.

### 3. Split ribbons

```
ribbons = seedOrder.filter(shouldDisplayItemAsRibbon)
remaining = seedOrder without ribbons
```

Ribbons pack as full-width pixel strips on the left:

- `leftPercent = 0`, `widthPercent = 1`, `layerIndex = 1`
- `leftPixels` = sum of prior overlapping ribbons’ `(ribbonWidth + ribbonGap)`
- Non-ribbon chips get `leftPixels = max over ribbons of (leftPixels + ribbonWidth + ribbonGap)`, or `0` if none

**Sanctuary:** `shouldDisplayItemAsRibbon` is always false → no ribbons,
`leftPixels = 0` for every event. Documented for parity; no-op in slice 2.

### 4. Merge duplicates

When `mergeDisabled` is false, events that `shouldMergeWithOtherEvent` (same
meeting on two calendars) collapse onto a primary; secondaries attach as
`mergedItems`.

**Sanctuary:** `mergeDisabled = true` → skip. Do not implement merge in slice 2.

### 5. Initial columns

One column per remaining event, in `seedOrder` (after ribbon/merge removal):

```
columns = remaining.map((event, i) => ({ colIndex: i, items: [event] }))
```

### 6. Left-compact

Walk events in column-flat order (column 0 items, then column 1, …):

```
for each event e in flatMap(columns, c => c.items):
  let home = column containing e
  if home.colIndex === 0: continue
  for i from 0 to home.colIndex - 1:
    if columns[i] has no item that overlaps e:
      MOVE e into columns[i]   // remove from home, append to columns[i]
      break
```

Move, do not copy. First free earlier column wins.

### 7. Drop empty columns

```
columns = columns.filter(c => c.items.length > 0)
```

After left-compact, empties are only trailing; `colIndex` values remain
`0 .. nCols-1` with no gaps. Let `nCols = columns.length`.

### 8. Right-expand

Walk events in **reverse** flat-map order:

```
for each event e in reverse(flatMap(columns, c => c.items)):
  let home = first column containing e
  if home.colIndex === nCols - 1: continue
  for i from home.colIndex + 1 to nCols - 1:
    if columns[i] contains any item that overlaps e:
      break
    COPY e into columns[i]   // append; leave previous columns unchanged
```

An event may occupy a **contiguous run** of columns. Self is not yet in later
columns during this pass, so the overlap check is against *other* events.

### 9. Percents (cascade — not a partition)

Process events in **`startMin` ascending** order (re-sort the unique event
list). Maintain a per-cluster list `stolen = []` shared across this pass.

For each event `e`:

```
occupied = columns that contain e, sorted by colIndex ascending
first    = occupied[0]
last     = occupied[occupied.length - 1]
f        = 1 / nCols

leftPercent  = first.colIndex / nCols
widthPercent = occupied.length / nCols
layerIndex   = first.colIndex + 1    // 1-based
```

#### Right peek

For each column index `c` from `last.colIndex + 1` to `nCols - 1` (do **not**
break early — peek every later column):

```
other = first item in columns[c] that overlaps e
if other exists:
  if (other.startMin - e.startMin) <= 30:   // threshold = 30 minutes
    widthPercent += f * 0.5
  else:
    widthPercent += f
```

#### Left steal

If `first.colIndex > 0`, walk columns to the left, **nearest first**
(`first.colIndex - 1` down to `0`):

```
overlapping = items in that column that overlap e

if overlapping.length === 1
   AND (
         (e.startMin - other.startMin) > 30
         OR other is already in stolen[]
       ):
  steal = f - 0.05          // 0.05 is 5% of the DAY COLUMN, not of f
  leftPercent  -= steal
  widthPercent += steal
  push e onto stolen[]      // this event, not other

if overlapping.length > 0:
  break                     // do not look further left
```

Notes:

- `stolen` is per-cluster and shared while laying out in startTime order.
- Checking `other ∈ stolen` allows a later event to steal from a neighbor that
  already stole, even when the start delta is ≤ 30 minutes.
- `steal = f - 0.05` can be negative when `nCols > 20`; still apply as written.

### 10. Emit

```
{ id, leftPercent, widthPercent, layerIndex, leftPixels }
```

`leftPixels` is the ribbon inset from step 3 (0 with Sanctuary defaults).

---

## Chip CSS (part of the algorithm)

Geometry and stacking tokens the packer output feeds:

```css
position: absolute;
top: <startMin → px>;
height: calc(<durationPx> - var(--chip-margin-bottom));
/* Sanctuary already subtracts CHIP_MARGIN_BOTTOM=3 inside eventHeightPx */
width: calc(<widthPercent * 100>% - <leftPixels>px - var(--chip-margin-right));
left: <leftPixels>px;
margin-left: <leftPercent * 100>%;
z-index: <selected || manipulating
           ? var(--z-index-grid-selected-item)
           : calc(var(--z-index-grid-foreground) + layerIndex)>;
```

| Token | Value |
| --- | --- |
| `--chip-margin-right` / `CHIP_MARGIN_RIGHT` | `13px` |
| `--chip-margin-bottom` / `CHIP_MARGIN_BOTTOM` | `3px` |

`left` + `margin-left` is intentional: pixel ribbon inset plus percent cascade
within the remaining day column.

---

## Worked numeric examples

Minutes since midnight. `f = 1 / nCols`. Results are exact fractions where
shown; decimals are repeating expansions of those fractions.

### 1. No overlap

Events: `A 9:00–10:00`, `B 11:00–12:00`, `C 14:00–15:00`

Three separate clusters. Each:

| id | leftPercent | widthPercent | layerIndex |
| --- | ---: | ---: | ---: |
| A | 0 | 1 | 1 |
| B | 0 | 1 | 1 |
| C | 0 | 1 | 1 |

### 2. Two overlap

Events: `A 9:00–11:00`, `B 10:00–12:00` → `nCols = 2`, `f = 0.5`

| step | A | B |
| --- | --- | --- |
| seed / columns | col 0 | col 1 |
| left-compact | stays 0 | stays 1 (overlaps A) |
| right-expand | blocked by B | (last col) |
| base percents | L=0, W=0.5, layer=1 | L=0.5, W=0.5, layer=2 |
| right peek | other B, Δstart=60>30 → W+=f → **1** | none |
| left steal | — | other A, Δstart=60>30 → steal=f−0.05=0.45 → L=0.05, W=0.95 |

| id | leftPercent | widthPercent | layerIndex |
| --- | ---: | ---: | ---: |
| A | 0 | 1 | 1 |
| B | 0.05 | 0.95 | 2 |

### 3. Same start, longer first

Events: `Long 9:00–12:00`, `Short 9:00–10:00` → `nCols = 2`, `f = 0.5`

Seed order: Long then Short (longer first).

| id | leftPercent | widthPercent | layerIndex | notes |
| --- | ---: | ---: | ---: | --- |
| Long | 0 | 0.75 | 1 | base W=0.5; half-peek into Short (Δstart=0≤30) → +0.25 |
| Short | 0.5 | 0.5 | 2 | no steal (starts with Long, Δ=0≤30; Long ∉ stolen) |

### 4. Chain

Events: `A 9:00–12:00`, `B 10:00–11:00`, `C 10:30–13:00` → `nCols = 3`, `f = 1/3`

Columns after compact/expand: A∈{0}, B∈{1}, C∈{2} (all pairwise overlap A–B, B–C, A–C).

Layout order A, B, C. `stolen` grows as B then C steal.

| id | leftPercent | widthPercent | layerIndex | derivation |
| --- | ---: | ---: | ---: | --- |
| A | 0 | 1 | 1 | base 1/3; full peek B (Δ=60>30) +1/3; full peek C (Δ=90>30) +1/3 |
| B | 0.05 | 0.5 + 1/3 − 0.05 ≈ **0.7833…** | 2 | base 1/3; half-peek C (Δ=30≤30) +1/6; steal from A (Δ=60>30): steal=1/3−0.05; L=1/3−steal=0.05; W=1/3+1/6+steal |
| C | 1/3 + 0.05 ≈ **0.3833…** | 2/3 − 0.05 ≈ **0.6166…** | 3 | base 1/3; left of col2 is B only; Δ(C,B)=30≤30 but **B ∈ stolen** → steal; L=2/3−(1/3−0.05); W=1/3+(1/3−0.05); stop (do not inspect A) |

Exact:

- B: `left = 0.05`, `width = 0.5 + 1/3 - 0.05 = 5/6 - 0.05`
- C: `left = 1/3 + 0.05`, `width = 2/3 - 0.05`

### 5. B and C share a column

Events: `A 9:00–12:00`, `B 9:00–10:00`, `C 11:00–12:00`

Seed (longer first at same start): A, B, C.
Initial columns: A@0, B@1, C@2.
Left-compact: C does not overlap B → **moves into B’s column**. Drop empty col 2.
`nCols = 2`. Columns: `{0: [A], 1: [B, C]}`.

| id | leftPercent | widthPercent | layerIndex | notes |
| --- | ---: | ---: | ---: | --- |
| A | 0 | 0.75 | 1 | base 0.5; half-peek into col1 (other B, Δstart=0≤30) → +0.25. (C also overlaps A but `find` hits B first.) |
| B | 0.5 | 0.5 | 2 | no steal — starts with A (Δ=0≤30), A ∉ stolen |
| C | 0.05 | 0.95 | 2 | left-steal from A: Δstart=120>30; steal=0.5−0.05=0.45; L=0.5−0.45; W=0.5+0.45 |

### 6. Touching endpoints

Events: `A 9:00–10:00`, `B 10:00–11:00`

Half-open → no overlap → two clusters → both full width:

| id | leftPercent | widthPercent | layerIndex |
| --- | ---: | ---: | ---: |
| A | 0 | 1 | 1 |
| B | 0 | 1 | 1 |

---

## All-day packing

Separate algorithm (Notion month / week all-day band). Sanctuary’s
`packAllDayLanes` already matches the first-fit core; slice 2 need not change
all-day unless a shared overlap helper is extracted for timed packing.

### Algorithm

1. Sort: earlier `startDay` first; same start → **longer span** first
   (`endDay - startDay` descending); stable otherwise.
2. For each event in that order, assign the lowest non-negative `topIndex`
   (lane) such that no already-placed event with an **inclusive** day-range
   overlap occupies that lane:

   ```
   dayRangesOverlap(a, b) ⇔ a.startDay <= b.endDay && b.startDay <= a.endDay
   ```

3. **Overflow** (optional): when `maxItemsPerDay` is set (Notion month /
   collapsed all-day):
   - If the event’s overlapping set in the cluster is larger than
     `maxItemsPerDay` and `topIndex >= maxItemsPerDay - 1`:
     - At `topIndex === maxItemsPerDay - 1`: emit `overflowCount` (`+N`)
     - At higher indices: mark `hidden`
   - Sanctuary week all-day band: no `maxItemsPerDay` unless product asks.

4. Output: `{ id, topIndex }` (Sanctuary: `lane`). Full width of the day span
   is handled by the renderer, not by left/width percents.

---

## What Sanctuary does wrong today

`packDayEvents` is Google-style column packing:

- Partitions into `col / cols / span` equal slices
- No right-peek (`+f` or `+f/2` into later columns)
- No left-steal (`f - 0.05`)
- No `layerIndex` / z-index cascade

Overlaps render as equal spreadsheet columns instead of stacked cards with
peek/steal edges. Slice 2 replaces the timed packer to match this spec; chip
CSS already uses `CHIP_MARGIN_RIGHT` / `CHIP_MARGIN_BOTTOM` and should consume
`leftPercent`, `widthPercent`, `layerIndex`, `leftPixels`.

---

## Implementation checklist (slice 2)

- [ ] Replace `packDayEvents` output with `{ leftPercent, widthPercent, layerIndex, leftPixels }`
- [ ] Half-open overlap + zero-duration ε
- [ ] Cluster → G9 sort → columns → left-compact → drop empty → right-expand → percents
- [ ] Right peek with **30-minute** threshold; no early break
- [ ] Left steal with **`steal = f - 0.05`** (not `0.05 * f`); shared `stolen[]`; break on any overlap after considering steal
- [ ] Percent pass ordered by `startMin`
- [ ] Wire chip `marginLeft` / `width` / `z-index` to packer output
- [ ] Unit tests copy worked examples 1–6 exactly
- [ ] Defaults: no ribbons, no merge, no always-on-top, pack per civil day
- [ ] Leave `packAllDayLanes` behavior unchanged unless sharing an overlap helper
