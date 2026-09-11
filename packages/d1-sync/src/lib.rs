//! Local D1 (SQLite) harness for calendar sync integration tests.
//!
//! Opens a temp SQLite database, applies `apps/worker/migrations/*.sql` in
//! name order (the same SQL wrangler `--local` applies), and exposes rusqlite
//! repo implementations over api-core SQL constants.

mod repos;

pub use repos::{
    SqliteCalendarEventRepo, SqliteCalendarEventOperationRepo, SqliteCalendarRepo, SqliteTokenRepo,
    SqliteWatchChannelRepo,
};

use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::{fs, io};

/// Shared connection handle used by all repo structs (one D1 database).
pub type Db = Arc<Mutex<Connection>>;

/// Opened harness: temp path (if file-backed), connection, and repo handles.
pub struct Harness {
    /// Temp file path when not `:memory:`; deleted on drop.
    _path: Option<PathBuf>,
    pub db: Db,
    pub calendars: SqliteCalendarRepo,
    pub events: SqliteCalendarEventRepo,
    pub watches: SqliteWatchChannelRepo,
    pub tokens: SqliteTokenRepo,
    pub operations: SqliteCalendarEventOperationRepo,
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(path) = self._path.take() {
            let _ = fs::remove_file(&path);
        }
    }
}

/// Resolve `apps/worker/migrations` relative to this crate's manifest dir.
pub fn migrations_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/worker/migrations")
}

/// Apply every `*.sql` migration file in sorted filename order.
pub fn apply_migrations(conn: &Connection) -> Result<(), String> {
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(|e| format!("PRAGMA foreign_keys: {e}"))?;

    let dir = migrations_dir();
    let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
        .map_err(|e| format!("read migrations dir {}: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("sql"))
        .collect();
    entries.sort();

    if entries.is_empty() {
        return Err(format!("no *.sql migrations in {}", dir.display()));
    }

    for path in entries {
        let sql = fs::read_to_string(&path)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        conn.execute_batch(&sql)
            .map_err(|e| format!("apply {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Open a unique temp-file SQLite DB, apply migrations, return repos.
pub fn open_harness() -> Result<Harness, String> {
    let path = unique_temp_path().map_err(|e| format!("temp path: {e}"))?;
    let conn = Connection::open(&path).map_err(|e| format!("open sqlite: {e}"))?;
    apply_migrations(&conn)?;
    let db = Arc::new(Mutex::new(conn));
    Ok(Harness {
        _path: Some(path),
        db: Arc::clone(&db),
        calendars: SqliteCalendarRepo::new(Arc::clone(&db)),
        events: SqliteCalendarEventRepo::new(Arc::clone(&db)),
        watches: SqliteWatchChannelRepo::new(Arc::clone(&db)),
        tokens: SqliteTokenRepo::new(Arc::clone(&db)),
        operations: SqliteCalendarEventOperationRepo::new(db),
    })
}

fn unique_temp_path() -> io::Result<PathBuf> {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "d1-sync-{}-{}.sqlite",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    Ok(path)
}

/// Seed user `u-1`, a far-future OAuth token, calendar `cal-1`, and list-state.
///
/// Does not log token values. Leaves `users.focused_task_id` NULL.
pub fn seed_user_token_calendar(
    conn: &Connection,
    opts: SeedOpts<'_>,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO users (id, google_id, email, name, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
        rusqlite::params![
            "u-1",
            "g-u-1",
            "user@example.com",
            "Test User",
            opts.created_at,
        ],
    )
    .map_err(|e| format!("seed user: {e}"))?;

    conn.execute(
        "INSERT INTO google_oauth_tokens
            (id, user_id, access_token, refresh_token, expiry, token_type, scope, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
        rusqlite::params![
            "tok-u-1",
            "u-1",
            opts.access_token,
            "rt-1",
            "2099-01-01T00:00:00Z",
            "Bearer",
            "calendar",
            opts.created_at,
        ],
    )
    .map_err(|e| format!("seed token: {e}"))?;

    conn.execute(
        "INSERT INTO google_calendars (
            id, user_id, google_calendar_id, summary, time_zone,
            is_primary, access_role, sync_enabled, sync_token, last_synced_at,
            event_labels, event_labels_updated_at,
            sync_query_fingerprint, sync_status, initial_sync_complete,
            last_attempt_at, last_success_at, last_error_code, failure_streak,
            next_retry_at, dirty_requested_generation, dirty_applied_generation,
            full_sync_requested, lease_owner, lease_expires_at, cache_revision,
            projection, watch_coverage, created_at, updated_at, deleted_at
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5,
            ?6, ?7, ?8, ?9, ?10,
            ?11, ?12,
            ?13, ?14, ?15,
            ?16, ?17, ?18, ?19,
            ?20, ?21, ?22,
            ?23, ?24, ?25, ?26,
            ?27, ?28, ?29, ?29, NULL
         )",
        rusqlite::params![
            opts.calendar_id,
            "u-1",
            opts.google_calendar_id,
            opts.summary,
            "UTC",
            1_i64, // is_primary
            "owner",
            if opts.sync_enabled { 1_i64 } else { 0_i64 },
            opts.sync_token,
            opts.last_synced_at,
            "[]",
            "2026-08-17T00:00:00Z",
            opts.sync_query_fingerprint,
            opts.sync_status,
            if opts.initial_sync_complete {
                1_i64
            } else {
                0_i64
            },
            opts.last_attempt_at,
            opts.last_success_at,
            opts.last_error_code,
            opts.failure_streak,
            opts.next_retry_at,
            opts.dirty_requested_generation,
            opts.dirty_applied_generation,
            0_i64, // full_sync_requested
            "",    // lease_owner
            Option::<String>::None, // lease_expires_at
            0_i64, // cache_revision
            "timed_masters_and_exceptions",
            "missing",
            opts.created_at,
        ],
    )
    .map_err(|e| format!("seed calendar: {e}"))?;

    conn.execute(
        "INSERT INTO google_calendar_list_state (user_id, sync_token, updated_at)
         VALUES (?1, ?2, ?3)",
        rusqlite::params!["u-1", opts.list_sync_token, opts.created_at],
    )
    .map_err(|e| format!("seed list state: {e}"))?;

    Ok(())
}

/// Options for [`seed_user_token_calendar`].
#[derive(Debug, Clone)]
pub struct SeedOpts<'a> {
    pub calendar_id: &'a str,
    pub google_calendar_id: &'a str,
    pub summary: &'a str,
    pub sync_enabled: bool,
    pub sync_token: &'a str,
    pub last_synced_at: Option<&'a str>,
    pub last_success_at: Option<&'a str>,
    pub last_attempt_at: Option<&'a str>,
    pub initial_sync_complete: bool,
    pub sync_status: &'a str,
    pub sync_query_fingerprint: &'a str,
    pub last_error_code: &'a str,
    pub failure_streak: i64,
    pub next_retry_at: Option<&'a str>,
    pub dirty_requested_generation: i64,
    pub dirty_applied_generation: i64,
    pub list_sync_token: &'a str,
    pub access_token: &'a str,
    pub created_at: &'a str,
}

impl Default for SeedOpts<'static> {
    fn default() -> Self {
        Self {
            calendar_id: "cal-1",
            google_calendar_id: "primary@example.com",
            summary: "Work",
            sync_enabled: true,
            sync_token: "old-tok",
            last_synced_at: Some("2023-11-14T21:00:00Z"),
            last_success_at: Some("2023-11-14T21:00:00Z"),
            last_attempt_at: None,
            initial_sync_complete: true,
            sync_status: "ready",
            sync_query_fingerprint: "",
            last_error_code: "",
            failure_streak: 0,
            next_retry_at: None,
            dirty_requested_generation: 0,
            dirty_applied_generation: 0,
            list_sync_token: "list-tok-1",
            access_token: "at-1",
            created_at: "2026-01-01T00:00:00Z",
        }
    }
}

/// Map a `SELECT *` rusqlite row into a serde value, then deserialize `T`.
///
/// INTEGER → i64, TEXT → string, NULL → null (so `de_d1_bool` / `de_empty_string` work).
pub fn row_to_value(row: &rusqlite::Row<'_>) -> Result<serde_json::Value, rusqlite::Error> {
    let stmt = row.as_ref();
    let mut map = serde_json::Map::new();
    for i in 0..stmt.column_count() {
        let name = stmt.column_name(i)?.to_string();
        let value = match row.get_ref(i)? {
            rusqlite::types::ValueRef::Null => serde_json::Value::Null,
            rusqlite::types::ValueRef::Integer(n) => serde_json::Value::from(n),
            rusqlite::types::ValueRef::Real(f) => serde_json::Value::from(f),
            rusqlite::types::ValueRef::Text(t) => {
                serde_json::Value::String(String::from_utf8_lossy(t).into_owned())
            }
            rusqlite::types::ValueRef::Blob(b) => {
                serde_json::Value::String(String::from_utf8_lossy(b).into_owned())
            }
        };
        map.insert(name, value);
    }
    Ok(serde_json::Value::Object(map))
}

/// Query zero-or-one row and deserialize via [`row_to_value`].
pub fn query_one<T: serde::de::DeserializeOwned>(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::types::ToSql],
) -> Result<Option<T>, api_core::repo::RepoError> {
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| api_core::repo::RepoError::Backend(e.to_string()))?;
    let mut rows = stmt
        .query(params)
        .map_err(|e| api_core::repo::RepoError::Backend(e.to_string()))?;
    match rows.next() {
        Ok(Some(row)) => {
            let value = row_to_value(row)
                .map_err(|e| api_core::repo::RepoError::Backend(e.to_string()))?;
            let parsed: T = serde_json::from_value(value)
                .map_err(|e| api_core::repo::RepoError::Backend(e.to_string()))?;
            Ok(Some(parsed))
        }
        Ok(None) => Ok(None),
        Err(e) => Err(api_core::repo::RepoError::Backend(e.to_string())),
    }
}

/// Query many rows and deserialize via [`row_to_value`].
pub fn query_vec<T: serde::de::DeserializeOwned>(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::types::ToSql],
) -> Result<Vec<T>, api_core::repo::RepoError> {
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| api_core::repo::RepoError::Backend(e.to_string()))?;
    let mut rows = stmt
        .query(params)
        .map_err(|e| api_core::repo::RepoError::Backend(e.to_string()))?;
    let mut out = Vec::new();
    loop {
        match rows.next() {
            Ok(Some(row)) => {
                let value = row_to_value(row)
                    .map_err(|e| api_core::repo::RepoError::Backend(e.to_string()))?;
                let parsed: T = serde_json::from_value(value)
                    .map_err(|e| api_core::repo::RepoError::Backend(e.to_string()))?;
                out.push(parsed);
            }
            Ok(None) => break,
            Err(e) => return Err(api_core::repo::RepoError::Backend(e.to_string())),
        }
    }
    Ok(out)
}

/// Execute a statement; return rows changed.
pub fn exec_changes(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::types::ToSql],
) -> Result<usize, api_core::repo::RepoError> {
    conn.execute(sql, params)
        .map_err(|e| api_core::repo::RepoError::Backend(e.to_string()))
}

/// Execute a statement; discard changes count.
pub fn exec(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::types::ToSql],
) -> Result<(), api_core::repo::RepoError> {
    exec_changes(conn, sql, params).map(|_| ())
}
