CREATE TABLE IF NOT EXISTS task_lists (
	id TEXT PRIMARY KEY,
	user_id TEXT NOT NULL,
	name TEXT NOT NULL,
	color TEXT NOT NULL,
	sort_order INTEGER NOT NULL DEFAULT 0,
	created_at TEXT NOT NULL DEFAULT (datetime('now')),
	updated_at TEXT NOT NULL DEFAULT (datetime('now')),
	deleted_at TEXT,
	FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS task_categories (
	id TEXT PRIMARY KEY,
	user_id TEXT NOT NULL,
	list_id TEXT,
	parent_id TEXT,
	title TEXT NOT NULL,
	slug TEXT NOT NULL,
	color TEXT NOT NULL DEFAULT '',
	is_productive INTEGER NOT NULL DEFAULT 0,
	google_calendar_id TEXT,
	google_color_id TEXT,
	sort_order INTEGER NOT NULL DEFAULT 0,
	is_untracked INTEGER NOT NULL DEFAULT 0,
	created_at TEXT NOT NULL DEFAULT (datetime('now')),
	updated_at TEXT NOT NULL DEFAULT (datetime('now')),
	deleted_at TEXT,
	FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
	FOREIGN KEY (list_id) REFERENCES task_lists(id),
	FOREIGN KEY (parent_id) REFERENCES task_categories(id)
);

CREATE TABLE IF NOT EXISTS task_category_patterns (
	id TEXT PRIMARY KEY,
	category_id TEXT NOT NULL,
	regex TEXT NOT NULL,
	google_calendar_id TEXT,
	sort_order INTEGER NOT NULL DEFAULT 0,
	created_at TEXT NOT NULL DEFAULT (datetime('now')),
	updated_at TEXT NOT NULL DEFAULT (datetime('now')),
	FOREIGN KEY (category_id) REFERENCES task_categories(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS tasks (
	id TEXT PRIMARY KEY,
	user_id TEXT NOT NULL,
	title TEXT NOT NULL,
	description TEXT,
	duration_minutes INTEGER NOT NULL DEFAULT 15,
	priority TEXT NOT NULL DEFAULT 'medium',
	status TEXT NOT NULL DEFAULT 'OPEN',
	created_at TEXT NOT NULL DEFAULT (datetime('now')),
	updated_at TEXT NOT NULL DEFAULT (datetime('now')),
	deleted_at TEXT,
	FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS task_logs (
	id TEXT PRIMARY KEY,
	task_id TEXT NOT NULL,
	user_id TEXT NOT NULL,
	type TEXT NOT NULL,
	at TEXT NOT NULL,
	calendar_id TEXT,
	google_event_id TEXT,
	created_at TEXT NOT NULL DEFAULT (datetime('now')),
	FOREIGN KEY (task_id) REFERENCES tasks(id),
	FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

-- Slice 4 wires tasks to calendar events; the column is created now so that
-- later slices do not need another migration. Existing rows carry NULL.
ALTER TABLE calendar_events ADD COLUMN task_id TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS idx_task_categories_user_slug
	ON task_categories(user_id, slug) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_task_lists_user ON task_lists(user_id);
CREATE INDEX IF NOT EXISTS idx_task_lists_deleted ON task_lists(deleted_at);
CREATE INDEX IF NOT EXISTS idx_task_categories_user ON task_categories(user_id);
CREATE INDEX IF NOT EXISTS idx_task_categories_list ON task_categories(list_id);
CREATE INDEX IF NOT EXISTS idx_task_categories_parent ON task_categories(parent_id);
CREATE INDEX IF NOT EXISTS idx_task_categories_deleted ON task_categories(deleted_at);
CREATE INDEX IF NOT EXISTS idx_task_category_patterns_category ON task_category_patterns(category_id);
CREATE INDEX IF NOT EXISTS idx_tasks_user ON tasks(user_id);
CREATE INDEX IF NOT EXISTS idx_tasks_deleted ON tasks(deleted_at);
CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
CREATE INDEX IF NOT EXISTS idx_task_logs_task ON task_logs(task_id);
CREATE INDEX IF NOT EXISTS idx_task_logs_user ON task_logs(user_id);
CREATE INDEX IF NOT EXISTS idx_events_task ON calendar_events(task_id);
