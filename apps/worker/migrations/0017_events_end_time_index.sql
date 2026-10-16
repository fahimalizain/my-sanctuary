-- Range-list index for GET /api/calendar/events.
--
-- The overlap query filters `start_time < window_end AND end_time >
-- window_start` per calendar. idx_events_calendar_start can only bound the
-- first term, so SQLite walked every event since the beginning of history.
-- (calendar_id, end_time, start_time) bounds the scan from below at the
-- window start (history is skipped) and carries start_time so rows that begin
-- after the window are rejected from the index without a table lookup.
CREATE INDEX IF NOT EXISTS idx_events_calendar_end
	ON calendar_events(calendar_id, end_time, start_time);
