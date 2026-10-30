# ADR 0007: Agenda backlog

Status: Accepted
Date: 2026-10-17

> Superseded in part by ADR 0009 (2026-10-30): `OPEN → PLANNED` (including
> create into Planned) parks the row on the backlog sentinel instead of civil
> today, unless the row is already at/after the 10-year horizon. The backlog read is three stretches (dated,
> Planned warehouse, Open warehouse). Reschedule gains
> `{ "unschedule": true }`. A `PLANNED` row at/after the horizon is valid and
> the self-heal does not pull it to today. See
> `docs/adr/0009-planned-agenda-section.md`.

## Context

Home (ADR 0006) puts every living task on exactly one agenda row, and that row
sat on **today** whenever the task was created, started, completed, discarded,
or reopened. `OPEN` (the Board's Backlog column) was included, so the backlog
poured onto today and pinned the stream. Backlog is not being retired.

The ideal this ADR locks: every living task still has exactly one row. The
row's **date follows the task status**. `OPEN` tasks sit far in the future and
render in a Backlog section under the day stream. Everything else sits on a
real day.

## Supersedes

**Supersedes in part ADR 0006 (2026-10-17):**

- The row-placement table, including the row "stop, pause, reorder,
  `OPEN` ↔ `PLANNED`, title edit → unchanged". Stop, plan (`OPEN` →
  `PLANNED`), and unplan (`PLANNED` → `OPEN`) now move the row. Pause, reorder,
  and title edit still do not.
- The default read. Both bounds omitted is no longer the anchor week
  (`anchor_date … anchor_date + 6`). It is `anchor_date … today+6` (no 31-day
  cap) plus a `backlog` array.
- Reschedule. It is no longer "today or later, task status unchanged, no
  `task_logs`". The target must be in `[today, today+10y)`, an `IN_PROGRESS`
  task cannot change day, and rescheduling an `OPEN` task plans it.

**Keeps from ADR 0006 (still valid — 0006 owns these):**

- One agenda row per living task, and a terminal task keeps its history row
  (terminal → terminal leaves it); soft-delete hard-deletes the row.
- `GET /api/agenda` is a pure read. The fill (`*/15` and routine create/edit)
  is what materializes occurrences. Occurrence horizon stays today … today+6.
- Anchor: oldest **past** date that still holds a pending item, else today.
  Pending definition unchanged. Backlog rows are never in the past, so they
  cannot pin the anchor.
- Occurrence rule date is never rewritten; reschedule moves the agenda row
  only; no EXDATE; no RRULE on Google.
- `move-to-today`, within-day `/move`, Play-only-on-today, Pause on a running
  row whatever the date.
- The browser never computes today.

## Decision

### Where a task's row sits

Only a **real** transition moves a row. An idempotent repeat (re-complete,
re-start while already `IN_PROGRESS`, same-status drag, reschedule onto the
date the row already occupies, backlog drop of an `OPEN` task already on the
backlog date) moves nothing.

"Today" is the server's civil today (`user_today`). "Backlog date" is
today+50y, computed at write time (below), not a stored constant. "Appended"
is `max(sort_order)+1` on the destination pile.

| Event                                                                                      | Status →                                                                                            | Row date                                                               |
| ------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------- |
| create (`POST /api/tasks` always creates `OPEN`)                                           | `OPEN`                                                                                              | backlog date (today+50y), appended                                     |
| create into Planned (the web creates `OPEN`, then `POST /api/tasks/:id/move` to `PLANNED`) | `PLANNED`                                                                                           | backlog date, appended — the `/move` is the real transition (ADR 0009) |
| plan (`OPEN` → `PLANNED`, `/move`)                                                         | `PLANNED`                                                                                           | backlog date, appended (ADR 0009)                                      |
| unplan (`PLANNED` → `OPEN`, `/move`)                                                       | `OPEN`                                                                                              | backlog date, appended                                                 |
| real start (`/start`, or `/move` → `IN_PROGRESS`)                                          | `IN_PROGRESS`                                                                                       | today, appended                                                        |
| pause (`IN_PROGRESS` → `PLANNED`)                                                          | `PLANNED`                                                                                           | unchanged                                                              |
| stop (`/stop`, and Board drag In Progress → Open, which is `/move`)                        | `OPEN`                                                                                              | backlog date, appended                                                 |
| real complete / discard                                                                    | terminal                                                                                            | today, appended                                                        |
| reopen → `OPEN`                                                                            | `OPEN`                                                                                              | backlog date, appended                                                 |
| reopen → `PLANNED`                                                                         | `PLANNED`                                                                                           | today, appended                                                        |
| reschedule a task onto a date in `[today, today+10y)`                                      | `OPEN` becomes `PLANNED` and writes a `planned` task log; `PLANNED` stays `PLANNED` with no new log | that date                                                              |
| drop into the backlog section                                                              | `OPEN`; a real `PLANNED` → `OPEN` writes an `unplanned` task log; already `OPEN` writes no log      | backlog date                                                           |
| reorder, title edit                                                                        | unchanged                                                                                           | unchanged                                                              |
| soft delete                                                                                | —                                                                                                   | row hard-deleted                                                       |
| terminal → terminal                                                                        | unchanged status class                                                                              | unchanged (history row stays)                                          |

Placement stays **best-effort** after the status and log writes, same as 0006:
a failure must not 500 a transition whose Google write already landed. The
`*/15` self-heal below repairs a missing or misplaced row. Reschedule and
backlog drop are the exception: the row move is the request, so a placement
failure is returned (the status transition is already committed and idempotent
on retry).

When reschedule or a backlog drop changes status, the task also lands on the
destination **Board** column the way `/move` does with `sort_order` omitted:
append (`max+1` over the mover-excluded pile). No client-supplied board rank.
Already in the destination status: no board re-rank and no log.

### Backlog date and the 10-year line

- Backlog date = the user's civil today plus 50 years, via
  `NaiveDate::checked_add_months(Months::new(600))`. That clamps the day
  (2024-02-29 + 600 months = 2074-02-28). Not `today + 365*50`, not a constant.
- "10 years ahead" is the same helper with 120 months. A date **≥ today+10y**
  is the backlog region. Reschedule's upper bound is exclusive: target must be
  in `[today, today+10y)`.
- Both are computed at the moment of the write or the read, from that call's
  civil today.

### Reschedule (`POST /api/agenda/items/:id/reschedule`)

Body is one of:

- `{ "date": "YYYY-MM-DD", "sort_order"?: n }` — relocate onto that day.
- `{ "backlog": true }` — drop into the backlog. `date` must be absent. A named
  `sort_order` is ignored (Open-column order is the Board's `tasks.sort_order`).

Body errors:

- Both `date` and `backlog: true` → 400 `date and backlog are mutually exclusive`.
- Neither → 400 `date is required`.

Date path (tasks and occurrences):

- Target `< today` → 400 `cannot reschedule into the past` (unchanged).
- Target `≥ today+10y` → 400 `cannot reschedule more than 10 years ahead`.
- Same date as the row → 200 no-op, even if a `sort_order` is named, and even
  if the task is `OPEN` (same-day placement is `/move`; status does not flip).
- Occurrence rules otherwise unchanged: `pending | skipped` only (`skipped` →
  `pending`), `in_progress` / `done` → the existing 400s, agenda row only, rule
  date untouched, two rows of the same routine on one day still legal.
- Task: `IN_PROGRESS` → 400 `cannot reschedule an in_progress task` (pause
  first). `COMPLETED` / `DISCARDED` → 400 `cannot reschedule a terminal task`.
  `OPEN` → `PLANNED` + `planned` log + append Planned column, then the row
  moves. `PLANNED` stays `PLANNED` (no log, no board re-rank) and the row moves.
- No Google write.

Backlog-drop path (tasks only):

- Occurrence (any status) → 400 `cannot move an occurrence to the backlog`.
- `IN_PROGRESS` task → 400 `cannot move an in_progress task to the backlog`.
- Terminal task → 400 `cannot move a terminal task to the backlog`.
- `PLANNED` → `OPEN` + `unplanned` log + append Open column, row to the
  backlog date (appended). Already on the backlog date: status/log still
  happen, the row is not reshuffled.
- Already `OPEN` and already on the backlog date → 200 no-op (no log, no board
  re-rank, row untouched). Already `OPEN` on any other date → row moves to the
  backlog date, no log, no board re-rank.
- No Google write.

### Self-heal (`ensure_task_items`, the `*/15` cron)

Still best-effort, still never on the read path. For each user's living tasks:

- (a) `OPEN` with a row whose date `< today+10y` → move the row to the backlog
  date (appended).
- (b) `PLANNED` or `IN_PROGRESS` with a row whose date `≥ today+10y` → move the
  row to today (appended).
- (c) A living task with no row → place by status: `OPEN` on the backlog date,
  otherwise today, in the existing board order within each destination
  (`IN_PROGRESS`, then `PLANNED`, then `OPEN`; each by `tasks.sort_order`, ties
  by id).
- A dated future `PLANNED` row with `today ≤ date < today+10y` is never touched.
  Past `PLANNED` / `IN_PROGRESS` rows are not touched (they can still pin the
  anchor). A second pass with nothing misplaced writes nothing.
- This is the migration: existing `OPEN` rows move to the backlog date on the
  first tick. No SQL migration.

`move-to-today` is unchanged, so it will still sweep a past `OPEN` row onto
today. The next tick's rule (a) sends that `OPEN` row back to the backlog. That
interaction is accepted.

### The read (`GET /api/agenda`)

Pure read. Response keeps `items`, `from`, `to`, `today`, `time_zone`,
`anchor_date`, `earliest_date`, and gains `backlog` (same `AgendaItemView` shape
as `items`).

- **No params** (blank/whitespace counts as omitted; exactly one of `from`/`to`
  is still 400): `items` = rows with `local_date` in `anchor_date … today+6`
  inclusive, ordered by `local_date`, `sort_order`, `created_at`. No 31-day
  cap — the window runs back to the anchor even when that is more than 31 days.
  `backlog` = rows with `local_date > today+6`, in two stretches:
  1. Dated rows with `today+6 < local_date < today+10y` (rescheduled `PLANNED`
     tasks and occurrences), ordered by `local_date`, then `sort_order`, then
     `created_at`.
  2. Then `OPEN` tasks whose row date is `≥ today+10y`, in **Board order**
     (`tasks.sort_order`, ties by task id).

  A `PLANNED` / `IN_PROGRESS` row still sitting at `≥ today+10y` (the self-heal
  has not run) is omitted until the next tick. That is the accepted
  misplacement window, not a third backlog stretch.

- **`from` and `to` both set** — history only; `backlog` stays empty. Both
  required, `to >= from` and span ≤ 31 inclusive days (unchanged messages);
  both are checked on the **requested** dates, before any clamping, so
  `2026-08-01 … 2026-09-01` is still the span 400 even though it reaches the
  anchor. `to` is then pulled back to `anchor_date - 1` — the anchor day
  itself is on the default read, never in a history page — and if that leaves
  `from > to`, the window lies entirely past the anchor: **200** with empty
  `items`, empty `backlog`, and those resolved bounds. Still a pure read, and
  a stored history param refetched after the anchor moved earlier never fails.
  There is no forward page: the stream ends at today+6.

`from`/`to` in the response are the resolved window (anchor…today+6, or the
history window). `earliest_date` stays `MIN(local_date)` over every row, else
today.

### Web (normative for the later slice, not implemented here)

Home renders the stream from the anchor through today+6 (history pages prepend
strictly before the anchor, as today), then a **Backlog** section: dated rows
grouped by their day (reorder within the day via agenda `/move`), then `OPEN`
rows (reorder via the Board's task `/move` within `OPEN`, so Home and Board
share one order). Drag from the backlog onto a stream day that is today or later
= reschedule (→ `PLANNED` on that day, not necessarily today). Drag a stream
task row into the backlog section = backlog drop. Occurrences and `IN_PROGRESS`
tasks are not dropped there. The reschedule popover is hidden on a running task
row. The date picker's max is today+10y−1, computed with the same month-clamp
as the server, from the server's `today`. No forward paging. Midnight and
visibility refetch stay. The browser still never computes today.

## Consequences

- `OPEN` work leaves the day stream. Home's today is the plan and whatever is
  running or was finished today, not the warehouse.
- An `OPEN` task created, stopped, unplanned, or reopened is not on today, so it
  cannot pin the anchor and it disappears from "today" immediately on stop. That
  is deliberate.
- The default read can be long (anchor far in the past). History before the
  anchor is the only paged read; it is clamped to end at `anchor_date - 1`, so
  a window that lies entirely past the anchor reads as empty rather than
  failing.
- Reschedule is now a status transition for `OPEN` tasks, so the Board's
  Planned column and `task_logs` finally agree with a Home drag onto a day.
- No new table and no migration. The first `*/15` tick relocates existing `OPEN`
  rows.

## Residual risk

- Stop takes the row off today. A task stopped at 23:59 leaves today before the
  user has looked at it.
- The default read is unbounded backwards to the anchor.
- Up to 15 minutes of misplacement after a failed best-effort placement, until
  self-heal. A `PLANNED`/`IN_PROGRESS` row left on the backlog date is missing
  from `GET /api/agenda` during that window.
- A `move-to-today` that sweeps an `OPEN` row onto today is undone by the next
  self-heal tick.
- `checked_add_months` clamps Feb 29, so a backlog date and a +10y bound
  computed on Feb 29 land on Feb 28 of the target year, not Mar 1.
- `earliest_date` still includes orphan rows (0006). A user with no primary
  calendar still uses UTC midnight (0006).
