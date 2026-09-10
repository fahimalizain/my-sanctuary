-- Calendar sync health + Google event identity columns (issue #50 / ADR 0005).
--
-- Health lives on `google_calendars` (not a side table) so the sync token,
-- cached events, and replica health stay in one D1 database. calendarList
-- upsert must never touch these health columns (same invariant as
-- `event_labels`). `task_id` remains the only app-owned event column protected
-- from Google overwrite via COALESCE on upsert.
--
-- UNIQUE (calendar_id, google_event_id) is unchanged — never add a global
-- unique on google_event_id.

-- ── google_calendars: replica health ────────────────────────────────────────

ALTER TABLE google_calendars ADD COLUMN sync_query_fingerprint TEXT NOT NULL DEFAULT '';
-- never_initialized | ready | retrying | rebuilding | authorization_required | disabled
ALTER TABLE google_calendars ADD COLUMN sync_status TEXT NOT NULL DEFAULT 'never_initialized';
ALTER TABLE google_calendars ADD COLUMN initial_sync_complete INTEGER NOT NULL DEFAULT 0;
ALTER TABLE google_calendars ADD COLUMN last_attempt_at TEXT;
ALTER TABLE google_calendars ADD COLUMN last_success_at TEXT;
ALTER TABLE google_calendars ADD COLUMN last_error_code TEXT NOT NULL DEFAULT '';
ALTER TABLE google_calendars ADD COLUMN failure_streak INTEGER NOT NULL DEFAULT 0;
ALTER TABLE google_calendars ADD COLUMN next_retry_at TEXT;
ALTER TABLE google_calendars ADD COLUMN dirty_requested_generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE google_calendars ADD COLUMN dirty_applied_generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE google_calendars ADD COLUMN full_sync_requested INTEGER NOT NULL DEFAULT 0;
ALTER TABLE google_calendars ADD COLUMN lease_owner TEXT NOT NULL DEFAULT '';
ALTER TABLE google_calendars ADD COLUMN lease_expires_at TEXT;
ALTER TABLE google_calendars ADD COLUMN cache_revision INTEGER NOT NULL DEFAULT 0;
-- current product projection; all-day still out of projection
ALTER TABLE google_calendars ADD COLUMN projection TEXT NOT NULL DEFAULT 'timed_masters_and_exceptions';

-- Production calendars that already have last_synced_at must not look
-- never-initialized after this migration.
UPDATE google_calendars
SET
  last_success_at = last_synced_at,
  initial_sync_complete = CASE
    WHEN last_synced_at IS NOT NULL AND TRIM(last_synced_at) != '' THEN 1
    ELSE 0
  END,
  sync_status = CASE
    WHEN sync_enabled = 0 THEN 'disabled'
    WHEN last_synced_at IS NOT NULL AND TRIM(last_synced_at) != '' THEN 'ready'
    ELSE 'never_initialized'
  END
WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_calendars_sync_status
  ON google_calendars (sync_status)
  WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_calendars_next_retry
  ON google_calendars (next_retry_at)
  WHERE deleted_at IS NULL;

-- ── calendar_events: Google identity columns ────────────────────────────────
-- Google-owned; merge may overwrite. task_id stays COALESCE-protected.

ALTER TABLE calendar_events ADD COLUMN ical_uid TEXT NOT NULL DEFAULT '';
ALTER TABLE calendar_events ADD COLUMN sequence INTEGER NOT NULL DEFAULT 0;
ALTER TABLE calendar_events ADD COLUMN status TEXT NOT NULL DEFAULT '';
ALTER TABLE calendar_events ADD COLUMN recurring_event_id TEXT NOT NULL DEFAULT '';
ALTER TABLE calendar_events ADD COLUMN original_start TEXT NOT NULL DEFAULT '';
ALTER TABLE calendar_events ADD COLUMN start_time_zone TEXT NOT NULL DEFAULT '';
ALTER TABLE calendar_events ADD COLUMN end_time_zone TEXT NOT NULL DEFAULT '';
ALTER TABLE calendar_events ADD COLUMN is_all_day INTEGER NOT NULL DEFAULT 0;
ALTER TABLE calendar_events ADD COLUMN raw_json TEXT NOT NULL DEFAULT '';
