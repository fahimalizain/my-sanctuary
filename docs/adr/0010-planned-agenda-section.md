# ADR 0010: A Planned warehouse section

Status: Superseded by ADR 0013
Date: 2026-10-30

> Superseded by ADR 0013 (2026-11-20). The Planned warehouse (undated
> `PLANNED` rows at/after the 10-year horizon, read as a Board-ordered
> stretch) and `{ "unschedule": true }` survive. Plan no longer relocates an
> existing row. Current placement is in
> `docs/adr/0013-task-status-and-agenda-date.md`.

## Context

ADR 0007 derived a task row's date from its status, so planning an `OPEN`
task (`OPEN -> PLANNED`) moved the row to civil today. A drag to the Board's
Planned column or a plan from Home therefore invented a day the user never
chose: the task appeared in today's stream as if it had been scheduled.

`PLANNED` carries two ideas that were conflated. "I intend to do this" is a
warehouse statement with no day; "I will do this on this day" is a real
calendar slot, and only a reschedule states it. The warehouse already had an
encoding for the first — the 50-year sentinel — so this ADR reused it instead
of adding a nullable scheduled date or a new column.

## What survives

- A `PLANNED` row at/after the 10-year line is valid, undated warehouse work.
- The default read exposes `PLANNED` warehouse rows as their own
  Board-ordered stretch, after dated backlog rows and before Open warehouse
  rows (ADR 0013).
- A living task with no row that becomes `PLANNED` is parked on the sentinel.
- `{ "unschedule": true }` keeps `PLANNED`, writes no log, leaves the Board
  rank alone, and moves the row to the sentinel. It is `PLANNED` only.
- `{ "backlog": true }` unplans `PLANNED` to `OPEN` with an `unplanned` log.
- The web has a separate Planned section above Backlog, and free drag-and-drop
  between the two warehouse groups follows the Board order.

## What changed

ADR 0013 removed the rule that a status move relocates the row. Planning a
dated task now keeps its date; only a rowless living task gets the sentinel. A
`PLANNED` row can therefore be dated and appear in the day stream, and a
dated `OPEN` row can appear in the dated backlog stretch.
