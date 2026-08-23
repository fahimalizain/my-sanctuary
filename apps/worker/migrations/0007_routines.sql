CREATE TABLE IF NOT EXISTS routines (
	id TEXT PRIMARY KEY,
	user_id TEXT NOT NULL,
	title TEXT NOT NULL,
	estimated_minutes INTEGER NOT NULL DEFAULT 15,
	rrule TEXT NOT NULL,
	sort_order INTEGER NOT NULL DEFAULT 0,
	created_at TEXT NOT NULL DEFAULT (datetime('now')),
	updated_at TEXT NOT NULL DEFAULT (datetime('now')),
	deleted_at TEXT,
	FOREIGN KEY (user_id) REFERENCES users(id)
);

CREATE INDEX IF NOT EXISTS idx_routines_user_living
	ON routines(user_id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_routines_user_sort
	ON routines(user_id, sort_order);

CREATE TABLE IF NOT EXISTS routine_occurrences (
	id TEXT PRIMARY KEY,
	routine_id TEXT NOT NULL,
	user_id TEXT NOT NULL,
	local_date TEXT NOT NULL,
	title TEXT,
	status TEXT NOT NULL DEFAULT 'pending',
	calendar_id TEXT,
	google_event_id TEXT,
	created_at TEXT NOT NULL DEFAULT (datetime('now')),
	updated_at TEXT NOT NULL DEFAULT (datetime('now')),
	UNIQUE (routine_id, local_date),
	FOREIGN KEY (routine_id) REFERENCES routines(id),
	FOREIGN KEY (user_id) REFERENCES users(id)
);

CREATE INDEX IF NOT EXISTS idx_occurrences_user_date
	ON routine_occurrences(user_id, local_date);
CREATE INDEX IF NOT EXISTS idx_occurrences_routine
	ON routine_occurrences(routine_id);

CREATE TABLE IF NOT EXISTS agenda_items (
	id TEXT PRIMARY KEY,
	user_id TEXT NOT NULL,
	local_date TEXT NOT NULL,
	kind TEXT NOT NULL,
	ref_id TEXT NOT NULL,
	sort_order INTEGER NOT NULL DEFAULT 0,
	created_at TEXT NOT NULL DEFAULT (datetime('now')),
	updated_at TEXT NOT NULL DEFAULT (datetime('now')),
	UNIQUE (user_id, local_date, kind, ref_id),
	FOREIGN KEY (user_id) REFERENCES users(id)
);

CREATE INDEX IF NOT EXISTS idx_agenda_items_user_date_sort
	ON agenda_items(user_id, local_date, sort_order);
