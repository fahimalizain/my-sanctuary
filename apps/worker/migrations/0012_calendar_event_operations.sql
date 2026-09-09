-- Outbound calendar write journal (issue #50 / Vertical 4).
--
-- Durable record of intended Google Calendar writes so a crash after Google
-- commits (but before the local cache applies) can be repaired. This is a
-- journal, not a domain entity — no soft-delete, no TTL; rows stay forever
-- for a personal app (same spirit as watch channels).
--
-- Status machine:
--   pending → google_committed → cache_applied
--   pending → failed
--   pending / google_committed → conflict  (after 412 retry cap)
--
-- Insert mints `google_event_id` (Google client-supplied id) before the HTTP
-- call and reuses it on retry. A 409 is treated as success via GET + merge.
-- Do not claim exactly-once delivery from a retry loop alone.
--
-- Never store OAuth tokens or access secrets in payload_json / last_error.
--
-- verb: insert | patch | delete
-- status: pending | google_committed | cache_applied | failed | conflict

CREATE TABLE IF NOT EXISTS calendar_event_operations (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  calendar_id TEXT NOT NULL,
  local_event_id TEXT NOT NULL DEFAULT '',
  google_event_id TEXT NOT NULL DEFAULT '',
  verb TEXT NOT NULL,
  payload_fingerprint TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  status TEXT NOT NULL,
  google_etag TEXT NOT NULL DEFAULT '',
  attempt_count INTEGER NOT NULL DEFAULT 0,
  last_error TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (user_id) REFERENCES users(id),
  FOREIGN KEY (calendar_id) REFERENCES google_calendars(id)
);

-- Cron repair lists pending / google_committed by age.
CREATE INDEX IF NOT EXISTS idx_calendar_event_operations_status_updated
  ON calendar_event_operations(status, updated_at);

-- Replica skip of in-flight writes for a calendar.
CREATE INDEX IF NOT EXISTS idx_calendar_event_operations_cal_google
  ON calendar_event_operations(calendar_id, google_event_id);

CREATE INDEX IF NOT EXISTS idx_calendar_event_operations_user_cal
  ON calendar_event_operations(user_id, calendar_id);
