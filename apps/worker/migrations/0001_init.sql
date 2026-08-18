CREATE TABLE IF NOT EXISTS users (
	id TEXT PRIMARY KEY,
	google_id TEXT NOT NULL UNIQUE,
	email TEXT NOT NULL UNIQUE,
	name TEXT NOT NULL,
	picture TEXT,
	created_at TEXT NOT NULL DEFAULT (datetime('now')),
	updated_at TEXT NOT NULL DEFAULT (datetime('now')),
	deleted_at TEXT
);

CREATE TABLE IF NOT EXISTS google_oauth_tokens (
	id TEXT PRIMARY KEY,
	user_id TEXT NOT NULL UNIQUE,
	access_token TEXT NOT NULL,
	refresh_token TEXT,
	expiry TEXT NOT NULL,
	token_type TEXT NOT NULL DEFAULT 'Bearer',
	scope TEXT,
	created_at TEXT NOT NULL DEFAULT (datetime('now')),
	updated_at TEXT NOT NULL DEFAULT (datetime('now')),
	deleted_at TEXT,
	FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS google_calendars (
	id TEXT PRIMARY KEY,
	user_id TEXT NOT NULL,
	google_calendar_id TEXT NOT NULL,
	summary TEXT,
	time_zone TEXT,
	is_primary INTEGER NOT NULL DEFAULT 0,
	access_role TEXT,
	sync_enabled INTEGER NOT NULL DEFAULT 1,
	sync_token TEXT,
	last_synced_at TEXT,
	created_at TEXT NOT NULL DEFAULT (datetime('now')),
	updated_at TEXT NOT NULL DEFAULT (datetime('now')),
	deleted_at TEXT,
	UNIQUE (user_id, google_calendar_id),
	FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS calendar_events (
	id TEXT PRIMARY KEY,
	calendar_id TEXT NOT NULL,
	google_event_id TEXT NOT NULL,
	google_etag TEXT,
	google_updated_at TEXT,
	last_synced_at TEXT NOT NULL,
	title TEXT NOT NULL,
	description TEXT,
	start_time TEXT NOT NULL,
	end_time TEXT NOT NULL,
	recurrence TEXT,
	created_at TEXT NOT NULL DEFAULT (datetime('now')),
	updated_at TEXT NOT NULL DEFAULT (datetime('now')),
	deleted_at TEXT,
	UNIQUE (calendar_id, google_event_id),
	FOREIGN KEY (calendar_id) REFERENCES google_calendars(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_users_deleted ON users(deleted_at);
CREATE INDEX IF NOT EXISTS idx_tokens_deleted ON google_oauth_tokens(deleted_at);
CREATE INDEX IF NOT EXISTS idx_calendars_user ON google_calendars(user_id);
CREATE INDEX IF NOT EXISTS idx_calendars_google_id ON google_calendars(user_id, google_calendar_id);
CREATE INDEX IF NOT EXISTS idx_calendars_deleted ON google_calendars(deleted_at);
CREATE INDEX IF NOT EXISTS idx_events_calendar_start ON calendar_events(calendar_id, start_time);
CREATE INDEX IF NOT EXISTS idx_events_cal_google ON calendar_events(calendar_id, google_event_id);
CREATE INDEX IF NOT EXISTS idx_events_deleted ON calendar_events(deleted_at);