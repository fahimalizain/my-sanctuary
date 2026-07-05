# Migrations

The two backends use **separate migration strategies**. The schema is defined
twice — once declaratively in Go struct tags, once imperatively in SQL files.

## Local dev — GORM AutoMigrate

`apps/api/main.go` calls `repository.AutoMigrate(db)` on startup. GORM inspects
the struct tags on the models and issues `CREATE TABLE` / `ALTER TABLE` /
`CREATE INDEX` as needed. No SQL files exist for local dev.

```go
func AutoMigrate(db *gorm.DB) error {
    return db.AutoMigrate(
        &models.User{},
        &models.GoogleOAuthToken{},
        &models.GoogleCalendar{},
        &models.CalendarEvent{},
    )
}
```

**AutoMigrate is additive-only:** it creates missing tables, adds missing
columns, and creates missing indexes. It will **not** drop a column, rename
one, or change a column type. For non-additive changes the developer must
either drop the local SQLite file (acceptable in dev — no precious local
data) or run raw `ALTER TABLE` statements by hand.

## Cloudflare Workers — `wrangler d1 migrations`

D1 has no GORM at runtime (the `js` build excludes `gorm_repo.go` via build
tags), so the schema is created by explicit SQL files applied via the Wrangler
CLI:

```bash
npx wrangler d1 migrations create sanctuary-db 0001_init
npx wrangler d1 migrations apply sanctuary-db --remote   # production
npx wrangler d1 migrations apply sanctuary-db --local    # wrangler dev
```

D1 tracks applied migrations in a `d1_migrations` system table. Migrations are
numbered SQL files (`0001_init.sql`, `0002_*.sql`, …) applied in order.

### Initial migration

Source: [`apps/cloudflare-deploy/migrations/0001_init.sql`](../../apps/cloudflare-deploy/migrations/0001_init.sql)

```sql
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
    primary INTEGER NOT NULL DEFAULT 0,
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
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    deleted_at TEXT,
    UNIQUE (calendar_id, google_event_id),
    FOREIGN KEY (calendar_id) REFERENCES google_calendars(id) ON DELETE CASCADE
);
```

**Table ordering matters:** `google_calendars` is created before
`calendar_events` because `calendar_events` has a FK to `google_calendars(id)`.

### Indexes

```sql
CREATE INDEX IF NOT EXISTS idx_users_deleted ON users(deleted_at);
CREATE INDEX IF NOT EXISTS idx_tokens_deleted ON google_oauth_tokens(deleted_at);
CREATE INDEX IF NOT EXISTS idx_calendars_user ON google_calendars(user_id);
CREATE INDEX IF NOT EXISTS idx_calendars_google_id ON google_calendars(user_id, google_calendar_id);
CREATE INDEX IF NOT EXISTS idx_calendars_deleted ON google_calendars(deleted_at);
CREATE INDEX IF NOT EXISTS idx_events_calendar_start ON calendar_events(calendar_id, start_time);
CREATE INDEX IF NOT EXISTS idx_events_cal_google ON calendar_events(calendar_id, google_event_id);
CREATE INDEX IF NOT EXISTS idx_events_deleted ON calendar_events(deleted_at);
```

### Type conventions

- **Dates as `TEXT` (ISO 8601).** Both GORM's SQLite driver and D1 serialize
  `time.Time` to ISO 8601 strings. Keeping the column affinity `TEXT` in both
  environments avoids type-mismatch bugs.
- **Booleans as `INTEGER`.** SQLite/D1 have no native boolean affinity; store
  `primary` and `sync_enabled` as `INTEGER` 0/1. The GORM model uses Go `bool`,
  which the SQLite driver maps to/from `INTEGER` automatically.

## Schema evolution beyond 0001

The two backends diverge in how they handle subsequent migrations.

**Local dev (GORM AutoMigrate)** is additive-only and automatic. It runs on
every `apps/api` startup, so local dev never falls behind on *additive* schema
changes. For destructive changes, drop the local SQLite file.

**D1 (`wrangler d1 migrations`)** is explicit and ordered. Destructive changes
(`DROP COLUMN`, `RENAME`, type changes) are first-class — they're just SQL.
The risk is forgetting to write the D1 migration that mirrors a Go struct tag
change.

### Discipline to avoid drift

1. Any change to a `models.go` struct tag **must** be accompanied by a new
   `apps/cloudflare-deploy/migrations/NNNN_*.sql` file in the same commit.
2. AutoMigrate is a dev convenience, not a migration tool. Never rely on it
   for the D1/production schema.
3. The `0001_init.sql` file is the source of truth for the *initial* D1 schema;
   the Go struct tags are the source of truth for the *current* GORM schema.
   When they diverge after a change, the new `NNNN_*.sql` migration is what
   brings D1 back in line.
4. Future CI step: run `gorm.AutoMigrate` against a scratch SQLite DB and diff
   the resulting schema against the latest D1 migration SQL — this catches
   drift automatically.

## `wrangler.toml`

The D1 binding is declared in [`apps/cloudflare-deploy/wrangler.toml`](../../apps/cloudflare-deploy/wrangler.toml):

```toml
[[d1_databases]]
binding = "DB"
database_name = "sanctuary-db"
database_id = ""  # set after `wrangler d1 create`
```