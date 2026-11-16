# ADR 0011: A `WAITING` status

Status: Accepted
Date: 2026-11-16

> Amends ADR 0007's row-placement table and ADR 0010's backlog read: adds a
> sixth task status, `WAITING`, with its own placement rules and a Waiting
> section under every date in the agenda.

## Context

Most of the user's tasks are software work handed to coding agents, several
at a time. While an agent runs, the task is neither in front of the user
(`IN_PROGRESS`) nor parked in the warehouse (`OPEN`/`PLANNED`): it is the
agent's turn.

Today those tasks stay `IN_PROGRESS`. Every start opens a Google Calendar
event, so five parallel agents show as five overlapping events, all claiming
the user's attention at once. `WAITING` gives that state its own status, so
`IN_PROGRESS` means only "my attention is on this" and the calendar only
records time the user actually spent.

The user checks waiting tasks by hand. There is no hand-back signal from
agents, no staleness nudge, and no extra runtime record: the `task_logs` row
written on every transition is enough to measure how long a task waited.

## Terms

This ADR keeps two things apart:

- **Calendar event**: the Google Calendar block that `start` opens and
  pause/stop/complete closes. It records time the user actually spent.
  `WAITING` never has one.
- **Agenda row**: the task's `agenda_items` entry, which says which date the
  task is listed under (a real date, or the backlog sentinel). Every living
  task has one, `WAITING` included. "Row" below always means agenda row.

## Decision

### The status

- New status `WAITING`, a living (non-terminal) status alongside `OPEN`,
  `PLANNED` and `IN_PROGRESS`.
- **No transition restrictions.** Any status can move to `WAITING`, and
  `WAITING` can move to any status, including `COMPLETED`/`DISCARDED →
  WAITING`. A terminal → `WAITING` move writes a single `handed_off` log, not
  `reopened` followed by `handed_off`.
- `WAITING` holds **no calendar time**. `IN_PROGRESS → WAITING` closes the
  running Google event through the same Google-gated path as pause.
- Handing off clears focus. Focus stays `IN_PROGRESS`-only.
- New task log type: `handed_off`, written on every real transition into
  `WAITING`.
- **No metadata.** No `waiting_on` field or other WAITING-only columns; a
  session or PR link goes in the task description. Deferred deliberately; a
  dedicated field would be its own ADR.

### Where the agenda row goes on a transition

**The rule:**

1. A row moves **only on a real status transition**, never on its own. No
   cron, read-time rule or day rollover moves a `WAITING` row forward.
2. A transition **into active or terminal work** (`WAITING`, `IN_PROGRESS`,
   `COMPLETED`, `DISCARDED`) moves the row to **civil today**, appended.
3. A transition **into the backlog** (`OPEN`, `PLANNED`) moves the row to the
   **backlog date** (today+50y), appended.

Rule 3 applies to every transition out of `WAITING`. Two older transitions
predate it and are deliberately left as they are: pause (`IN_PROGRESS →
PLANNED`) leaves the row unchanged, and reopen → `PLANNED` lands on today
(ADR 0007).

| Event                                       | Status →             | Row date               | Log                       |
| ------------------------------------------- | -------------------- | ---------------------- | ------------------------- |
| hand off (any status → `WAITING`)           | `WAITING`            | today, appended        | `handed_off`              |
| take back (`WAITING` → `IN_PROGRESS`)       | `IN_PROGRESS`        | today, appended        | `started` (opens event)   |
| complete / discard from `WAITING`           | terminal             | today, appended        | `completed` / `discarded` |
| shelve (`WAITING` → `OPEN` / `PLANNED`)     | `OPEN` / `PLANNED`   | backlog date, appended | `unplanned` / `planned`   |
| `WAITING` → `WAITING` (same-status drag)    | unchanged            | unchanged (reorder)    | none                      |

Shelving reuses the log named for the destination: `WAITING → PLANNED` writes
`planned`, `WAITING → OPEN` writes `unplanned`. No new log type. `stopped` and
`paused` are not used because they imply a closed calendar event, and
`WAITING` never has one. An `unplanned` log can therefore follow either
`PLANNED` or `WAITING`; the preceding log (`handed_off`) tells them apart.

Placement stays **best-effort** after the status and log writes, as in ADR
0006/0007. "Already on today" is a no-op for the row, as for start.

Consequence: a task handed off Monday and taken back Wednesday ends up on
Wednesday. Monday's Waiting section does not keep a record of it; the
`handed_off` log does.

### Agenda

- Each date in the agenda gets a **Waiting section**, rendered between the
  date header and that day's main list. It lists that day's `WAITING` rows.
- `WAITING` rows are day-bound like `IN_PROGRESS` rows: agenda fill leaves a
  row inside the 10-year horizon where it is.

### Self-heal

Agenda fill (the `*/15` cron) treats `WAITING` like `IN_PROGRESS`: a
`WAITING` task with **no agenda row**, or with its row on the backlog
sentinel, is placed on today. A row already on a real date is never touched.
This is not an automatic move under rule 1: it only completes a hand-off
placement that failed, and lands where the hand-off would have put it. It
never creates a calendar event.

Concretely: one new arm in the fill match (`agenda_fill.rs`) and a
`status_rank` for `WAITING`, ranked after `IN_PROGRESS`.

### Board

- New **Waiting** column between In Progress and Done: Backlog, Planned,
  In Progress, Waiting, Done, Discarded. The usual loop (hand off, review,
  finish) stays between neighbouring columns.
- A drop with no explicit position **prepends** (top of the column), like
  In Progress, Done and Discarded. Same-status drags with a rank are reorders.
- Waiting shows all matches (no cap), like Backlog / Planned / In Progress.

### MCP

- `move_task` accepts `WAITING` in its `status` enum. Its description gains:
  "`WAITING` holds no calendar time; leaving `IN_PROGRESS` for `WAITING`
  closes the Google event."
- No new tool. `reschedule_agenda_item` and `move_agenda_item` need no schema
  change; they inherit the server rules (reschedule allowed, unschedule 400).
- No `hand_back` tool: checking waiting tasks stays manual.

### Reschedule and unschedule

- **Reschedule** of a `WAITING` row is allowed: the row moves to the target
  date (same `[today, today+10y)` window as other reschedules) and the status
  stays `WAITING`. Use case: "the agent finishes tonight, I'll review
  Thursday." This differs from `IN_PROGRESS`, which still rejects reschedule.
- **Unschedule** (`{ "unschedule": true }`) of a `WAITING` row is rejected
  with a 400 telling the client to move the task to Open or Planned instead.
  Under the placement rule, reaching the backlog requires a status change;
  unschedule does not change status, and a `WAITING` task on the backlog
  sentinel is a state that cannot occur any other way.

## Consequences

- `IN_PROGRESS` means only "my attention is on this". Parallel agent runs no
  longer stack overlapping Google events; the calendar records review and
  steering sessions, not agent runtime.
- A review done by completing straight from `WAITING` opens no event, so
  that review time never reaches the calendar. Accepted for quick
  "looks good, merge" reviews; start the task first if the time matters.
- The Waiting section under a date is not a history. A task handed off Monday
  and taken back Wednesday shows only on Wednesday; the `handed_off` log is
  the record.
- Agent runtime is measurable from `task_logs` alone: `handed_off` to the next
  transition. No extra span or flag.
- Nothing nudges a stale `WAITING` task and nothing signals that an agent has
  finished. Checking is manual by design.
- Known inconsistencies, left for later ADRs:
  - Terminal → `WAITING` is one move, but terminal → `IN_PROGRESS` is not
    (reopen still only reaches `OPEN`/`PLANNED`). Restarting a finished task
    takes reopen then start.
  - Placement rule 3 covers every move out of `WAITING`, but pause
    (`IN_PROGRESS → PLANNED`, row unchanged) and reopen → `PLANNED` (row on
    today) keep their ADR 0007 behaviour.
  - `WAITING` rows can be rescheduled; `IN_PROGRESS` rows cannot.
- Touches: the status and log constants in `api-core`, the move matrix and
  Google gate, agenda fill (`status_rank` + one arm), reschedule/unschedule
  validation, the Board columns, the agenda Waiting section, the
  `AGENDA_LIVING_STATUSES` list in the web app, and the `move_task` MCP schema.
