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
- No Cloudflare Queue
- Request-path cache-only after first successful publication
  (`initial_sync_complete` / parseable `last_synced_at` as the *gate*, not as
  health)
- Watch 404 / `events.list` 404 disable sync

## Decision

### Destination (two paths)

V2 implements two distinct Google read shapes. They must not be collapsed:

- **Window fetch:** `timeMin` + `timeMax`, `singleEvents=true`, first paint /
  visible range. Any `nextSyncToken` from this query is **thrown away**.
- **Replica walk:** `singleEvents=false`, optional `timeMin ≈ now − 12 months`,
  no `timeMax`. **Only this walk owns `sync_token`.** Persist the token only
  after the last page is durably committed.

V1 still uses the single existing replica-shaped `events.list`
(`singleEvents=false`, no time bounds) on first paint only. The two-path split
is locked here so later verticals do not invent a third shape.

### Invariants

1. Never advance `syncToken` past work that is not durably committed.
2. Upsert by `(calendar_id, google_event_id)` — never a global unique on
   `google_event_id`.
3. One fenced owner per calendar (lease columns exist; fencing logic is later).
4. Watches are hints; the periodic incremental is the contract.
5. 410 is merge-full, not truncate (V2 implements merge-full; V1 keeps the
   in-invocation single retry and does not wipe rows).
6. Cancelled exceptions are kept rows (V2; columns exist). Ordinary cancelled
   events stay soft-deletes.
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

Product projection declared: `timed_masters_and_exceptions` (all-day still
out). `task_id` (and future notes) must not live in columns a Google merge
blindly overwrites — keep COALESCE / app-owned semantics permanent.

### What V1 shipped vs later

**V1 (this vertical)**

- Health columns on `google_calendars` + event identity columns on
  `calendar_events` (nullable / defaulted)
- `CalendarRepo::record_sync_{attempt,success,failure}`
- Sanitized `sync` envelope on `GET /api/calendar/events`
- Existing sync path records attempt vs success; missing terminal
  `nextSyncToken` is **not** publication success
- Request path still cache-only after first publication; stale is visible in
  the envelope, not a blocking resync
- Projection declared: `timed_masters_and_exceptions`

**Later**

- **V2:** two-path reads (live window vs replica apply), populate identity /
  `raw_json`, 410 merge-full, cancelled-exception retention
- **V3:** dirty generations + cron `notify_user` + watch as hint only
- **V4:** If-Match / operation journal / writes
- **V5:** web health chrome
- **Not planned:** Cloudflare Queues (ADR 0001)

## Consequences

- `last_synced_at` / `initial_sync_complete` remain the **request-path gate**
  only. Clients and operators read health from the sanitized `sync` envelope
  (`last_success_at`, `sync_status`, `stale`, `error_code`, …).
- A failed incremental leaves the previous cursor and success stamp intact;
  the next run can replay (upsert-by-id). Empty `items` + `nextSyncToken`
  still counts as success.
- Missing terminal `nextSyncToken` is classified as `missing_sync_token` and
  stored as `retrying` — never as a silent “keep old token and stamp success.”
- Tokens, OAuth credentials, lease secrets, event bodies, and `raw_json` never
  appear in the envelope or in `last_error_code`.
- Cron continues to key off `last_synced_at`; `record_sync_success` writes it,
  so the 15-minute backstop keeps working without a separate dirty channel
  (until V3).

## Residual risk

- V1 still has one replica-shaped fetch on first paint only; a long-stale
  cache is visible (`stale` / `degraded`) but not force-refetched on the
  request path until cron/webhook run.
- 410 is still an in-invocation single retry, not merge-full — residual
  truncate risk if a future change wipes rows on 410 before V2.
- No auto-reset after N failures: a permanently bad cursor stays until an
  operator or V2 merge-full path clears it.
- Identity columns and `raw_json` stay empty until V2; app-owned COALESCE is
  ready but unexercised by the replica apply path.
