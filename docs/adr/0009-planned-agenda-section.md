# ADR 0009: A Planned warehouse section

Status: Accepted
Date: 2026-10-30

> Amends ADR 0007 (2026-10-17): plan (`OPEN → PLANNED`) no longer lands on
> civil today — the row goes to the backlog sentinel unless it is already
> at/after the 10-year horizon. The backlog read is three stretches instead of
> two, reschedule gains `{ "unschedule": true }`, and a `PLANNED` row at/after
> the horizon is valid rather than a misplacement window.

## Context

ADR 0007 gave every living task exactly one agenda row and moved that row onto
civil today whenever the task became `PLANNED`. A drag on Home, or on the
Board's Planned column (`POST /api/tasks/:id/move`), therefore _invented_ a
day the user never chose: a task dragged out of Backlog appeared in today's
stream as if it had been scheduled.

`PLANNED` carries two meanings that were conflated. "I intend to do this" is a
warehouse statement with no day; "I will do this on this day" is a real
calendar slot, and only a reschedule (or a reopen straight into `PLANNED`)
states it. The warehouse already has an encoding for the first — the 50-year
sentinel — so this ADR reuses it. No nullable scheduled date, no new column, no
SQL migration.

## Decision

### Where a `PLANNED` row sits (delta vs ADR 0007)

| Event                                                               | Status →                                       | Row date                                                                                                           |
| ------------------------------------------------------------------- | ---------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| plan (`OPEN` → `PLANNED`, `/move`)                                  | `PLANNED`                                      | backlog date (today+50y), appended — unless the row is already `>= today+10y`, in which case the row is left alone |
| create into Planned (web: create `OPEN`, then `/move` to `PLANNED`) | `PLANNED`                                      | backlog date — the plan arm; no longer today                                                                       |
| pause (`IN_PROGRESS` → `PLANNED`)                                   | `PLANNED`                                      | unchanged (ADR 0007)                                                                                               |
| reopen → `PLANNED`                                                  | `PLANNED`                                      | today, appended (ADR 0007, unchanged)                                                                              |
| reschedule a task onto a date in `[today, today+10y)`               | `OPEN` → `PLANNED`; `PLANNED` stays (ADR 0007) | that date (ADR 0007, unchanged)                                                                                    |
| `{ "unschedule": true }` on a `PLANNED` task                        | `PLANNED`                                      | backlog date, appended — unless the row is already `>= today+10y` (200 no-op)                                      |

Plan is still a **real** transition: the status flip and the `planned` task
log always happen. Only the agenda row is allowed to be a no-op. `IN_PROGRESS
→ PLANNED` (pause) is untouched, and `COMPLETED`/`DISCARDED → PLANNED` (reopen)
still uses civil today.

### Plan

- The status flip and the `planned` log are committed first; placement is
  **best-effort** like the rest of the lifecycle (`if let Err(_) = …`), because
  the Google work of a transition must not be 500'd by a membership write.
- If the task's row is missing, or sits before the 10-year horizon (`today` is
  in that range), the row moves to the backlog date, appended at
  `max(sort_order)+1` (0 when that pile is empty).
- If the row already sits at/after the horizon, it is left **byte-for-byte**
  alone: no date change, no agenda-rank churn, `updated_at` untouched. That is
  deliberate — yesterday's `+50y` sentinel and exactly `today+10y` are both
  valid parking spots, and retargeting them to today's exact sentinel would
  reshuffle a row for no reason. `place_task_on_backlog` (which always
  retargets today's exact `+50y`) is therefore **not** the planner here.

### Unscheduling (`POST /api/agenda/items/:id/reschedule`)

Body is now one of:

- `{ "date": "YYYY-MM-DD", "sort_order"?: n }` — relocate onto that day.
- `{ "backlog": true }` — drop into the Backlog (unplans to `OPEN`).
- `{ "unschedule": true }` — keep `PLANNED`, move the row off its day.

`{ "unschedule": true }` is mutually exclusive with a **non-blank** `date` and
with `backlog`. A blank/whitespace `date` is ignored, matching the existing
backlog drop. When `date` and `backlog` are both named, the pre-existing
`date and backlog are mutually exclusive` is still the message (checked
first).

On a `PLANNED` task it writes **no** `task_logs` row, does **not** re-rank the
Planned column (`tasks.sort_order` is untouched; a named `sort_order` is
ignored), and moves the agenda row to the backlog date, appended. A row already
at/after the horizon → 200 no-op, byte-for-byte including `updated_at`. `OPEN`
→ 400 `cannot unschedule an open task`; `IN_PROGRESS` → 400 `cannot unschedule
an in_progress task`; `COMPLETED`/`DISCARDED` → 400 `cannot unschedule a
terminal task`; occurrence → 400 `cannot unschedule an occurrence`;
missing/other-user/soft-deleted task → 404 before any row write. A placement
failure is returned: the row move is the request.

### Self-heal (`ensure_task_items`, the `*/15` cron)

Status-specific now, because `PLANNED` is no longer a real-day status:

- `OPEN`, no row → backlog date; `OPEN`, row `< today+10y` → backlog date;
  `OPEN`, row `>= today+10y` → left alone. (ADR 0007, unchanged.)
- `PLANNED`, no row → **backlog date** (not today).
- `PLANNED`, ANY existing row → left alone: past, dated future, and
  at/after the horizon are all valid. A self-heal failure therefore parks the
  task on the sentinel, never on today.
- `IN_PROGRESS`, no row → today; `IN_PROGRESS`, row `>= today+10y` → today
  (the only far-future status that is healed); any other existing row → left
  alone.
- Terminal statuses never enter the task list. The two destination piles are
  appended in board order: today holds only `IN_PROGRESS`; the backlog pile
  holds `PLANNED` first, then `OPEN` (`tasks.sort_order`, ties by id).

### The read (`GET /api/agenda`)

The default read's `backlog` array now has **three** stretches:

1. Dated rows (`today+6 < local_date < today+10y`) in the range query's order
   — never re-sorted.
2. `PLANNED` tasks at/after the horizon, in **Planned-column Board order**
   (`tasks.sort_order`, ties by task id).
3. `OPEN` tasks at/after the horizon, in **Open-column Board order**.

`IN_PROGRESS` tasks and occurrences at/after the horizon stay omitted. A
not-yet-healed `OPEN` row on a real day (`< today+10y`) stays in the dated
stretch. History reads keep an empty `backlog`.

### Web (normative)

- A separate, always-visible **Planned** section above **Backlog**.
- Free drag-and-drop between the two warehouse groups (the Board order is the
  contract; the read never re-sorts by agenda rank).
- Dated `PLANNED` rows stay in their day groups in the stream.
- An **Unschedule** menu action on a dated `PLANNED` task.
- A sentinel `PLANNED` task shows "Currently: Planned" (no date).

## Consequences

- Dragging a task to Planned no longer puts it on today. Home's today is the
  plan the user actually dated, plus what is running or was finished today.
- The two warehouse groups are one Board order each: reordering inside
  Planned/Backlog from Home and from the Board agree.
- `PLANNED` rows already on today (created under ADR 0007) are **not**
  migrated. They are legitimate dated plans until the user reschedules or
  unschedules them.
- No new table, no new column, no SQL migration.
- The read can become longer: the Planned warehouse is another stretch of
  tasks, unbounded in count but never in the past.

## Residual risk

- A failed best-effort plan placement only heals when the row is **missing**:
  the next `*/15` tick parks a rowless `PLANNED` task on the sentinel, never on
  today. A row that a failed plan left on a real date stays there as a dated
  plan — the status flip and log already committed, and the cron deliberately
  leaves every existing `PLANNED` row alone.
- A `PLANNED` row at/after the horizon is now a real read rather than an
  accepted misplacement window, so a genuinely misplaced `PLANNED` plan on the
  sentinel cannot be distinguished from an intentional one. That is the point:
  the user can move it with a reschedule or the Unschedule/Plan affordances.
- The `unschedule` verb acts on the agenda row only. A client that calls it on
  a task it believes is dated will see the task move to the Planned warehouse
  with no log entry to explain the move.
- `checked_add_months` clamps Feb 29 (ADR 0007), so a horizon computed on
  Feb 29 is Feb 28 of the target year.
