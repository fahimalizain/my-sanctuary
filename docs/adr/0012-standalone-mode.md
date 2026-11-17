# ADR 0012: Standalone mode (zero-Google local development)

Status: Accepted
Date: 2026-11-17

> Adds a per-user `integration_type` and a local username/password identity so
> the app can be developed and used with no Google OAuth credentials and no
> calls to Google. Production is fail-closed: standalone auth is off unless it
> is explicitly enabled *and* the request arrives on a non-public host.

## Context

Local development currently requires a Google OAuth client, a Google account
with a calendar grant, and network calls to `accounts.google.com`,
`oauth2.googleapis.com`, and `www.googleapis.com`. That makes the app
unusable offline and forces every contributor through a Google Cloud Console
setup before the calendar, tasks, agenda, reports, or MCP surfaces work.

The write path is already the right shape for a local stand-in: every write
goes through `api_core::HttpClient`, is journaled in
`calendar_event_operations`, and is then cached into `calendar_events`. If
that `HttpClient` can satisfy the journal locally, the rest of the app — the
cache, the timer, the agenda, the digest — needs no special case.

Constraints that drive the design:

- `users` and `google_oauth_tokens` are Google-shaped, but rebuilding the
  `users` table would be a risky migration for an app with real data.
- `EVENT_LIST_BY_USER_ID_AND_TIME_RANGE_SQL` and
  `EVENT_LIST_RUNNING_BY_USER_ID_SQL` only return events whose calendar has
  `sync_enabled = 1`.
- The cron fallback and elongate paths select across all users; without a
  filter they would pick up standalone calendars.
- Cloudflare Workers caps PBKDF2 at 100,000 iterations in WebCrypto.

## Terms

- **Google user**: the existing identity, provider `google`, authenticated by
  the OAuth callback, with a refreshable token and real Google calendars.
- **Standalone user**: a local identity (`integration_type = standalone`),
  authenticated by username/password, whose one calendar is a local
  stand-in served by `StandaloneCalendarHttp`.
- **Stand-in**: the read-only, repo-backed `HttpClient` that answers the write
  journal's Google calls from the cached `calendar_events` row. It never
  writes `calendar_events`; the journal's cache upsert remains the only writer.

## Decision

### Identity and schema

- New column `users.integration_type TEXT NOT NULL DEFAULT 'google'`
  (migration `0024_standalone.sql`). `google` is the default so every existing
  row and every Google login is unchanged. `users` is **not** rebuilt.
- `IntegrationType { Google, Standalone }` in api-core, serde-lowercase,
  `Default = Google`. `User` and `SessionUser` carry the field;
  `SessionUser` uses `#[serde(default)]` so a session cookie sealed before
  this change still decodes as a Google user.
- New table `local_credentials` (`user_id` PK, `username` UNIQUE,
  `password_hash`, `created_at`, `updated_at`, FK to `users` with
  `ON DELETE CASCADE`). Usernames are stored trimmed and lowercased; the
  UNIQUE index is the registration race loser's 409.
- A standalone user is created explicitly (`POST /auth/standalone/register`),
  never auto-provisioned from a Google identity. The row gets a synthetic
  `google_id = 'standalone:<uuid>'` and `<username>@standalone.invalid` email
  so the existing NOT NULL/UNIQUE columns stay satisfied. `.invalid` is
  reserved by RFC 2606 and can never collide with a real Google email.

### Passwords

- PBKDF2-HMAC-SHA256 in api-core (`password.rs`), pure Rust (`pbkdf2` +
  the existing `sha2`), 210,000 iterations, 16-byte random salt, 32-byte
  derived key, constant-time compare (`subtle`).
- Stored format `pbkdf2-sha256$<iterations>$<salt_b64>$<hash_b64>` (standard
  base64). The iteration count travels with the hash, so it can be raised
  later without invalidating old rows. Malformed strings verify as `false`.
- WebCrypto is deliberately not used: Workers caps PBKDF2 at 100,000
  iterations. `uuid` is deliberately not added; salts use the existing
  `getrandom` 0.2 dependency to avoid a second `getrandom` version.

### Production gate

`Config::standalone_enabled_for(request_host)` is the single server-side
check, evaluated on every standalone auth endpoint and in calendar access:

1. `ALLOW_STANDALONE_AUTH == "true"` (exact match; default off).
2. `SECURE_COOKIE != "true"` (production sessions are always secure).
3. The configured `FRONTEND_URL` host is non-public *and* the incoming
   request host is non-public. `None` (missing/unparseable) fails closed.

Non-public means `localhost`, `*.localhost`, `*.local`, `*.lan`, loopback
(127/8, `::1`), RFC 1918 (10/8, 172.16/12, 192.168/16), or link-local
(169.254/16, `fe80::/10`). Ports and IPv6 brackets are stripped; parse
errors fail closed. There is no `STANDALONE_ALLOWED_HOSTS`: a public
allowlist would reopen the production hole, and the private ranges already
cover phones on the LAN.

`GET /auth/providers` reports `{google, standalone}` so the login page never
offers a form the server would reject.

### Standalone calendar row

A standalone user owns exactly one `google_calendars` row, seeded
idempotently (`INSERT … ON CONFLICT(user_id, google_calendar_id) DO NOTHING`)
on register, on login, and in the standalone list paths when the snapshot is
empty:

- `google_calendar_id = 'standalone:<user_id>'`, `summary = 'Personal'`.
- `is_primary = 1`, `access_role = 'owner'`.
- **`sync_enabled = 1`, `sync_status = 'disabled'`.**
- `initial_sync_complete = 1`, `last_synced_at` / `last_success_at = now`.
- `event_labels` = the 24 `GOOGLE_EVENT_LABEL_COLORS` as
  `[{"id":"1"…"24","backgroundColor":…}]`, `event_labels_updated_at = now`.
- `watch_coverage = 'covered'`, `event_coverage = 'complete'`.

**Why `sync_enabled = 1`:** the event-range query behind the calendar view
and the running-task query behind the running chips both filter on
`sync_enabled = 1`. With `0`, standalone events would save but never appear.

**Why `sync_status = 'disabled'`:** it keeps `calendar_sync_view` `Disabled`
(non-stale), `aggregate_sync_status` `ready`, the sync-health banner quiet,
`classify_operator_warning` `None`, and `request_calendar_repair` a 400.
Nothing tries to sync, repair, or watch a calendar that cannot be synced.
`sync_enabled = 1` alone is not enough to enter a replica walk: the fallback
cron's user list is filtered to Google users, so standalone rows are never
candidates.

### Cron and queue filters

The cron's cross-user queries add
`AND user_id IN (SELECT id FROM users WHERE integration_type = 'google')`:
`CALENDAR_LIST_USER_IDS_SQL`, `CALENDAR_LIST_SYNC_ENABLED_SQL`,
`TASK_LIST_IN_PROGRESS_SQL`, and `OCCURRENCE_LIST_IN_PROGRESS_SQL`.
Subqueries (not joins) keep `SELECT *` row shapes unchanged. Queue messages
that resolve to a standalone-owned calendar are acked as a defense-in-depth
no-op.

### Stand-in limits (v1)

`StandaloneCalendarHttp` answers only the endpoints the write journal calls:
`events.insert`, `events.get`, `events.patch`, `events.move`, and
`calendars.get` (labels). Everything else is a loud error:
`events.list`, `calendarList`, and `post_form` must never be reached for a
standalone user. `events.watch` / `channels.stop` return `501`, never `404`:
with a public `WATCH_CALLBACK_URL` (a tunnel), a `404` would run
`set_sync_enabled(false)` and hide the user's events; the cost is one logged
`sync_errors` line per request.

Because the stand-in rebuilds responses from the cached row's `raw_json`:

- It never writes `calendar_events` — the journal's cache upsert does.
- It resolves calendars by `(user_id, google_calendar_id)`, never by the
  Google calendar id alone.
- It normalizes `dateTime` to UTC `…Z` and replaces `start`/`end` objects
  whole on patch, because the range/running SQL compare strings
  lexicographically.
- `etag`s are unique per write (`sequence` + random bytes), never the
  one-second request timestamp, so two patches in the same second still
  advance the journal's `If-Match` and a stale write gets a `412` and retry.

## Consequences

- Zero-Google local dev: no `GOOGLE_CREDENTIALS_JSON`, no Google requests.
  Google users are unchanged, and both login paths can coexist.
- Standalone has no recurring events, no attendees, no push channels, and no
  Google-side edits. Recurrence/`events.list`/calendarList are not emulated.
- Cron elongation is skipped for standalone in v1: a running chip stops
  growing after ~15 minutes, but stop/pause/complete still patch
  `end = now`, so recorded durations stay correct.
- The registration form captures the browser IANA zone
  (`Intl.DateTimeFormat().resolvedOptions().timeZone`), with `UTC` only as a
  fallback. Agenda "today", occurrence starts, and the 5-minute grid depend
  on it.
- `wrangler dev --remote` points at the **production** D1. Never use it for
  standalone development; use local D1 or a dedicated preview database.
- **Migrations before deploy.** `0024` must be applied to the target D1
  (`wrangler d1 migrations apply <db> --remote`) *before* deploying code that
  reads `integration_type`. An un-migrated database breaks API-key auth
  (`API_KEY_AUTHENTICATE_SQL` selects the column) and errors every cron tick.
- `wrangler dev` needs `--test-scheduled` to fire cron locally, so nothing
  silently relies on a scheduled tick in a zero-Google dev loop.
- Touches: migration `0024`, `IntegrationType`/`User`/`SessionUser`,
  `Config::standalone_enabled_for`, `local_credentials` + password hashing,
  `UserRepo::create_standalone`, `CalendarRepo::insert_standalone`, the four
  cron filters, the standalone calendar stand-in, the worker auth routes and
  handler rewiring, the web login/settings/banner, and this ADR.
