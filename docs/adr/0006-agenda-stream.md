# ADR 0006: Agenda as a stream

Status: Accepted
Date: 2026-10-17

> Superseded in part by ADR 0007 (2026-10-17), ADR 0010 (2026-10-30), and
> ADR 0013 (2026-11-20). ADR 0007 added the backlog sentinel and widened the
> default read; ADR 0010 split the warehouse into Planned and Open; ADR 0013
> decoupled task status from the agenda date. The task-row placement table
> below and the "row follows the task's status/lifecycle" wording are
> superseded: a status-only transition never moves an existing dated row. The
> stream, the one-row-per-task invariant, the occurrence model, and the read
> contract (as amended) still hold. Current task placement is in
> `docs/adr/0013-task-status-and-agenda-date.md`.

## Context

Home was a **date-scoped page**. `GET /api/agenda?date=` **ensured and seeded**
that one date on read: opening a day nobody had opened created a fresh pile of
`pending` occurrences to deal with one by one, and a day never opened stayed
empty. Tasks reached a day only through the **Add task** picker, so work that
was never pinned never appeared on Home at all. Navigation was prev / today /
next plus a date input — one day at a time, nowhere to see a run of days and
nowhere to drag work between them.

Three failures followed from that shape:

1. **Reading wrote.** A GET could create occurrences and agenda rows, so what
   the user saw depended on which days they happened to open — and paging back
   through a missed week manufactured work to skip.
2. **A task had no single day.** `UNIQUE (user_id, local_date, kind, ref_id)`
   keyed one row per date, so the same task could sit on many days at once;
   "where is this task" had no answer, and removing it from a day was a
   first-class chore.
3. **The Board did not move.** New tasks kept arriving and old ones stayed
   put; Home only ever showed what had been pinned by hand.

The ideal this ADR locks: **Home is one vertical stream of civil days**, and
every living task occupies exactly one of them. The day a task sits on moves
only when the user moves it or finishes the work.

## Supersedes

**Supersedes in part ADR 0004 (2026-08-23):**

- **Seed-on-read.** `GET /api/agenda` no longer ensures a date or seeds a row;
  it is a pure read. Materializing the occurrence horizon belongs to the fill
  (the existing `*/15` cron and routine create/edit).
- **Many agenda rows per task.** Migration `0018_agenda_stream.sql` collapses
  duplicates and pins one task-kind row per task. The add/unpin endpoints
  (`POST /api/agenda/items`, `DELETE /api/agenda/items/:id`), the Add-task
  picker and Remove-from-day are gone.
- **The date-scoped Home.** The `?date=` read, the prev/next/date-picker
  surface, and "one GET returns one day" are replaced by the stream.

**Keeps from ADR 0004 (still valid — 0004 owns these):**

- the **routine / occurrence model** — standing definitions, rule-date
  instances, `routines.rrule` as the two-line blob
- the **occurrence verb matrix** — start / pause / complete / skip / reopen,
  including the Google write each leg implies
- **Google events as time logs** — one living one-shot chip per occurrence,
  never an RRULE
- the **Board's task status machine** — `OPEN`, `PLANNED`, `IN_PROGRESS`,
  `COMPLETED`, `DISCARDED` — and its lifecycle verbs

## Decision

### Home is a stream of days, not a date

One vertically scrolling run of civil days, contiguous, with no gaps: scroll up
for history, down for the future. Opening Home lands on `anchor_date` (below),
and the browser extends the stream a page at a time. There is **no date picker,
no Add task and no Remove from day** — tasks are born on the Board, and a
task's row carries the user's scheduling intent (ADR 0013). An empty day still
renders, as a header and a drop target whenever it is today or later.

### One row per task

Migration `0018_agenda_stream.sql` adds `idx_agenda_items_one_per_task` —
`UNIQUE (user_id, ref_id) WHERE kind = 'task'` — after deleting task rows whose
task is missing or soft-deleted and collapsing duplicates to the greatest
`local_date`. The pre-existing per-date unique key stays —
`UNIQUE (user_id, local_date, kind, ref_id)`: occurrence rows still rely on it,
and two occurrence rows of the same routine on one day remain legal.

Every living task in `OPEN | PLANNED | IN_PROGRESS | WAITING` holds exactly
one row. The index makes that **at most one**; the fill gives every rowless
living task its status default (ADR 0013), so in practice it is exactly one.
**Backlog (`OPEN`) is included.** A terminal task may keep its last row as
history; terminal -> terminal never creates one.

`COMPLETED | DISCARDED` tasks need no agenda row — they reappear when
reopened. A soft-deleted task **hard-deletes** its agenda row, so no ghost can
keep a day pending.

### The day a task's row sits on

**Superseded by ADR 0007, ADR 0010, and ADR 0013.** The task-row placement
table that stood here (create, start, complete, and reopen on today; stop,
pause, and `OPEN` ↔ `PLANNED` unchanged) no longer holds. Current rule: status
is workflow, the row's `local_date` is scheduling intent, and a status-only
transition never moves an existing dated row. See
`docs/adr/0013-task-status-and-agenda-date.md`.

Two invariants still hold:

- Only a **real** transition does placement work (insert a missing row or date
  an undated one). An idempotent re-complete, a re-start while already
  `IN_PROGRESS`, and a same-status drag are not transitions and do nothing.
  "Today" is the server's civil today (below); "appended" is
  `max(sort_order) + 1` on today's pile, so arriving on today never reshuffles
  ranks already there.
- **Unfinished work never moves by itself.** There is no auto-skip, no `missed`
  status, no midnight rollover. A day the user did not finish stays exactly
  where it was until its work is completed, discarded, deleted, or swept with
  "Move all to today".

### Occurrences keep their rule date

`occurrence.local_date` is the **series-instance key — the rule date** — and is
never rewritten. Finishing a late occurrence does not move it. A drag or a
reschedule relocates the **agenda row only**; `routines.rrule` is never touched
and no EXDATE is ever written. The fill's "no membership on any date" check is
what keeps a moved occurrence from re-attaching to its rule date.

### "Pending", locked

A row counts as pending when either:

- it is a task item whose task is living with an open status —
  `OPEN`, `PLANNED`, `IN_PROGRESS` or `WAITING` (ADR 0011); or
- it is an occurrence item whose status is `pending | in_progress` and whose
  routine is living.

`done` and `skipped` are **not** pending. Neither are a soft-deleted routine's
`pending`/`in_progress` leftovers: they are hidden and never count. That
routine's `done`/`skipped` history **stays visible**, read-only, each row
carrying `routine_deleted: true` and borrowing the routine's display fields.

"Pending" is a read-side classification used by the anchor and the
move-to-today sweep — not a column and not a fifth task status.

### Days move forward only

- A drop or a reschedule targets **today or later**; a past target is
  `400 cannot reschedule into the past`. `pending | skipped` occurrences are
  reschedulable (`skipped` becomes `pending` on landing), `in_progress` and
  `done` are not, and neither are terminal tasks.
- `POST /api/agenda/days/:date/move-to-today` sweeps a **past** day's movable
  pending rows onto today in one call, preserving their relative order and
  appending after today's existing ranks. Tasks and `pending` occurrences move;
  `in_progress` occurrences cannot change day and stay put, as do
  `done`/`skipped` rows, terminal tasks, orphans, and a soft-deleted routine's
  leftovers. Today or a future date → 400.
- Within a day, `POST /api/agenda/items/:id/move` is still the reorder.

### The read

`GET /api/agenda` is a **pure read**: zero writes, never an `INSERT`, not even
when a living routine's rule covers the window. Bound rules:

- `from` / `to` are inclusive, `to >= from`, span at most **31 days**.
- **Both omitted → the amended default read:** `anchor_date … today + 6`
  inclusive plus the `backlog` array (ADR 0007, ADR 0010, ADR 0013). The
  original anchor week (`anchor_date … anchor_date + 6`) no longer applies.
- Exactly one supplied → 400.

The response carries `items` (ordered by `local_date`, then `sort_order`, then
`created_at`), the **resolved** `from`/`to` (never the raw query), and:

| Field           | Meaning                                                                        |
| --------------- | ------------------------------------------------------------------------------ |
| `today`         | the server's civil today in `time_zone`                                        |
| `time_zone`     | the primary Google calendar's IANA zone, else `UTC`                            |
| `anchor_date`   | the oldest past date that still holds a pending item, else today               |
| `earliest_date` | `MIN(local_date)` over every row, else today — the browser's back-scroll floor |

`POST /api/agenda/items` and `DELETE /api/agenda/items/:id` are **removed**. A
task leaves the agenda only when it is soft-deleted (the row is hard-deleted).
Completing or discarding keeps the last row as history; there is no manual
add/remove path. A rowless living task is repaired by the fill (ADR 0013).

### The fill

The horizon is **today … today+6** (`OCCURRENCE_HORIZON_DAYS = 7`). Nothing is
ever written before today, and a routine whose `DTSTART` lies in the past is not
backfilled into the past.

- The existing `*/15` cron (`run_agenda_fill_cron`) materializes the horizon for
  every affected user; routine **create** fills the new routine's horizon right
  away. The fill is idempotent: a second pass with unchanged data writes nothing.
- A changed `rrule` reconciles **tomorrow … today+6** for that routine:
  untouched rows the new rule no longer covers are hard-deleted together with
  their agenda rows, then the horizon is refilled — that refill also starts at
  **tomorrow**, so the edit itself never gives today a new occurrence or
  agenda row; the next `*/15` cron tick fills today if the new rule covers it.
  **Today and every earlier date are never deleted or edited.** A
  title/estimate/order-only update does no fill work at all. This replaces
  0004's "rule changes never touch materialized occurrences".
- Deleting a routine hard-deletes that **same untouched future set** and leaves
  today and the past in D1 — where `done`/`skipped` history renders and
  `pending`/`in_progress` leftovers are hidden.
- **Untouched** means the row is still `pending`, with no title override, no
  Google chip, and not rescheduled (its agenda row still sits on its rule date).
  Anything the user has acted on survives a reconcile.
- Task rows are maintained by the lifecycle hooks and the fill, not by the
  read. The fill inserts a row for a rowless living task using its status
  default: `OPEN`/`PLANNED` on the backlog sentinel, `IN_PROGRESS`/`WAITING`
  on today, appended in board order (`tasks.sort_order`, ties by id). It
  never moves a dated task row (ADR 0013). This is the self-healing path; the
  task lifecycle hooks maintain the single row on the normal path.
- **The read path never inserts.** A failed fill leaves gaps until the next tick;
  no GET repairs them.

### Today belongs to the server

The **browser never computes today**. `today` and `time_zone` ride on every
agenda response, and Home's day labels, its anchor and the Play gate are drawn
from them — never from the browser clock.

- **Play** (start) appears only on a row whose day **is** today. A start opens a
  Google log "now", so the log always lands on today.
- **Pause** stays available on a running row whatever the date: a session that
  crosses midnight is still running, and the abort must be able to close it. The
  running row is the one case where a non-today row stays interactive.

## Consequences

- Every living task is on Home without being pinned, so Home and the Board
  finally describe the same set of work. Home is a view of the board, not a
  second place to file tasks.
- `agenda_items` shrinks to one task-kind row per task. Migration `0018` deletes
  task rows whose task is gone, keeps the greatest `local_date` when collapsing
  duplicates, and pins the invariant with the partial unique index.
- Reading is cheap and side-effect free. What the future holds depends on the
  `*/15` fill, not on whether the app was opened.
- The API surface shrinks (two endpoints removed) and the read gains an explicit
  window contract with the anchor and the floor.
- A task can no longer be planned on two days at once; an occurrence still can —
  a daily snooze today→tomorrow remains legal.

## Residual risk

- A stored `rrule` blob that no longer parses is skipped by the fill, never
  fatal: that one routine silently stops producing occurrences until its rule is
  edited. A routine's horizon is only as good as the last successful fill.
- `earliest_date` is `MIN(local_date)` over orphan rows too, so the browser's
  back-scroll floor can sit below the oldest day that actually renders.
- The horizon and the anchor both key off the user's civil today, which falls
  back to UTC when the user has no primary calendar — that user's day boundary is
  UTC midnight.
