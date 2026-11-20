# ADR 0013: Task status and agenda date are independent

Status: Accepted
Date: 2026-11-20

> Consolidates the task-row placement rules from ADR 0006, ADR 0007, ADR 0010,
> and ADR 0011 into one rule: a task's **status** is workflow; its single
> agenda row's `local_date` is **scheduling intent**. The placement tables in
> ADR 0007, ADR 0010, and ADR 0011 are superseded.

## Context

ADR 0006 gave every living task exactly one agenda row. ADR 0007, ADR 0010,
and ADR 0011 then derived that row's date from the task's status: `OPEN` and
`PLANNED` were warehoused at a 50-year sentinel, `IN_PROGRESS`, `WAITING`, and
the terminal statuses landed on civil today, and status-only verbs (`/move`,
`/start`, `/stop`, `/pause`, `/complete`, `/discard`) moved the row.

That conflated two independent facts. Status says where the work is in the
workflow; the row's date says when the user intends to do it. The reported
case was `WAITING -> PLANNED`: the user wanted to keep Thursday's date, but
the status move shelved the row at the sentinel. The coupling also forced
two-write UI flows (status, then date) and made a dated `OPEN` row
unrepresentable. A status-only transition must never rewrite user scheduling
intent.

## Decision

### One row, two meanings

- Every living task has exactly **one** `agenda_items` row of kind `task`
  (ADR 0006's partial unique index). A terminal task may keep its last row as
  history.
- `tasks.status` is **workflow**.
- `agenda_items.local_date` is **scheduling intent**.
- A status transition does not imply a date change. A date change does not
  imply a status change, except for the two deliberate `reschedule` cases
  below.

### Dated and undated rows

Both lines are computed from the user's civil today at write or read time,
with `checked_add_months`, so 29 February clamps to 28 February in the target
year.

| Term                       | Definition                             |
| -------------------------- | -------------------------------------- |
| planning horizon           | civil today + 10 years (`+120 months`) |
| backlog sentinel           | civil today + 50 years (`+600 months`) |
| **dated** row              | `local_date < planning horizon`        |
| **undated**/warehoused row | `local_date >= planning horizon`       |

Undated is a region, not one exact date. A sentinel written yesterday and
`today + 10 years` exactly are both valid warehouse slots; nothing normalizes
them to today's exact sentinel.

### Status-only transitions

These rules cover the `/move` matrix, the raw timer verbs `/start`, `/stop`,
`/pause`, `/complete`, `/discard`, reopen, hand-off, and terminal -> terminal.
They do not cover the explicit scheduling actions below.

| Target status        | Existing dated row | Existing undated row      | No row                           |
| -------------------- | ------------------ | ------------------------- | -------------------------------- |
| `OPEN`               | unchanged          | unchanged (stays undated) | insert on the backlog sentinel   |
| `PLANNED`            | unchanged          | unchanged (stays undated) | insert on the backlog sentinel ¹ |
| `IN_PROGRESS`        | unchanged          | date it today             | insert on civil today            |
| `WAITING`            | unchanged          | date it today             | insert on civil today            |
| `COMPLETED`          | unchanged          | date it today             | insert on civil today            |
| `DISCARDED`          | unchanged          | date it today             | insert on civil today            |
| terminal -> terminal | unchanged          | unchanged (stays undated) | no row inserted                  |

¹ A reopen from a terminal status is the one exception: it routes through the
"date today" helper, so terminal -> `PLANNED` inserts today for a rowless task
and dates an undated row today. Terminal -> `OPEN` uses the sentinel when
rowless and leaves an undated row untouched.

- **Unchanged** means `local_date` and `sort_order` are byte-for-byte
  untouched: no date write, no rank churn, no `updated_at` bump.
- **Date it today** sets `local_date = civil today` and appends at
  `max(sort_order) + 1` on today's pile. **Insert** does the same on a rowless
  task. This is dating an undated task, not moving a real date.
- **Terminal -> terminal** (`COMPLETED <-> DISCARDED`) is a history rewrite
  only: it never inserts an agenda row and never dates an undated one. A dated
  history row is left alone.
- An idempotent repeat is not a transition and changes nothing.

Placement is **best-effort** and runs after the status and log writes: a
placement failure must not 500 a transition whose Google write already
landed. The status write is idempotent, so a retry is safe.

### Self-heal (`ensure_task_items`, the `*/15` fill)

The fill maintains the one-row invariant without overriding a real date:

- A living task with **no row** gets its status default: `OPEN`/`PLANNED` on
  the backlog sentinel, `IN_PROGRESS`/`WAITING` on civil today. Terminal
  statuses are not in the fill's task set.
- An **undated** `IN_PROGRESS`/`WAITING` row is dated today.
- An **undated** `OPEN`/`PLANNED` row is left alone; it is valid warehouse
  work.
- A **dated** row is never moved, whatever the task's status. That includes a
  dated `OPEN` row placed by `move-day-to-today`: the fill must not undo it.
- A second pass with nothing missing or undated writes nothing.

### Explicit scheduling actions

Only these actions move an existing dated row or clear a date. Status
transitions may still date an undated or missing row as described above.

| Action                            | Row                                                              | Status                                                                                                 |
| --------------------------------- | ---------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------ |
| `reschedule { date }`             | moves to `date`, appended; same date is a no-op                  | `OPEN` -> `PLANNED` + `planned` log + Planned-column append; `PLANNED` and `WAITING` keep their status |
| `reschedule { backlog: true }`    | moves to the sentinel                                            | `PLANNED` -> `OPEN` + `unplanned` log + Open-column append; already `OPEN` writes no log               |
| `reschedule { unschedule: true }` | moves to the sentinel                                            | `PLANNED` stays `PLANNED`; no log, no Board re-rank                                                    |
| `move-day-to-today`               | every movable row on a past date moves to today, order preserved | unchanged                                                                                              |
| create                            | inserts on the sentinel                                          | `OPEN`                                                                                                 |
| soft delete                       | row hard-deleted                                                 | -                                                                                                      |

- `reschedule { date }` requires `[today, today + 10y)` and keeps its existing
  rejections: occurrences other than `pending | skipped`, `IN_PROGRESS`
  tasks, and terminal tasks. Rescheduling an `OPEN` task is a **date
  action**; its status flip is deliberate and is not the status-only path.
  Rescheduling `WAITING` stays allowed (ADR 0011).
- `reschedule { backlog: true }` rejects occurrences, `WAITING`,
  `IN_PROGRESS`, and terminal tasks. Shelving a `WAITING` task is a
  `WAITING -> OPEN`/`PLANNED` status move (which preserves its date), then a
  second explicit `backlog` or `unschedule` write.
- `reschedule { unschedule: true }` is `PLANNED` only and keeps that status.
  It is the "keep the warehouse, drop the day" action.
- `move-day-to-today` moves `OPEN`, `PLANNED`, `IN_PROGRESS`, and `WAITING`
  task rows on a past date. It does not change status. Its `OPEN` result is
  legitimate and survives the fill.
- `create` (always `OPEN` + sentinel) and soft delete (row removed) are
  lifecycle actions, not scheduling actions. Reorder
  (`POST /api/agenda/items/:id/move`) changes `sort_order` within a day and
  never `local_date`.

### Read model

The still-valid read decisions from ADR 0007/0010/0011 are kept:

- `GET /api/agenda` is a pure read. No parameters means `items` from
  `anchor_date` through `today + 6` inclusive. The anchor is the oldest past
  date that still holds a pending item, else today; there is no 31-day cap.
  A dated `OPEN`/`PLANNED` row in the window is a normal item in this stream.
- `backlog` holds rows past `today + 6` in three stretches: dated rows below
  the 10-year line (ordered by `local_date`, `sort_order`, `created_at`),
  then `PLANNED` tasks at/after the line in Board order, then `OPEN` tasks
  at/after the line in Board order. `IN_PROGRESS`/`WAITING` tasks and
  occurrences at/after the line are omitted.
- An explicit `from`/`to` read is history only: both bounds are required, the
  span is at most 31 days, `to` is clamped to `anchor_date - 1`, and `backlog`
  is empty.
- Every date carries a **Waiting section** for that day's `WAITING` rows,
  collapsed behind a count; an empty target appears only while dragging and
  only on today/future days (ADR 0011).
- `WAITING` holds no calendar time and closes a running Google event on the
  way in. The Board columns are unchanged from ADR 0011: Backlog, Planned, In
  Progress, Waiting, Done, Discarded.
- Play stays today-only; Pause stays available on a running row whatever its
  date; the browser never computes today.

### Web

Home's optimistic paint mirrors the server: start, complete, hand-off, and a
terminal -> `PLANNED` reopen only date an **undated** row on today; a dated
row keeps its day and rank. Reopen -> `OPEN` preserves the history date and
never dates. Planning (`OPEN -> PLANNED`) never retargets a dated row to the
sentinel. Because `WAITING -> OPEN`/`PLANNED` no longer shelves by itself,
Home sends the `/move` and then an explicit `{ backlog: true }` or
`{ unschedule: true }` reschedule; if the second write fails it repaints the
real status-move result (the old date), not the sentinel.

### MCP

- `move_task` is the status tool. It preserves the agenda date. An undated
  task may be dated today when the target is `IN_PROGRESS`, `WAITING`,
  `COMPLETED`, or `DISCARDED`; targets `OPEN`/`PLANNED` leave it undated (the
  terminal -> `PLANNED` reopen above is the exception); a rowless task gets
  the target default.
- `reschedule_agenda_item` is the explicit date tool: `date` (with optional
  `sort_order`), `backlog: true`, or `unschedule: true`. It is the only tool
  that relocates a dated row or shelves one; `move_task` only dates a row that
  was undated or missing.

### Migration

None. No schema change and no backfill. Existing rows keep their dates,
including dated `OPEN`/`PLANNED` rows and history rows. Dates that earlier
versions already moved to the sentinel or today cannot be recovered; only the
user can reschedule them.

## Consequences

- A dated row may be `OPEN` or `PLANNED` and then appears in the day stream.
  That is scheduling intent, not a misplaced warehouse row.
- Starting or completing a dated task leaves it on its day. Home's today may
  not show it; the Board and Focus still do.
- Status-only transitions no longer invent a date, so `WAITING -> PLANNED`
  keeps Thursday, `PLANNED -> OPEN` keeps the day, and stopping a task keeps
  it where it was.
- The warehouse stretches contain only **undated** `OPEN`/`PLANNED` rows, so
  Home and the Board agree on what "no date" means.
- `WAITING -> OPEN`/`PLANNED` needs two writes on the client to shelve. A
  failed second write leaves the new status with the old date, which is the
  honest result and what Home repaints.
- No backfill; the 10-year line and 50-year sentinel are recomputed from the
  server's civil today, so an old sentinel row remains undated.

## Residual risks

- Reopening a past terminal row keeps the past date; the row can pin the
  anchor until the user reschedules it or completes it again.
- Any status-only move that leaves a living row on a past date — start, stop,
  pause, unplan, hand-off — keeps that pending row pinning the anchor until it
  is rescheduled or finished.
- A dated `OPEN`/`PLANNED` row is easy to mistake for a bug: it is deliberately
  outside the warehouse stretches and inside the day stream.
- Explicit date actions can still change status (`OPEN -> PLANNED` on a dated
  reschedule, `PLANNED -> OPEN` on a backlog drop). A client that only wanted
  a date change must accept the workflow side effect.
- `move-day-to-today` can pull warehouse-era dated rows forward; it cannot
  resurrect a date that was already lost.
- A failed best-effort placement is healed only for a missing row on a living
  task and for an undated `IN_PROGRESS`/`WAITING` row. A missing or undated
  terminal row, or an undated `PLANNED` row left by a failed terminal ->
  `PLANNED` reopen, is not healed automatically.
- `checked_add_months` clamps 29 February, so both the horizon and the sentinel
  can land on 28 February.
