-- Per-user Google calendarList.list sync cursor (issue #50 / Vertical 3).
--
-- Absence of a row, or an empty sync_token, means "no cursor → full list".
-- Cron walks calendarList incrementally so added calendars appear and removed
-- ones can be orphaned/disabled without a one-shot re-import.

CREATE TABLE IF NOT EXISTS google_calendar_list_state (
  user_id TEXT PRIMARY KEY,
  sync_token TEXT NOT NULL DEFAULT '',
  updated_at TEXT NOT NULL,
  FOREIGN KEY (user_id) REFERENCES users(id)
);
