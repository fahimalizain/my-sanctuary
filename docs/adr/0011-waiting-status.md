# ADR 0011: A `WAITING` status

Status: Accepted
Date: 2026-11-16

> Amended by ADR 0013 (2026-11-20). The `WAITING` status, its log, its
> agenda/Board surfaces, and its no-calendar-time rule stand. The
> row-placement table and the self-heal placement rules in this ADR are
> superseded: a status transition never moves a dated row, and leaving
> `WAITING` for `OPEN`/`PLANNED` no longer shelves the row by itself. See
> `docs/adr/0013-task-status-and-agenda-date.md`.

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
  `WAITING` can move to any status, including `COMPLETED`/`DISCARDED` ->
  `WAITING`. A terminal -> `WAITING` move writes a single `handed_off` log,
  not `reopened` followed by `handed_off`.
- `WAITING` holds **no calendar time**. `IN_PROGRESS -> WAITING` closes the
  running Google event through the same Google-gated path as pause.
- Handing off clears focus. Focus stays `IN_PROGRESS`-only.
- New task log type: `handed_off`, written on every real transition into
  `WAITING`.
- **No metadata.** No `waiting_on` field or other WAITING-only columns; a
  session or PR link goes in the task description. Deferred deliberately; a
  dedicated field would be its own ADR.

### Placement

**Superseded by ADR 0013.** Status is workflow; the agenda row's date is
scheduling intent. A status transition never moves a dated row: an undated or
missing row is dated by its target default, and `WAITING` follows the same
rule as `IN_PROGRESS`. `WAITING -> OPEN`/`PLANNED` preserves the date, so
shelving requires a second, explicit `{ "backlog": true }` or
`{ "unschedule": true }` reschedule. See
`docs/adr/0013-task-status-and-agenda-date.md`.

Placement stays **best-effort** after the status and log writes, as in ADR
0006/0013.

### Agenda

- Each date in the agenda gets a **Waiting section**, rendered between the
  date header and that day's main list. It lists that day's `WAITING` rows,
  collapsed by default behind a count ("5 tasks in Waiting"); clicking the
  count slides the rows open.
- An empty Waiting section is not permanent copy. While a task row is being
  dragged, today and future days show a dashed drop target so a hand-off
  still has a destination. Past days never show an empty target.
- `WAITING` rows are day-bound: a dated row stays on its day, and only an
  undated or missing row is dated today (ADR 0013).

### Self-heal

Agenda fill (the `*/15` cron) treats `WAITING` like `IN_PROGRESS`: a rowless
`WAITING` task, or one whose row is **undated** at/after the 10-year horizon,
is placed on today. A row already on a real date is never touched. This is
not an automatic move: it only completes a failed hand-off placement, and
lands where the hand-off would have put it. It never creates a calendar event.

Concretely: one arm in the fill match (`agenda_fill.rs`) and a `status_rank`
for `WAITING`, ranked after `IN_PROGRESS`.

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
  closes the Google event." Status moves preserve the agenda date (ADR 0013).
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
  Unschedule is a `PLANNED`-only date action; a `WAITING` task must first move
  to `OPEN`/`PLANNED` (which preserves its date), then be shelved explicitly.

## Consequences

- `IN_PROGRESS` means only "my attention is on this". Parallel agent runs no
  longer stack overlapping Google events; the calendar records review and
  steering sessions, not agent runtime.
- A review done by completing straight from `WAITING` opens no event, so
  that review time never reaches the calendar. Accepted for quick
  "looks good, merge" reviews; start the task first if the time matters.
- A dated `WAITING` row stays on its day across hand-off and take-back. The
  Waiting section under a date is still not a history: the `handed_off` log is
  the record of how long the task waited.
- Agent runtime is measurable from `task_logs` alone: `handed_off` to the next
  transition. No extra span or flag.
- Nothing nudges a stale `WAITING` task and nothing signals that an agent has
  finished. Checking is manual by design.
- Known asymmetries, left for later ADRs:
  - The **reopen** verb reaches only `OPEN`/`PLANNED`; terminal -> `WAITING` is
    a single move. A direct start on a finished task is allowed and opens a new
    event (ADR 0006).
  - `WAITING` rows can be rescheduled; `IN_PROGRESS` rows cannot.
  - Shelving a `WAITING` task takes a status move plus an explicit date
    action; the server does not combine them.
- Touches: the status and log constants in `api-core`, the move matrix and
  Google gate, agenda fill (`status_rank` + one arm), reschedule/unschedule
  validation, the Board columns, the agenda Waiting section, the
  `AGENDA_LIVING_STATUSES` list in the web app, and the `move_task` MCP schema.
