# ADR 0004: Routines and agenda

Status: Accepted
Date: 2026-08-23

> Amendment (2026-08-23): recurrence is **one `routines.rrule` TEXT blob** —
> exactly two `\n`-separated lines, `DTSTART:YYYYMMDDTHHMMSS` (floating
> local, no `Z`, no `TZID`) + `RRULE:<body>` (the body may keep a Z-form
> `UNTIL`). `dtstart` and `exdates` columns are gone; the **EXDATE concept is
> removed entirely** (rdates stay deferred). `occurrence.local_date` is the
> **series-instance key — the rule date**; `agenda_item.local_date` is the
> **Home day**. Reschedule moves the **agenda item only**: no
> `occurrence.local_date` rewrite, no exdate — the seed's "no membership
> anywhere" rule (`AgendaItemRepo::get_by_ref`) keeps the rule date from
> re-attaching a moved occurrence, and daily snooze today→tomorrow may put
> two rows of the same routine on tomorrow (allowed). Start is keyed off
> **agenda membership on civil today**, not `occurrence.local_date`. §
> Recurrence / Schema / Agenda rules / Start / the API table are amended.

> Amendment (2026-08-23): agenda **reschedule** is locked:
> `POST /api/agenda/items/:id/reschedule { date }` relocates a slot to another
> day — a task moves by relocating its **membership slot** only (task status
> unchanged, **no `task_logs` row** — the agenda is an overlay and
> `task_logs` has no civil-date column). Session-only: never a Google write,
> never an RRULE. The same task **may** appear on multiple days at once
> (`UNIQUE (user, date, kind, ref_id)` is a per-date key, not a per-task one);
> reschedule relocates a slot, while Add-task still clones onto another day.
> § Agenda rules and the API table are amended.

> Amendment (2026-08-23): there is **one civil "today" per user** — the civil
> date of `now` in the **primary Google calendar's IANA `time_zone`**,
> resolved through **chrono-tz** (the full tzdb; the three-row
> `resolve_tz_offset` table in `packages/api-core/src/time.rs` is deleted,
> and **no hardcoded Kolkata anywhere**). **Travel = home base**: a user
> whose primary calendar says `Asia/Kolkata` stays on Kolkata time abroad —
> the hotel's timezone never moves today. `GET /api/agenda` returns
> `today` + `time_zone` alongside `items`; the `date` query is **optional**
> (missing/blank = today). Occurrence start gates on the **same**
> `user_today` (agenda membership on that date) — not a hardcoded zone.
> The browser never computes today: Home's first load **omits `date`**, and
> the server's `today` anchors the header label, the "Today" button, and the
> Play gate (the date picker may still open any date, including the
> browser-local one). `ceil_5min_unix_in_zone` (the elongate cron) is
> chrono-tz too: local wall-clock ceil, DST-aware; a fold resolves
> `.single()` else `.earliest()`. Unknown/empty IANA → UTC.
> § Expansion semantics / Agenda rules (Start) / Surfaces (Home) / API /
> Residual risk are amended.

## Context

Sanctuary **Tasks** (ADR 0002) are a finite work queue: five columns, a timer, and the status machine `OPEN | PLANNED | IN_PROGRESS | COMPLETED | DISCARDED`. Completing a task means the item is finished — the queue is supposed to drain.

Most of the owner's day is not finite work. Daily repeating commitments — Salat, chores, checking email, personal finance — already live on Google Calendar as recurring events. The owner moves them around and does them when they feel like it. They must **not** become Board cards: a card that resurrects itself every morning breaks the queue's premise.

The ideal this ADR locks: things you intend to do are managed from Sanctuary. Google Calendar events are **logs of time spent** — never the recurrence engine, never the plan. Sanctuary owns repetition; Google records it.

This ADR is slice 1 of 6: it locks the design for routines and the agenda. Nothing is implemented yet; later slices implement against this document the same way ADR 0001 was the source of truth for watch channels and ADR 0002 for the board.

## Decision

Four nouns, three tables, one expansion rule. The following spec is the source of truth for all later slices.

### Out of scope

- Board (`/board`) is frozen — tasks only, unaware of routines.
- Calendar page is frozen — a started occurrence's log appears as an ordinary cached event; no Calendar-page work.
- Consistency is out of this train. It will later read occurrence history.
- No new nav tab for Routines. `/routines` is linked from Home ("Routines") and optionally Settings.
- Existing Google recurring events are **not** imported. Once a routine lives in Sanctuary, the owner deletes the old Google series by hand so the calendar stops double-drawing a plan. Manual, out of v1.
- No create-task from Home in v1. New finite work is `TaskModal` on the Board, then add to a date.
- `rdates` (extra inclusion dates) are deferred to v2; there are no `exdates` — exclusions are expressed by moving the day's agenda item (a reschedule), never by editing the rule.
- The elongate cron growing occurrence events is documented here, implemented in slice 6.

### Refused alternatives

Each of these was considered and refused. They stay refused for this entire train:

- `tasks.recurrence` or `tasks.kind = 'routine'`. Routines get their own tables.
- Writing `recurrence: [RRULE…]` (or any RRULE) on any Google Calendar insert/patch. Google events stay one-shot logs.
- Routine cards in Backlog / Planned / In Progress / Done / Discarded.
- Redefining `PLANNED` as "on today's agenda." Agenda membership is its own table, not a task status.
- Treating "has a calendar event today" as "the occurrence exists." Existence is a `routine_occurrences` row; the event is only a log of time spent.
- Auto-adopting existing Google recurring series. Manual cleanup in Google is out of v1.
- Renaming `tasks.duration_minutes`. Routines use `estimated_minutes`; tasks keep their column.
- Client-only RRULE expansion as the source of truth for seed. The server expands.
- Create-task from Home in v1.
- Start on a non-today occurrence.
- Auto-rollover of unfinished yesterday-agenda tasks onto today.
- Task focus (`users.focused_task_id`) for routines in v1.
- Changing Board, Calendar page, or Consistency in this train. (A start's one-shot event appears on the Calendar automatically because it is an ordinary cached event — no Calendar-page work.)
- New nav tab for Routines.

### Nouns

| Noun             | Job                                                                                                                              |
| ---------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| **Routine**      | Standing definition. Never completable. Never on the Board.                                                                       |
| **Occurrence**   | One local-date instance of a routine (`pending \| in_progress \| done \| skipped`). Completing Fajr today does not complete the series. |
| **Agenda item**  | Date-scoped row in today's (or any day's) run-of-show: `{ kind: task \| occurrence, ref_id, sort_order }`.                        |
| **Task**         | Unchanged (ADR 0002). Finite. Board warehouse.                                                                                    |

### Recurrence: Sanctuary-owned, full RFC 5545

Repetition is expressed as a full RFC 5545 RRULE stored on the routine — not a bespoke cadence enum. Anything the standard can say, a routine can say.

#### Stored on `routines`

One TEXT column, two lines — the whole recurrence is one blob:

```text
DTSTART:20260105T063000
RRULE:FREQ=WEEKLY;BYDAY=MO
```

- `rrule` — the blob: a `DTSTART:` line (basic form `YYYYMMDDTHHMMSS`, **floating local** — no `Z`, no `TZID`, no offset) then a `RRULE:` line holding the RFC 5545 RRULE **body** (`FREQ=DAILY;INTERVAL=1`, `FREQ=WEEKLY;BYDAY=MO,WE;UNTIL=…`, …). Exactly two `\n`-separated lines, no trailing extras. Never sent to Google.
- The blob is **rejected** (400 `invalid rrule`) when it contains `EXDATE`, `RDATE`, `EXRULE`, or `TZID` anywhere, more than one `RRULE:` line, or a `Z` on the DTSTART value. `UNTIL` inside the RRULE body **may** stay Z-form (`UNTIL=20260104T063000Z`) — the Rust crate validates it against the UTC-carried DTSTART; the DTSTART line itself must never be Z.
- There is **no `exdates`** — exclusions are expressed by rescheduling the day's agenda item, never by editing the rule. `rdates` stay deferred to v2.

#### Expansion semantics

- Expansion runs in **floating local civil time**. Do not attach `TZID` to the rule; interpret results as civil dates in the same calendar. Production TZ `Asia/Kolkata` has no DST, so this is honest.
- **"Today"** on Home (the default date) is the civil date implied by `now` plus the existing locked offset table in `packages/api-core/src/time.rs` (`resolve_tz_offset`: the UTC family → 0, `Asia/Kolkata`/`Asia/Calcutta` → `+19800`, `Asia/Dubai` → `+14400`, anything else → UTC). Do not add a tzdb for this feature.

#### Two libraries, one stored string

| Side                                   | Library                                        | Used for                                            |
| -------------------------------------- | ---------------------------------------------- | --------------------------------------------------- |
| Server seed / membership               | Rust crate `rrule` (`fmeringdal/rust-rrule`)   | `GET /api/agenda?date=`: expand, `between(start_of_D, end_of_D)` → ensure occurrence |
| Editor / next-N preview                | npm `rrule` (`jakubroztocil/rrule`)            | `/routines` builder + preview, Home cadence summary |

Both sides consume the **same stored blob** (the whole `rrule` string). Golden fixtures (blob → expected dates) must run on **both** sides so the engines cannot drift — this is an acceptance requirement of the RRULE slices. Fixtures may land in slice 2 and be asserted from slice 3.

The Rust crate pulls `chrono` (+ `chrono-tz`). Documented as a Worker-size residual risk below: measure wasm size when it lands; if needed, compile `chrono-tz` with a zone filter. Either way, expansion stays floating local — the tzdb is never used for membership.

#### Editing a rule

- Changing `rrule` does **not** delete already-materialized occurrences. It only affects future ensure. An occurrence that no longer matches the rule stays — it was planned.
- The blob is parsed/validated on routine create/update. Invalid rule → 400.

### `estimated_minutes`

- `routines.estimated_minutes INTEGER NOT NULL DEFAULT 15` (min 1, enforced at the API). The name is deliberate: a planned estimate on the card.
- Start still opens the fixed `T … T + START_EVENT_MINUTES` (15) live marker, independent of `estimated_minutes` — the same rule as tasks (ADR 0002 amendment). The estimate never sizes the Google chip.
- `tasks.duration_minutes` is not renamed.

### Occurrence title (unlike Tasks)

- `routine_occurrences.title` is **nullable**. `NULL` = inherit `routines.title`. Seeding copies nothing; inheritance stays live until overridden.
- Resolved title = `occurrence.title ?? routine.title`.
- `PATCH /api/occurrences/:id { title }` writes the override.
- **If `google_event_id` is set, that PATCH also PATCHes the Google event `summary`.** Required. Tasks do not do this; the difference is locked here.
- Changing `routine.title` does **not** rewrite overrides and does **not** PATCH existing Google events. The next start of an un-overridden occurrence uses the new routine title.
- Start snapshots the **resolved** title onto the new event.
- Home / Agenda classify the card by the **resolved** title — the same `classify` matcher, `CalendarScope::Ignore` — so a rename can re-file the card's computed category (and color). Create/update of the **routine** title uses the same unique non-untracked classify rules as `create_task` (400 on 0 matches / conflict / untracked sink). An occurrence title override is allowed to become untracked — it renames a day, not a new definition — so listing still returns the untracked summary; a read never 400s on classification.

### Schema

Three new tables. Soft-delete lives **only on `routines`** (it is the domain entity). `routine_occurrences` are not soft-deleted — skip is the decline. `agenda_items` are membership rows: **hard-delete** on unpin, the same reasoning as watch channels in ADR 0001 (a subscription, not a domain entity).

Routine titles are **not** unique — no `UNIQUE (user_id, title)`.

```sql
CREATE TABLE IF NOT EXISTS routines (
	id TEXT PRIMARY KEY,
	user_id TEXT NOT NULL,
	title TEXT NOT NULL,
	estimated_minutes INTEGER NOT NULL DEFAULT 15,
	rrule TEXT NOT NULL,
	sort_order INTEGER NOT NULL DEFAULT 0,
	created_at TEXT NOT NULL DEFAULT (datetime('now')),
	updated_at TEXT NOT NULL DEFAULT (datetime('now')),
	deleted_at TEXT,
	FOREIGN KEY (user_id) REFERENCES users(id)
);

CREATE INDEX IF NOT EXISTS idx_routines_user_living
	ON routines(user_id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_routines_user_sort
	ON routines(user_id, sort_order);

CREATE TABLE IF NOT EXISTS routine_occurrences (
	id TEXT PRIMARY KEY,
	routine_id TEXT NOT NULL,
	user_id TEXT NOT NULL,
	local_date TEXT NOT NULL,
	title TEXT,
	status TEXT NOT NULL DEFAULT 'pending',
	calendar_id TEXT,
	google_event_id TEXT,
	created_at TEXT NOT NULL DEFAULT (datetime('now')),
	updated_at TEXT NOT NULL DEFAULT (datetime('now')),
	UNIQUE (routine_id, local_date),
	FOREIGN KEY (routine_id) REFERENCES routines(id),
	FOREIGN KEY (user_id) REFERENCES users(id)
);

CREATE INDEX IF NOT EXISTS idx_occurrences_user_date
	ON routine_occurrences(user_id, local_date);
CREATE INDEX IF NOT EXISTS idx_occurrences_routine
	ON routine_occurrences(routine_id);

CREATE TABLE IF NOT EXISTS agenda_items (
	id TEXT PRIMARY KEY,
	user_id TEXT NOT NULL,
	local_date TEXT NOT NULL,
	kind TEXT NOT NULL,
	ref_id TEXT NOT NULL,
	sort_order INTEGER NOT NULL DEFAULT 0,
	created_at TEXT NOT NULL DEFAULT (datetime('now')),
	updated_at TEXT NOT NULL DEFAULT (datetime('now')),
	UNIQUE (user_id, local_date, kind, ref_id),
	FOREIGN KEY (user_id) REFERENCES users(id)
);

CREATE INDEX IF NOT EXISTS idx_agenda_items_user_date_sort
	ON agenda_items(user_id, local_date, sort_order);
```

Column notes:

- `routines.rrule` holds the whole recurrence blob — `DTSTART:YYYYMMDDTHHMMSS` + `RRULE:<body>`, `\n`-separated; `local_date` everywhere is `YYYY-MM-DD`. Neither ever carries an offset.
- `routine_occurrences.local_date` is the **series-instance key**: the rule date the instance was materialized for. It is never rewritten by a reschedule. `agenda_items.local_date` is the **Home day** — where the row is actually planned.
- `routine_occurrences.status` is one of `pending | in_progress | done | skipped` (lowercase — these are occurrence states, not task statuses).
- `calendar_id` / `google_event_id` are null until start. There is **no `google_event_id` on `routines`** and no series master anywhere.
- Calendar destination resolves at **start**, with the same inheritance as `start_task`: matching pattern → category → parent root → primary; a named calendar that is missing or read-only falls back to primary; no writable calendar at all → 400.
- One next D1 migration (latest today is `0006_user_focused_task_id.sql`): `apps/worker/migrations/0007_routines.sql` contains **all three tables**, schema-ahead like `0003_lists_categories_tasks.sql`. Slice 2 writes that file but still has no occurrence/agenda handlers; later slices add APIs only.

### Agenda rules

The agenda is date-scoped. `GET /api/agenda?date=YYYY-MM-DD` is the read. Missing/invalid `date` → 400.

**Seeding happens on GET, not a midnight cron:**

1. For every living routine of the user whose rrule blob includes that local date, `INSERT` the occurrence if missing (`UNIQUE (routine_id, local_date)` makes this idempotent).
2. Append an `agenda_items` row **only if this occurrence id has no agenda item on ANY date** (`AgendaItemRepo::get_by_ref(user, kind=occurrence, ref_id=occurrence.id)` — no date). This is the "Monday must not come back" guarantee: a reschedule moves the item to another day, so a GET on the rule date never re-attaches it. Auto-seeded occurrences land in `routines.sort_order` relative to each other, **after** any already-present items — never reshuffle a list the user already reordered.
3. Tasks never auto-land.

**Living rows only.** Seeding covers living routines (`deleted_at IS NULL`); a soft-deleted routine never seeds a new occurrence. The response likewise omits occurrence items whose routine is missing or soft-deleted and task items whose task is missing or soft-deleted. Their orphan `agenda_items` membership rows stay in D1 — membership is hard-deleted only by unpin.

**Adding a task** to a date: `POST /api/agenda/items { kind: "task", ref_id, sort_order? }`. Default append (`max + 1` for that date). Allowed task statuses in v1: living `OPEN | PLANNED | IN_PROGRESS`. Already on that date → **idempotent 200 returning the existing item** (locked over a 400). Does **not** change `tasks.status` — Home is an overlay, not a sixth column.

**Removing a task** from a date: `DELETE` the agenda item. The task stays on the Board.

**Unpinning an auto-seeded occurrence is refused.** DELETE on an occurrence-kind agenda item is 400. **Skip is the decline** (`POST /api/occurrences/:id/skip`).

**Reorder:** `POST /api/agenda/items/:id/move { sort_order }` — that date's pile only, peers shifted within the date. Tomorrow ignores today's permutation except each routine keeps its standing `routines.sort_order` for the next seed.

**Reschedule:** `POST /api/agenda/items/:id/reschedule { date }` relocates a slot to another day — the weekly occurrence that cannot happen today moves to tomorrow without becoming a Board card and without writing a Google RRULE. Session-only: no Google write of any kind.

- **Occurrence:** the **agenda item moves only**. `occurrence.local_date` is never rewritten (it is the series-instance key — the rule date) and the routine's `rrule` is never touched; **there is no EXDATE anywhere**. Why this is safe: the seed's step-2 rule ("no membership on any date") means the next `GET /api/agenda?date=<rule-date>` does **not** re-attach the moved occurrence — the moved-away day stays empty, while next week's same weekday is a different `YYYY-MM-DD` and seeds fresh. `pending | skipped` are the only reschedulable statuses — `skipped` becomes `pending` on the new date (deferred, not declined); `in_progress` / `done` → 400. A target date that already has an occurrence of this `routine_id` is **allowed** — daily snooze today→tomorrow puts TWO rows of the same routine on tomorrow (today's moved instance + tomorrow's own seeded instance). The agenda row moves to the target date **appended** (`max+1` on the target pile). Title override and any stored google ids travel with the row (pending/skipped should have no chip, but ids are never cleared if somehow set). Same date → 200 no-op returning the item unchanged.
- **Task:** the **membership slot** moves, not the task — `tasks.status` is unchanged and **no `task_logs` row is written**: the agenda is an overlay, and `task_logs` has no civil-date column, so a reschedule has nothing to log (locked refusal). Same date → 200 no-op. A target date that already has this task **unpins the source** (hard-delete this item) and returns the **existing** target item — never a duplicate. The same task **may** appear on multiple days at once (`UNIQUE (user, date, kind, ref_id)` keys one row per date); reschedule relocates a slot, while Add-task still clones onto another day.
- Missing item / other-user / soft-deleted routine → 404. Missing/invalid `date` → 400. Response is `{ "item": AgendaItemView }` (same embed as add/move).

**Crossing off:**

- Task → the existing `/complete` (Board → Done). The agenda row stays as a crossed-off row for the rest of that date.
- Occurrence → `done` for that date. If an event is currently open for the occurrence, its end is PATCHed closed like a task exit; with no running event there is no Google write.

**Skip:** occurrence → `skipped`. First-class verb. No task equivalent. No Google write — except skip while running PATCHes the open event's end closed (see verb matrix below).

**Start:**

- A task on Home uses the existing `/start` (Board → In Progress, one-shot Google log as today).
- An occurrence start creates the one-shot Google log, moves the occurrence to `in_progress`, and stores `calendar_id` + `google_event_id` on the occurrence. Start is valid **only when this occurrence has an agenda item whose `local_date` is civil today** (civil today via the offset table); otherwise 400 (`"occurrence can only be started on a day it is scheduled"`). The gate is agenda membership, NOT `occurrence.local_date` — a rescheduled occurrence starts where its item sits, not on its rule date.
- One Google event per occurrence, ever. Repeating start while `in_progress` is a 200 no-op (no new event).
- The elongate cron must grow living `in_progress` occurrence events the same way it grows `IN_PROGRESS` task events (slice 6). Documented here; not implemented before then.

**Occurrence verb matrix (locked across all slices):**

| status ↓ / verb → | `start`                         | `complete`                             | `skip`                                   |
| ----------------- | ------------------------------- | -------------------------------------- | ---------------------------------------- |
| `pending`         | starts — today-only (else 400)  | → `done`; no Google                    | → `skipped`; no Google                   |
| `in_progress`     | 200 no-op                       | → `done`; PATCH open event end closed  | → `skipped`; PATCH open event end closed |
| `done`            | 400                             | 200 no-op                              | → `skipped`; no Google                   |
| `skipped`         | 400                             | → `done`; no Google                    | 200 no-op                                |

- `done` / `skipped` are terminal for `start`.
- `complete` ↔ `skip` flips are allowed; there is no dedicated unskip verb.
- Skip while running must close the chip (PATCH the open event's end), same as complete while running.
- A missing, other-user, or soft-deleted-routine occurrence is **404** on every verb.

**Focus** stays task-only. A running routine is highlighted on Home, never takes `users.focused_task_id`, and never appears in the Board's In Progress column.

### Surfaces

| Surface              | This train                                                                                                                                                          |
| -------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Home (`/`)**       | The working surface. Date selector (prev / today / next + calendar pick). Mixed Agenda list for the selected date. Add-task picker. Reorder. Check-off. Skip. Start. Tap occurrence to rename. Tap task → existing `TaskModal`. Replace the mock `todayItems` list; **remove** the mock timeline (`SkewedTimeline`) from Home so the page is the Agenda, not two competing UIs. |
| **`/routines`**      | CRUD for standing routines. RRULE builder via npm `rrule`, next-N preview, `estimated_minutes`, standing order. **Not** a nav tab — linked from Home ("Routines") and optionally Settings. Creating a routine does not put it on a date; the next matching Agenda GET seeds it. |
| **Board**            | Frozen. Tasks only. Unaware of routines.                                                                                                                             |
| **Calendar page**    | Frozen. Logs appear as ordinary events after start.                                                                                                                  |
| **Consistency**      | Out of this train. Will later read occurrence history.                                                                                                               |

Existing Google recurring events are not imported; the owner deletes the old series by hand once the routine lives in Sanctuary (see § Out of scope).

### API

Shapes are locked here; the Rust module layout is not. Endpoints are session-gated like lists/categories. Google token refresh happens only when a handler would write Google: occurrence `/start`, and occurrence title `PATCH` when a chip exists.

| Endpoint                                    | Behavior                                                                                                                                         |
| ------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| `GET /api/routines`                         | Living routines for the user.                                                                                                                    |
| `POST /api/routines`                        | Create; body `{title, estimated_minutes?, rrule}` where `rrule` is the two-line recurrence blob (`DTSTART:` + `RRULE:`); validates the blob (400 on invalid, incl. EXDATE/RDATE/EXRULE/TZID/Z-on-DTSTART) and classifies the title like `create_task`. |
| `PATCH /api/routines/:id`                   | Update; body `{title?, estimated_minutes?, rrule?, sort_order?}` — same validation on a present blob (it carries its own DTSTART). Rule changes never touch materialized occurrences. |
| `DELETE /api/routines/:id`                  | Soft-delete (`deleted_at`). Materialized occurrences are not deleted.                                                                            |
| `GET /api/agenda?date=YYYY-MM-DD`           | Ensure-for-date + seed, then return mixed items with an embedded task view / occurrence+routine view: resolved title, computed category summary, status, `estimated_minutes` on the routine, `focused` only on tasks. Seed appends the item only when the occurrence has **no agenda item on any date**. Missing/invalid date → 400. Seeds only living routines; omits occurrence items whose routine is missing/soft-deleted and task items whose task is missing/soft-deleted (orphan membership rows stay in D1). |
| `POST /api/agenda/items`                    | `{ kind, ref_id, sort_order? }`. Tasks only in v1; idempotent 200 with the existing item when already present.                                   |
| `POST /api/agenda/items/:id/move`           | `{ sort_order }`; reorder within that date's pile.                                                                                               |
| `POST /api/agenda/items/:id/reschedule`     | `{ date }`; relocate the slot to that day. Occurrence: **agenda item moves only** — `occurrence.local_date` (the rule date) and `routines.rrule` are never touched, no EXDATE anywhere (`pending|skipped` only; `skipped` → `pending`; `in_progress`/`done` → 400; a target date already holding this routine is allowed — two rows of the same routine on one day are legal), agenda row appended on the target pile. Task: move the membership slot only (no `task_logs`, task status unchanged); already-on-target unpins the source and returns the existing target item. Same date → 200 no-op. Session-only, no Google write. |
| `DELETE /api/agenda/items/:id`              | Hard-delete (unpin). Occurrence-kind items → 400 (skip is the decline).                                                                          |
| `PATCH /api/occurrences/:id`                | `{ title? }` writes the override; PATCHes the Google event `summary` when a chip exists.                                                         |
| `POST /api/occurrences/:id/start`           | One-shot Google log, occurrence → `in_progress`, store ids. Today-only (else 400). Repeat while `in_progress` → 200 no-op.                        |
| `POST /api/occurrences/:id/complete`        | Occurrence → `done`; closes an open event if one is running, otherwise no Google write.                                                           |
| `POST /api/occurrences/:id/skip`            | Occurrence → `skipped`. No Google write.                                                                                                         |

Errors, across all of the above:

- **401** — missing session.
- **400** — invalid input: bad/missing date, malformed body, invalid rrule blob, DELETE on an occurrence item, start on an occurrence not scheduled today, adding a terminal or foreign task, classify failures on routine title.
- **404** — missing, other-user, or soft-deleted routine/occurrence/item. Never leak existence.
- **502** — Google write failure.
- **500** — repository failure.

### Residual risk (documented, not fixed in this ADR)

- The Rust `rrule` crate pulls `chrono` + `chrono-tz`; wasm size must be measured when it is added, and `chrono-tz` compiled with a zone filter if needed. Membership never uses the tzdb regardless.
- Dual-library drift between the Rust and npm RRULE implementations — mitigated by golden fixtures asserted on both sides.
- Floating-local RRULE is wrong for a DST zone. Production is Kolkata (no DST), so this is honest today. A user calendar set to `America/New_York` already falls back to UTC in `resolve_tz_offset` — the same known limitation as the elongate cron, not introduced by this ADR.
- Existing Google recurring series remain until the owner deletes them; until then the plan double-draws on the calendar.

### Implementation slices

Planned follow-through, not work in this commit:

1. **This ADR.**
2. **Routine CRUD API** — writes the single migration `0007_routines.sql` with all three tables (schema-ahead, like `0003_lists_categories_tasks.sql`), classify, RRULE parse/validate (Rust `rrule`), `estimated_minutes`, worker routes, tests. Still no occurrence/agenda handlers; later slices add APIs only.
3. **`/routines` page** — npm `rrule` builder, next-N preview, standing order. Golden fixtures shared with the Rust side as soon as both exist (fixtures can land in slice 2 and be asserted from slice 3).
4. **Occurrences + Agenda API** — ensure-for-date, mixed list, add/remove/reorder task, complete/skip, occurrence title PATCH (the Google part may wait for slice 6 if no chip exists yet). Tests: cadence × civil date × idempotent ensure; Board endpoints still never return routines.
5. **Home Today** — date selector, list, add-task picker, reorder, check-off. Board untouched.
6. **Occurrence start** — one-shot log, store ids on the occurrence, title PATCH updates `summary`, elongate.

## Consequences

- Three new D1 tables behind the single migration `0007_routines.sql` (schema-ahead, like `0003_lists_categories_tasks.sql`). No existing table changes: no `tasks.recurrence`, no `tasks.kind`, no renamed `duration_minutes`, no focus changes.
- Repetition becomes Sanctuary-owned, full-strength RFC 5545 expanded in floating local civil time; "today" comes from the existing fixed offset table, with no new tzdb dependency.
- Two new dependencies land in their own slices: the Rust `rrule` crate (wasm-size watch) and npm `rrule`. Golden fixtures on both sides are the drift guard and gate the RRULE slices.
- The Google write surface grows by exactly two cases — occurrence start creating a one-shot log, and an occurrence title override patching an existing chip's `summary`. Everything sent to Google stays a one-shot event; no RRULE ever leaves Sanctuary.
- Seeding happens on GET, so agenda correctness depends on the read being called; there is no midnight cron ensuring occurrences.
- Home becomes the daily working surface (mixed agenda, date selector, reorder, check-off, skip, start, rename) and loses both mocks (`todayItems`, `SkewedTimeline`); `/routines` exists without a nav entry; Board, Calendar page, and Consistency are untouched.
- Later slices add: the single `0007_routines.sql` migration (slice 2, all three tables), routine CRUD routes, the `/routines` page, the occurrences + agenda handlers, the Home rebuild, and occurrence start + elongate support.
