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
