# ADR 0005: Calendar sync reliability

Status: Accepted
Date: 2026-09-10

## Context

Personal Goals stalled for nine days (2026-09-01..09). `GET /api/calendar/events`
was cache-only once `last_synced_at` was parseable, regardless of age. Sync
errors were `console_log` only. A calendar could serve a successful-looking
cache forever while incremental apply failed. Sibling calendars on the same
OAuth token kept syncing, so the product looked “fine” for some calendars and
silently rotten for others.

The root confusion: **a parseable `last_synced_at` was used as both the
request-path gate and an implied health signal.** Those must diverge.

## Supersedes

**Supersedes ADR 0001’s health signal:** parseable `last_synced_at` is **not**
“healthy / cache-only forever” as a freshness or health signal.

**Keeps from ADR 0001 (still valid):**

- `google_calendars_watch_channels` table and lifecycle
- 15-minute fallback cron
- ~~No Cloudflare Queue~~ — reversed 2026-09-11 (issue #80 / V6):
  queue is a latency layer; dirty generation + 15-minute cron remain
  the contract
- Request-path cache-only after first successful publication
  (`initial_sync_complete` / parseable `last_synced_at` as the _gate_, not as
  health)
- Watch 404 / `events.list` 404 disable sync

## Decision

### Destination (two paths)

Two distinct Google read shapes. They must not be collapsed:

- **Window fetch (Path A):** `timeMin` + `timeMax`, `singleEvents=true`,
  `orderBy=startTime`, first paint / visible range. Any `nextSyncToken` from
  this query is **thrown away**. Does **not** call `record_sync_success`.
  Bumps `dirty_requested_generation` so Path B can catch up.
- **Replica walk (Path B):** `singleEvents=false`, **no** `timeMin` / `timeMax`
  / `updatedMin` (unbounded list). Optional `syncToken` / `pageToken`.
  **Only this walk owns `sync_token`.** Persist the token only after the last
  page is durably committed under a still-held lease.

### Invariants

1. Never advance `syncToken` past work that is not durably committed.
2. Upsert by `(calendar_id, google_event_id)` — never a global unique on
   `google_event_id`. Never use `iCalUID` as PK. Window `singleEvents=true`
   instance ids are distinct from masters — upsert under the instance id.
3. One fenced owner per calendar (lease acquire / renew / fenced success /
   release on both paths that write).
4. Watches are hints; the periodic incremental is the contract.
5. 410 on the **replica** is merge-full, not truncate (drop in-memory cursor,
   re-list once, keep already-applied rows). Window 410 is a plain error (no
   token was sent).
6. Cancelled exceptions are kept rows. Ordinary cancelled events stay
   soft-deletes. Same `classify_replica_item` on window write-through and
   replica apply.
7. App-owned fields (`task_id`, future notes) stay off the Google-overwrite set.
   `task_id` COALESCE is permanent.
8. A recent attempt is not success. A healthy watch is not a healthy replica.
   Completed empty deltas count as success. Invalid/future timestamps are
   stale, not “fresh.”
9. Same D1 database for tokens, events, and health. Never split token store
   from event store.
10. HTTP 401 is session auth only. Google reconnect is
    `authorization_required` in the envelope.
11. Do not auto-reset the token after N failures.

### Projection / app-owned

Product projection: `timed_masters_and_exceptions` (all-day **stored**,
**excluded** from GET list SQL). All-day civil dates are stored as RFC 3339
Z-midnight (`YYYY-MM-DDT00:00:00Z`); lexicographic overlap has residual risk
for non-UTC civil days. `task_id` (and future notes) must not live in columns
a Google merge blindly overwrites — keep COALESCE / app-owned semantics
permanent.

### What shipped vs later

**V1**

- Health columns on `google_calendars` + event identity columns on
  `calendar_events` (nullable / defaulted)
- `CalendarRepo::record_sync_{attempt,success,failure}`
- Sanitized `sync` envelope on `GET /api/calendar/events`
- Existing sync path records attempt vs success; missing terminal
  `nextSyncToken` is **not** publication success
- Request path cache-only after first publication; stale is visible in the
  envelope, not a blocking resync
- Projection declared: `timed_masters_and_exceptions`

**V2 (shipped)**

- Two-path reads: never-initialized calendars first-paint via a bounded
  `singleEvents=true` **window**; initialized calendars stay cache-only
- Window throws away `nextSyncToken`; does not call `record_sync_success`;
  bumps `dirty_requested_generation`
- Replica walk is unbounded (`singleEvents=false`, no `timeMin`) so production
  tokens are not invalidated / 410-stormed by a fingerprint change that adds
  time bounds
- Identity columns / `raw_json` populated on apply; cancelled exceptions kept;
  upsert restores `deleted_at` and returns the persisted id
- 410 merge-full (no truncate); page-by-page apply under a lease; fenced
  `record_sync_success_if_owner`
- First paint is **window**, not replica-await
- GET envelope `source` is `cache` | `window` | `mixed`
- Projection still `timed_masters_and_exceptions`; all-day stored as Z-midnight

**V3 (shipped)** — 2026-09-10

- Watches are **hints**; webhook `exists` persists `dirty_requested_generation`
  then HTTP 200; optional `wait_until` replica is an optimization only
  (removed by #79; V6 replaces that optimization with a queue)
- `not_exists` persists disable then 200; stop is best-effort
- Cron consumes dirty (`requested > applied`) **and** the 15-minute
  `last_success_at` backstop **and** `full_sync_requested` **and** due
  `next_retry_at`; honors backoff and `authorization_required`
- Successful publish sets `dirty_applied_generation` to the
  **generation-at-start**
- Worker `notify_user` after successful cron publish (`CronReport.published`);
  broadcast to 0 sockets is logged
- calendarList incremental with per-user cursor; 410 merge-full; incremental
  `deleted=true` disables+stops; incremental absence does not delete; full
  absence orphans
- Metadata upsert does not re-enable a living user-disabled calendar
- Leftover watch channels on disable/soft-delete are retried by cron

**V4 (shipped)** — 2026-09-10

- Outbound `calendar_event_operations` journal before Google insert/patch/delete
- Client-supplied insert ids; 409 → GET; no phantom local id
- If-Match on patch/delete; 412 GET+retry cap 3 → operation `conflict` (calendar not disabled)
- `403 forbiddenForNonOrganizer` not retried
- Delete 404/410 (including empty-etag GET 404) still local-delete
- Cron GET-only repair of pending/google_committed
- Replica skips in-flight google ids
- GET projection hides unmodified instances when a living master exists
- PATCH set remains minimal (start/end/summary / status cancelled) — attendees/conferenceData not sent

**V5 (shipped)** — 2026-09-10

- Browser reads sanitized `sync` on GET events; one compact chrome banner
- `authorization_required` / degraded / never_initialized visible without
  clearing the grid
- `never_initialized` (including API `stale=true`) uses quiet “Syncing earlier
  events…” not an out-of-date warning
- WS open/reconnect cancel-then-invalidates events + calendars so a missed cron
  `notify_user` converges
- `calendar.changed` uses the same catch-up runner (cron already notifies; no
  second push channel)
- Overlay map resets on logout / account change
- Failed writes surface a small error; no offline queue; no user-facing Full
  Refresh

**V6 (shipped)** — 2026-09-11 (issue #80)

- Cloudflare Queue `calendar-sync` in the **same** Worker (`fetch` +
  `scheduled` + queue consumer). Binding `CALENDAR_SYNC`.
  `max_batch_size = 1`, `max_batch_timeout = 0`, default `max_retries`
  (3), `max_concurrency` unset, **no dead-letter queue**.
- Webhook contract unchanged through the durable write: verify →
  `persist_webhook_decision` → 200. Best-effort enqueue **after** a
  successful dirty write. Failed enqueue is logged and swallowed.
- Orchestration in api-core (`run_queue_sync`). Worker shell:
  deserialize, wire D1, `refresh_if_needed`, map
  `Ack` / `Retry` / `Reenqueue` to `message.ack()` / `message.retry()` /
  `queue.send()`. `Retry` is reserved; current rules never return it.
- Action rules: Published + `requested == applied` → Ack; Published +
  `requested > applied` after the walk (post-publish generation
  predicate, not a pre/post requested comparison) → one Reenqueue then
  ack; LeaseBusy → Ack; sync failure → Ack (preserves `next_retry_at`;
  cron owns retry); missing / disabled / soft-deleted → Ack.
- Follow-up send failure: log and ack. Never throw.
- After a successful queue publish the Worker notifies open browsers
  (`notify_user`), same predicate as cron (`published`).
- Cron unchanged. No change to `sync_calendar` lease semantics, replica
  walk, token refresh, or the sanitized `sync` envelope.
- Queue is **not** a second source of truth. Watches remain hints
  (invariant 4). Dirty generation + fallback cron remain the contract.

**Later**

- Cloudflare Queues: **shipped as V6 latency layer** (issue #80).
  Not the correctness contract.

## Consequences

- `last_synced_at` / `initial_sync_complete` remain the **request-path gate**
  only. Clients and operators read health from the sanitized `sync` envelope
  (`last_success_at`, `sync_status`, `stale`, `error_code`, …).
- A failed incremental leaves the previous cursor and success stamp intact;
  the next run can replay (upsert-by-id). Empty `items` + `nextSyncToken`
  still counts as success **on the replica path**.
- Missing terminal `nextSyncToken` on the replica is classified as
  `missing_sync_token` and stored as `retrying` — never as a silent “keep old
  token and stamp success.”
- Tokens, OAuth credentials, lease secrets, event bodies, and `raw_json` never
  appear in the envelope or in `last_error_code`.
- Cron keys off dirty generation + `last_success_at` (15-minute backstop) +
  `full_sync_requested` + due `next_retry_at`. Window bumps dirty as a hint
  without setting `full_sync_requested`. Watches never disable the poll.
- Queue `calendar-sync` cuts webhook staleness from the 15-minute cron
  cadence to seconds; dropped messages still converge on the next cron tick.

## Residual risk

- Merge-full ghosts remain until a later incremental cancel (or operator
  action); there is no mark-and-sweep.
- Lease renew uses the caller’s fixed `now`, so a walk longer than the 90s TTL
  can be stolen mid-flight.
- Window instances (`singleEvents=true` ids) and replica masters share
  `calendar_events` but different `google_event_id`s — both are valid rows
  under the natural key. Unmodified expansions are hidden from GET when a
  living master exists; modified exceptions are kept.
- GET-only repair never re-POSTs; a pending insert that 404s is marked failed
  (user retries, new client id).
- All-day Z-midnight storage: non-UTC civil dates have residual lexicographic
  overlap risk on GET.
- Channel rows whose parent calendar row is hard-missing (no `user_id`) cannot
  be stopped automatically; they surface once per tick as a cron error string.

## Module map (2026-09-10)

Calendar code is split by boundary so a newcomer does not scroll a single novel:

- **Service** (`packages/api-core/src/calendar/`): `list` / `write` /
  `write_journal` / `journal` / `repair` / `watch` / `webhook` / `catalog` /
  `cron` / `queue` / `labels` / `apply` / `replica` / `window` / `sync`
  (health). Shared URL encoding lives in `google.rs`. Public names are
  re-exported from `calendar/mod.rs` and `lib.rs`.
- **Persistence**: `models/calendar.rs` (row types), `repo/calendar.rs`
  (traits + SQL), `apps/worker/src/db/calendar.rs` (D1 impls).
- **Worker HTTP**: `apps/worker/src/calendar/http.rs` (REST),
  `calendar/webhook.rs` (push notifications), and `apps/worker/src/queue.rs`
  (queue consumer). Route wiring stays in `apps/worker/src/lib.rs`.

V4 writes land in `calendar/write.rs` (facade), `journal.rs` (insert),
`write_journal.rs` (patch/delete), and `repair.rs` (GET-only recovery).
