# ADR 0002: Kanban board

Status: Accepted
Date: 2026-08-19

## Context

`/lists` is a list-colored pile of tasks. We are replacing it in the nav with a status kanban at `/board`. This ADR is slice 0 of 5: it locks the design for the board. Nothing is implemented yet; later slices implement against this document the same way ADR 0001 was the source of truth for watch channels.

Today's task model, as locked in `packages/api-core/src/tasks.rs`:

- Statuses are `OPEN`, `IN_PROGRESS`, `COMPLETED`, `DISCARDED`.
- Status is **not** on `PATCH /api/tasks/:id`. Transitions go only through the timer actions: start / stop / pause / complete / discard.
- Start opens a Google Calendar event (`now … now + duration_minutes`). Stop/pause PATCH the event end to now (`start + 60s` when `now <= start`); today both land back in `OPEN`. Complete/discard auto-stop, then set the terminal status.
- Start on `COMPLETED`/`DISCARDED` is a 400. A second start while another task runs is a 409. "Running" is derived from the calendar cache (`task_id` set AND `start_time <= now < end_time`), **not** from `tasks.status`.
- Tasks have no `sort_order`. `GET /api/tasks` is `ORDER BY updated_at DESC, created_at DESC`. Lists/categories already use `sort_order`.

`apps/web/app/modules/lists/` is **not deleted**. Only unlinked from the nav.

## Decision

Replace Lists in the nav with a five-column status board at `/board`. The following spec is the source of truth for all later slices.

### Out of scope

- Deleting Lists code or the `/lists` route.
- List CRUD on the Board (stays on the unlinked `/lists`).
- Untracked tasks (stay hidden, same as Lists).
- Changing Home / FocusTimer (still mock).
- A "show more" for Done/Discarded overflow.
- The 0.x `@dnd-kit/react` rewrite.
- `@atlaskit/pragmatic-drag-and-drop` / `@hello-pangea/dnd`.
- Server-side filter query params on `GET /api/tasks`.
- Rolling back a closed Google event when a later start fails.

### Status model

- New status `PLANNED`. Constant `TASK_STATUS_PLANNED = "PLANNED"` (alongside the four existing constants).
- Column map:

| Column       | Status        |
| ------------ | ------------- |
| Backlog      | `OPEN`        |
| Planned      | `PLANNED`     |
| In Progress  | `IN_PROGRESS` |
| Done         | `COMPLETED`   |
| Discarded    | `DISCARDED`   |

- Create still stamps `OPEN`. New tasks prepend to Backlog (`sort_order = 0`, shift peers).
- **Nothing is terminal.** Any task may move to any column.
- Lift: start on `COMPLETED`/`DISCARDED` becomes allowed (a new calendar event is created; history stays).
- Lift: stop/pause on `COMPLETED`/`DISCARDED` stay invalid as *verbs* if the task is not running — reopen is the path back. (If the task is `IN_PROGRESS`, stop/pause/complete/discard work as today.)
- `pause` **changes landing status** from `OPEN` to `PLANNED`. This is an API contract change: the Lists page still calls pause, and those tasks become `PLANNED`. `stop` still lands `OPEN`.
- New log types: `planned`, `unplanned`, `reopened` (plus the existing `started|stopped|paused|completed|discarded`).

### Transition matrix (board drop → action)

| From \ To      | OPEN                 | PLANNED                  | IN_PROGRESS | COMPLETED        | DISCARDED        |
| -------------- | -------------------- | ------------------------ | ----------- | ---------------- | ---------------- |
| `OPEN`         | reorder              | plan                     | start       | complete         | discard          |
| `PLANNED`      | unplan               | reorder                  | start       | complete         | discard          |
| `IN_PROGRESS`  | stop                 | pause                    | no-op       | complete         | discard          |
| `COMPLETED`    | reopen → `OPEN`      | reopen → `PLANNED`       | start       | no-op/reorder    | discard          |
| `DISCARDED`    | reopen → `OPEN`      | reopen → `PLANNED`       | start       | complete         | no-op/reorder    |

Google side effects:

- **start**: create event `now … now + duration` (existing `start_task`). 409 if any running event exists unless `displace` is provided (see Move API).
- **Every exit from `IN_PROGRESS`** (stop / pause / complete / discard): PATCH the open event end to now (existing auto-stop). No new event.
- **plan / unplan / reopen / reorder / complete-or-discard when not running**: no Google writes.
- Reopen is a new chapter, not an undo. Old completed/discarded logs and closed events stay.
- Same-column drop is reorder (except In Progress with a singleton is a no-op).

### `sort_order`

- Add `tasks.sort_order INTEGER NOT NULL DEFAULT 0`. Named `sort_order`, **not** `board_order_idx`.

```sql
ALTER TABLE tasks ADD COLUMN sort_order INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_tasks_user_status_sort
	ON tasks(user_id, status, sort_order);
```

- Order is **per status** (and per user), not a single global rank.
- Honored on all five columns, including In Progress — even though at most one task is running there.
- The next D1 migration is `apps/worker/migrations/0005_task_sort_order.sql`. The migration file itself is written in slice 1, not here.
- Backfill on migrate (per user):
  - `OPEN`: `created_at ASC` → 0..n.
  - `IN_PROGRESS`: whatever exists (at most one — rank is irrelevant, assign 0).
  - `COMPLETED` / `DISCARDED`: `updated_at DESC` → 0..n — today's "most recent" become the front of the rank.
  - `PLANNED`: none exist yet.
- Create: prepend Backlog (`sort_order = 0`, increment the other living `OPEN` rows for that user).
- Index `(user_id, status, sort_order)` is part of the contract (see DDL above).

### Done / Discarded cap

- Rank **is** the cap. The board renders the first 20 of the **filtered** list (`sort_order` ascending among matches).
- Membership is not recency. A matching card at unfiltered #25 can appear when a filter is on.
- Incoming moves into Done/Discarded are **clamped into the visible window**: the resulting view index is always `0..min(len, 19)`. Never land at #20+.
- Insert: drop index if provided, else **prepend (0)**. Complete / discard / displacement (no drop) prepend.
- Inserting into a full window pushes the old last-visible card to #20 (off the board). Eviction is silent; the row stays in the DB.
- Moving a visible card *out* of Done/Discarded lets #20 slide back into view (first-20-by-rank of the current filter).
- No "show more".

### Filters

- Category (multi-select by category id), Priority (single or all), Difficulty (single or all). Combined with **AND**.
- **Filter first, then cap**: Done/Discarded take the first 20 matches.
- Positions (drop, prepend, 20-cap, reorder) are **view-relative**. `sort_order` is spliced so the card appears at that view index. Non-matches keep their relative order; do not compact a whole column because a filter is on.
- No filter = the unfiltered 0–19 rule.
- Persist in the URL: `/board?priority=high&difficulty=hard&category=id,id`. Unknown category ids are ignored. Default (no params) = all.
- `GET /api/tasks` stays "all living tasks". The client filters and caps. No new query params on the tasks list API.

### Move API

- New endpoint: `POST /api/tasks/:id/move`.
- Body:

```json
{
  "status": "OPEN" | "PLANNED" | "IN_PROGRESS" | "COMPLETED" | "DISCARDED",
  "sort_order": 123,
  "displace": { "id": "<task-id>", "status": "PLANNED", "sort_order": 4 }
}
```

`displace` is optional (omitted or explicit `null`).

- Same status = reorder only (shift peers).
- Different status = dispatch the matrix action, then place at `sort_order` in the target status (shift living peers in that status with `sort_order >=` the insert).
- The client sends an **absolute** `sort_order` (the integer to assign). The server does not receive the filter set.
- Existing timer verbs stay (`/start` `/stop` `/pause` `/complete` `/discard`). The `pause` landing change (→ `PLANNED`) applies to the verb too.
- Auth gate: session + token refresh, same as other timer actions, when the move touches Google (start, or leaving `IN_PROGRESS`). Status-only moves (plan/unplan/reopen/reorder/idle complete/discard) use the session-only gate like CRUD. In short: **if the dispatched action would call Google, require a refreshable token (401 otherwise); otherwise a session cookie is enough.**
- One running task remains. `move` to `IN_PROGRESS` without `displace` while something is running → 409 `"a task is already running"`.
- `displace` (optional): park that task first (matrix to the given status + `sort_order` — must be `PLANNED`, `COMPLETED`, or `DISCARDED`; not `OPEN`, not `IN_PROGRESS`), then start the moved task. Order: free the slot, then start.
- Partial failure is honest: if start fails after displace, displace **stays** (do not reopen the closed event). Locked response shapes:
  - Success: `{ "task": TaskView, "displaced": TaskView | null, "event": CalendarEvent | null }` — the same envelope family as `TaskActionResponse`, plus `displaced`.
  - Start failure after a successful displace: **no rollback.** Return the error status (400/409/502) with `{ "error": "...", "displaced": TaskView }` so the client can keep A parked and snap B back.
- `displace.id` must be the currently running task (the one with an open timed event). Wrong id / not running → 400.
- Canceling the conflict dialog = no request.

### UI

- Nav: replace Lists (`/lists`) with **Board** (`/board`). The pill is also active on `/categories` (same as Lists today in `apps/web/app/routes/__root.tsx`).
- `/lists` remains routed and implemented; not linked.
- Board header: **New Task** + **Edit Categories** (no New List).
- New Task = the existing `TaskModal`; creates `OPEN`; prepends Backlog.
- Click card = `TaskModal`. **No** Play/Pause/Stop/Complete/Discard buttons on the card. The drop is the action.
- Untracked tasks hidden.
- Always **optimistic**. On failure: snap the dragged card back + error banner. A displaced task A stays parked if the start failed.
- Occupied In Progress: `onDragEnd` does **not** apply the optimistic move. Stash `{ taskId, from }`, open a small dialog: move the old task to Planned / Done / Discarded. Confirm → one `move` with `displace`. Cancel → nothing.
- Five columns, horizontal scroll on narrow screens (column min-width ~260px). Pad the bottom for the floating nav. No mobile-only column picker.
- Neutral columns (Categories style: cream page, `rounded-xl`, `border-border`). Thin status accent on the header only. Color lives on the card: title, duration, priority dot, difficulty badge, category swatch (the Lists chip on a light surface).
- Column count = cards **currently shown** (after filter + cap). No `20 / 64` overflow hint.
- Load: `GET /api/lists` first (seeds the taxonomy), then `GET /api/tasks` + `GET /api/categories` (filter options). Same sequential seed rule as `ListsPage`.

### DnD

- `@dnd-kit/core` + `@dnd-kit/sortable` + `@dnd-kit/utilities` (stable 6.x).
- PointerSensor with ~8px activation distance, so a click opens the modal and a horizontal swipe still scrolls the board.
- Not the 0.x `@dnd-kit/react` rewrite.

### Implementation slices

Planned follow-through, not work in this commit:

1. Schema: migration 0005 (`sort_order` + backfill + index), `TASK_STATUS_PLANNED` constant, `pause` lands `PLANNED`, list/create ordering, tests.
2. `POST /api/tasks/:id/move` implementing the full matrix, the start-on-terminal lift, displace, tests.
3. `/board` route, nav unlink, Board chrome, filters (URL), `TaskModal` wiring, columns without drag.
4. dnd-kit, optimistic move, In Progress conflict dialog.

## Consequences

- New status `PLANNED` and three new log types; `pause` now lands `PLANNED` — an API contract change that visibly affects the still-linked Lists page.
- Schema change: `tasks.sort_order` with per-user, per-status order, backfilled by migration 0005. No other table changes.
- `POST /api/tasks/:id/move` becomes the only status/order mutation besides the existing timer verbs; Done/Discarded stay capped at 20 by the client.
- Board UI: five columns, mandatory filters in the URL, optimistic drops, and a displace dialog for an occupied In Progress column. Lists code stays but is unlinked.
- The board adds a dependency on `@dnd-kit/*` (stable 6.x) in its slice.
- Google writes are unchanged in kind (create on start, end-patch on exit), but reopen and start-on-terminal now create new events for previously terminal tasks.
- Later slices add: migration 0005, the move handler, the `/board` route, and the dnd-kit layer.
