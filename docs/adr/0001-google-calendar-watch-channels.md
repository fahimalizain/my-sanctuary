# ADR 0001: Google Calendar watch channels

Status: Accepted
Date: 2026-08-18

## Context

Calendar sync is pull-only today: `GET /api/calendar/events` syncs on the request path, so changes from Google appear only when the user opens the app. We want Google to push changes to us instead. This ADR is slice 1 of N: it locks the design for `events.watch` push channels. Nothing is implemented yet; later slices implement against this document.

Two known defects are in scope to fix:

- Cancelled events are dropped by `is_skipped` instead of being soft-deleted (`delete_by_google_event_id`).
- There is no mechanism to catch up after a failed or missed sync.

## Decision

Use `events.watch` push channels on `sync_enabled` calendars, with a 15-minute cron as backstop. The following spec is the source of truth for all later slices.

### Out of scope

- No `calendarList.watch` — only `events.watch` on `sync_enabled` calendars.
- No Cloudflare Queue.
- No `watch_*` columns on `google_calendars`.

### Table `google_calendars_watch_channels`

| Column        | Type | Constraints                      | Purpose                                                                    |
| ------------- | ---- | -------------------------------- | -------------------------------------------------------------------------- |
| `id`          | TEXT | PK (UUIDv4)                      | Row identity                                                               |
| `calendar_id` | TEXT | NOT NULL → `google_calendars.id` | Owning calendar                                                            |
| `channel_id`  | TEXT | NOT NULL UNIQUE                  | UUID we mint; webhook lookup key (`X-Goog-Channel-ID`)                     |
| `resource_id` | TEXT | NOT NULL                         | Google's id; required to `channels.stop`                                   |
| `token`       | TEXT | NOT NULL                         | Secret we mint; compared to `X-Goog-Channel-Token`                         |
| `expiration`  | TEXT | NOT NULL                         | RFC 3339 UTC (rest of the schema uses ISO 8601 TEXT, never epoch integers) |
| `created_at`  | TEXT | NOT NULL                         | Row creation time                                                          |
| `updated_at`  | TEXT | NOT NULL                         | Last change time                                                           |

Rules:

- Many rows per calendar (`UNIQUE(channel_id)` only), so renewal overlap works (1:2 briefly).
- **Hard-delete on stop.** No `deleted_at`. This table is a subscription, not a domain entity; every other table stays soft-delete.
- FK `calendar_id REFERENCES google_calendars(id)`. Do **not** rely on `ON DELETE CASCADE` — parent deletes are soft (`deleted_at`), so CASCADE never fires.

```sql
CREATE TABLE IF NOT EXISTS google_calendars_watch_channels (
	id TEXT PRIMARY KEY,
	calendar_id TEXT NOT NULL,
	channel_id TEXT NOT NULL UNIQUE,
	resource_id TEXT NOT NULL,
	token TEXT NOT NULL,
	expiration TEXT NOT NULL,
	created_at TEXT NOT NULL DEFAULT (datetime('now')),
	updated_at TEXT NOT NULL DEFAULT (datetime('now')),
	FOREIGN KEY (calendar_id) REFERENCES google_calendars(id)
);

CREATE INDEX IF NOT EXISTS idx_watch_channels_calendar
	ON google_calendars_watch_channels(calendar_id);
CREATE INDEX IF NOT EXISTS idx_watch_channels_expiration
	ON google_calendars_watch_channels(expiration);
```

Indexes: `channel_id` (unique, via column constraint), `calendar_id`, `expiration`.

### Watch lifecycle

- Call `events.watch` as soon as a `google_calendars` row exists and `sync_enabled` is true.
- `GET /api/calendar/events` **ensure-watches**: any sync-enabled calendar with no unexpired channel gets `events.watch`. Covers deploy backfill and failed watches.
- Watch HTTP 404 → `set_sync_enabled(false)` (same as `events.list` 404). Other watch errors: log, leave enabled, retry later.
- `WATCH_CALLBACK_URL` env = **full** HTTPS callback URL (e.g. `https://my-sanctuary.fahimalizain.com/api/calendar/notifications`). Empty / non-HTTPS / localhost → ensure-watch is a no-op (local `wrangler dev` + local D1).
- Stop + hard-delete **every** channel row for a calendar when:
  - `sync_enabled` becomes false (including 404 auto-disable);
  - the calendar is soft-deleted.

### Read path (`ListEvents`)

- Await `sync_calendar` **only when `last_synced_at` is NULL** (first paint after calendar import).
- After that, `ListEvents` is **cache-only**. No 5-minute stale pull on the request path.
- Incremental cancelled events (`status: cancelled`) must call `delete_by_google_event_id` (already a soft delete). Today `is_skipped` drops them — that is a bug this design fixes. Timed creates/updates still upsert. All-day events (no `dateTime`) stay skipped.

### Webhook

- `POST` to the path in `WATCH_CALLBACK_URL`.
- Unauthenticated (Google cannot send the session cookie).
- Handle **outside** the workers-rs `Router` so the fetch `Context` is available (`Router` currently discards `_ctx`).
- Verify `X-Goog-Channel-ID` exists and `X-Goog-Channel-Token` matches the stored token with a **constant-time** compare.
- Unknown id, token mismatch, disabled/soft-deleted calendar → **200**, no work, log the miss. Never 401/404/500 for verify failures (no existence leak; no Google retry hammer).
- `X-Goog-Resource-State: sync` (handshake) → 200, no sync.
- `X-Goog-Resource-State: exists` → 200 immediately, then `ctx.wait_until(sync_calendar)`.
- Token refresh via existing `refresh_if_needed` using the calendar's `user_id` (no session).

### Fallback cron

- `#[event(scheduled)]` every 15 minutes (`*/15 * * * *` in wrangler.toml). None exists today.
- For each sync-enabled calendar: if `last_synced_at` is older than 15 minutes (or NULL), run `sync_calendar`. Webhooks already refresh `last_synced_at`, so the cron no-ops when watches work. No separate `last_webhook_at` column.
- Same job also **renews** watches: if a calendar has no channel with `expiration > now + 24h`, mint a new `events.watch` (new `channel_id`), then `channels.stop` + DELETE the old row. Overlap of two rows is expected and allowed.
- Cron iterates calendars across users and refreshes each user's Google token. New repository methods (`list` sync-enabled / expiring) will be needed; SQL beyond that intent is left to the implementing slice.

### Residual risk (documented, not fixed in this ADR)

- Google push is not 100% reliable — the 15-minute cron is the backstop.
- `APIs-Google` respects `robots.txt`. There is none today. A future `Disallow: /` would silently kill webhooks.
- Recurring events are still stored as masters (`singleEvents=false`); watch does not change that.
- All-day events remain skipped.

## Consequences

- One new D1 table plus two indexes; no changes to existing tables (`google_calendars` gains no `watch_*` columns).
- `ListEvents` becomes cache-only after first paint: steady-state requests stop hitting the Google API, at the cost of a slower first paint.
- Cancelled events stop silently disappearing; the webhook and cron converge on `last_synced_at`, keeping repeated syncs idempotent.
- Production must set `WATCH_CALLBACK_URL`; local dev skips watches by design.
- The webhook handler lives outside the `Router` and is unauthenticated; verification failures are swallowed into 200s by design.
- Later slices add: repository methods, the scheduled handler + wrangler.toml cron, watch/webhook handlers, and D1 migration 0002.
