# ADR 0002: Task recurrence

Status: Accepted
Date: 2026-08-18

## Context

Tasks today are one-shot and terminal: a task goes `OPEN` → start/stop/pause → `COMPLETED` / `DISCARDED`. Completing a row kills it, and there is no due date. `calendar_events.recurrence` already stores Google RRULE strings, but we are **not** using that: Google Calendar is the history / log of what happened — the timer's `start_task` writes a one-shot event tagged with `sanctuary_task_id` — and recurring Google events are rejected. Editing or deleting a future occurrence in Google is an easy mess.

Recurrence belongs on tasks. `docs/PROJECT_VISION.md` already lists optional due date + recurrence on tasks. This ADR is slice 1 of 1: the model was locked in a design grilling on 2026-08-18. Nothing is implemented yet; later slices implement against this document.

## Decision

Recurrence lives on a new `task_series` object; every occurrence is an ordinary row in the existing `tasks` table. The following spec is the source of truth for all later slices.

Schema conventions for this model: IDs are UUIDv4 TEXT; timestamps are RFC 3339 TEXT; civil dates are `YYYY-MM-DD` TEXT, **never** instants (no time zone, no time of day); booleans are INTEGER 0/1; domain tables soft-delete via `deleted_at`.

### Out of scope

- No RRULE on Google Calendar events. Never expand `calendar_events.recurrence`; never create recurring Google events from a series. `start_task` remains a one-shot actual tagged with `sanctuary_task_id`.
- No Consistency-rule schedules in this ADR (separate consumer later).
- No hourly recurrences, no `BYSETPOS`, no "third Tuesday", no raw user-authored RRULE text accepted from the client, no EXDATE lists.
- No series chrome on Lists: no grouping header, no repeat badge.
- No due **time** — civil date only.
- Implementation (repositories, HTTP, cron wiring, UI) is later slices.

### Two objects: the series and the child

- **`task_series`** (new table; use this name, not `task_recurrences`) is the template. It is **not** a task: it has no status, is not startable, is not completable, and is never shown as a Lists card.
- **`tasks`** (existing table) gains child semantics: a task row with `series_id` set is one materialized occurrence of a series.

### Table `task_series`

| Column             | Type    | Constraints                  | Purpose                                                        |
| ------------------ | ------- | ---------------------------- | -------------------------------------------------------------- |
| `id`               | TEXT    | PK (UUIDv4)                  | Series identity                                                |
| `user_id`          | TEXT    | NOT NULL → `users.id`        | Owning user                                                    |
| `title`            | TEXT    | NOT NULL                     | Title copied onto every child                                  |
| `description`      | TEXT    | NULL                         | Shared description for created children                        |
| `duration_minutes` | INTEGER | NOT NULL                     | Default duration of each occurrence                            |
| `priority`         | TEXT    | NOT NULL                     | Default priority of each occurrence                            |
| `rrule`            | TEXT    | NOT NULL                     | Compiled RRULE string; written by the server only              |
| `timezone`         | TEXT    | NOT NULL                     | IANA timezone; defines dtstart, "today", and rule evaluation   |
| `dtstart`          | TEXT    | NOT NULL (civil `YYYY-MM-DD`) | First occurrence date; always **today** at create              |
| `until`            | TEXT    | NULL (civil `YYYY-MM-DD`)    | End date, when "end on a date" is chosen                       |
| `count`            | INTEGER | NULL                         | Occurrence count N, when "end after N occurrences" is chosen   |
| `created_at`       | TEXT    | NOT NULL                     | Row creation time                                              |
| `updated_at`       | TEXT    | NOT NULL                     | Last change time                                               |
| `deleted_at`       | TEXT    | NULL                         | Non-NULL = stop repeating                                      |

Rules:

- Where this lock is silent, nullability and defaults of `task_series` mirror the corresponding `tasks` column (`description` optional; `duration_minutes` and `priority` use the task defaults).
- At most one of `until` / `count` is set. Both NULL means repeat forever (until stopped).
- **Soft-delete is the stop-repeating action.** The cron must skip soft-deleted series (`deleted_at IS NULL`); a soft-deleted series must never materialize another child.

### `tasks` gains three columns

| Column           | Type | Constraints                          | Purpose                                              |
| ---------------- | ---- | ------------------------------------ | ---------------------------------------------------- |
| `due_date`       | TEXT | NULL (civil `YYYY-MM-DD`)            | Optional on one-shot tasks; **required** on children |
| `series_id`      | TEXT | NULL → `task_series.id`              | Parent series                                        |
| `occurrence_date`| TEXT | NULL (civil `YYYY-MM-DD`)            | The slot's date; immutable once set                  |

Rules:

- `due_date` is a civil date, never an instant. One-shot tasks may carry it with no series.
- `occurrence_date` is **immutable once set**; it is required iff `series_id` is set.
- Child identity: `UNIQUE(series_id, occurrence_date)` — **including soft-deleted rows**. This must not be a partial unique over living rows: a slot is created at most once, ever. (Rows without a series carry NULL `series_id` and are unaffected by the constraint.)

### Debt model

- Every materialized child is created `OPEN`.
- The user owes every still-`OPEN` child until they `complete` or `discard` it.
- Reuse the existing `DISCARDED` status as the write-off. Do **not** add a `SKIPPED` status.
- Completing a child does **not** complete the series and does **not** spawn the next child — the materializer does.

### Materializer

One pure function: given a living series and "today" in the series timezone, insert any missing slots from `dtstart` through today (inclusive) whose civil dates match the compiled RRULE.

- **Idempotent:** if a row exists for `(series_id, occurrence_date)` in any status — including soft-deleted — skip that slot. Never resurrect a discarded or deleted child.
- **Never materialize future dates:** `occurrence_date` > today is never inserted.
- `dtstart` at create is **always today** (series timezone). A past `dtstart` is a **400**.
- Catch-up covers only days from that create-time `dtstart` through today: if cron was down for a week, those days appear as `OPEN` children. There is no historical backfill.
- A series whose end (`until` / `count`) is passed yields no further slots.

Callers:

1. Creating a series — or adopting a one-shot into a series — runs the materializer for **that** series before the response returns.
2. The existing Worker 15-minute cron runs it for **every** living series (unattended). The cron is the sweeper, not the only writer.

### Cadence (v1)

Closed picker; the **server compiles** the choices into the stored RRULE string. Never accept raw RRULE text from the client.

- Daily.
- Every N days.
- Weekdays (MO–FR).
- Weekly, selected days, optional interval (e.g. every 2 weeks).
- Monthly on a day-of-month: 1–28 or the last day. **Not** 29/30/31.
- End: never / on a civil date / after N occurrences.

No hourly. No EXDATE — skip an individual day by `DISCARDED`-ing that child or by postponing that child's `due_date`.

### Lists and due dates

- Lists stays a **flat** list of ordinary task cards. No series header, no repeat icon.
- `due_date` is a normal task field, like priority — that is how 3 March differs from today.
- Category classification stays on **title**, unchanged. Never bake the date into the title.

### Editor split (`TaskModal`)

- Title / description / duration / priority / `due_date` → **this child only**.
- Repeat / until / stop repeating → **the series**.
- **Postponement:** `occurrence_date` never moves; `due_date` may. Two siblings may share a `due_date` (postponed 3 Mar + real 10 Mar). Cron still emits the real 10 Mar slot.
- **Cadence change:** existing children stay as-is, including `OPEN` days that no longer match the new rule. The cron applies the new rule only to slots it has not created yet.
- **Stop repeating:** soft-delete the series only. Existing children are untouched; no new children appear.

### One-shot → series

Setting Repeat on an ordinary `OPEN` task creates a series with `dtstart = today` and adopts that row as today's child: `series_id` = the new series, `occurrence_date = today`, and `due_date = today` if it was empty. No historical backfill.

### Timer / Google

Unchanged: starting a task writes a one-shot Google event; one running task per user. Google is actuals, not the schedule.

### Residual risk / open items (documented, not grilled in this ADR)

These were left open in the grilling; later slices must decide, not assume a lock:

- Timezone source after create — recommended default is the user's primary calendar `time_zone`, else UTC — and whether the timezone is editable.
- Home / timeline: whether dated `OPEN` tasks appear there — recommendation: Lists only; no phantom clock blocks.
- Clearing Repeat on a child: stop the whole series vs detach this row into a one-shot.
- `UNTIL` / `COUNT` exhausted: treat as stop — recommendation.
- Midnight while yesterday is `IN_PROGRESS`: today's child already exists; the one-running-task rule still blocks a second Start.
- Product: one-shot tasks with `due_date` and no series — the schema allows it.

## Consequences

- One new table (`task_series`); `tasks` gains `due_date`, `series_id`, `occurrence_date` and a unique `(series_id, occurrence_date)` index; no change to `calendar_events.recurrence` or watch channels.
- The task lifecycle is unchanged per row: `OPEN` → start/stop/pause → `COMPLETED` / `DISCARDED` stays terminal for that child; `DISCARDED` doubles as the write-off, so no new status exists.
- The materializer makes recurrence self-healing: cron downtime surfaces as `OPEN` children through today rather than silently dropping slots.
- Google stays actuals-only; the timer path is untouched.
- Later slices add: the D1 migration, repository methods, the RRULE compiler + materializer, cron wiring, and the `TaskModal` / Lists UI.
