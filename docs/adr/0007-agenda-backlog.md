# ADR 0007: Agenda backlog

Status: Superseded by ADR 0013
Date: 2026-10-17

> Superseded by ADR 0013 (2026-11-20). This ADR introduced the backlog
> sentinel (civil today + 50 years), the 10-year planning horizon, the
> `backlog` array (later three stretches), and the `{ "backlog": true }`
> reschedule form; those survive. Its placement table and its rule that the
> task row's date follows the task status do not: status transitions no
> longer move an existing agenda row. Current placement is in
> `docs/adr/0013-task-status-and-agenda-date.md`.

## Context

ADR 0006 put every living task on exactly one agenda row, and that row sat on
**today** whenever the task was created, started, completed, discarded, or
reopened. `OPEN` (the Board's Backlog column) was included, so the backlog
poured onto today and pinned the stream.

Backlog was not being retired. It needed somewhere to live that is not a
calendar day. This ADR chose a far-future sentinel and a separate read array
rather than a nullable scheduled date or a new status column: every task kept
its one row, warehouse rows could not pin the anchor, and the day stream
stayed a view of dated intent. ADR 0010 later split the warehouse into
Planned and Open stretches; ADR 0013 removed the status-to-date coupling that
this ADR introduced.

## What survives

- The backlog sentinel is civil today + 50 years, computed at write time with
  month arithmetic (29 February clamps to 28 February).
- The 10-year planning horizon separates dated rows from warehoused rows.
- `GET /api/agenda` returns a `backlog` array for rows past `today + 6`;
  history reads carry an empty one. The three stretches (dated, Planned
  warehouse, Open warehouse) are defined in ADR 0013.
- `POST /api/agenda/items/:id/reschedule` accepts `{ "backlog": true }` to
  drop a task into the backlog and unplan a `PLANNED` task to `OPEN`.
- The browser still never computes today; the sentinel and the horizon come
  from the server's civil today.
