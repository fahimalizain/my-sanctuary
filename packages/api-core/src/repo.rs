//! Repository traits, errors, and the D1 SQL statements.
//!
//! The traits live in api-core (pure Rust, unit-testable with fakes); the D1
//! implementations live in `apps/worker/src/db.rs`. SQL is a single source of
//! truth here so it can be reviewed and asserted in tests.
//!
//! Soft-delete rules:
//! - Reads filter `deleted_at IS NULL`.
//! - `TokenRepo::delete` is a soft delete (`UPDATE … SET deleted_at = ?`),
//!   fixing the old Go D1 bug that hard-deleted token rows.
//! - Both upserts set `deleted_at = NULL` on conflict, so a subsequent login
//!   resurrects a soft-deleted user/token row instead of tripping the UNIQUE
//!   constraint.

use async_trait::async_trait;
use thiserror::Error;

use crate::models::{
    CalendarEvent, GoogleCalendar, GoogleOAuthToken, NewCalendar, NewCalendarEvent, NewTask,
    NewTaskCategory, NewTaskCategoryPattern, NewTaskList, NewTaskLog, NewToken, NewUser, Task,
    TaskCategory, TaskCategoryPattern, TaskList, TaskLog, UpdateTask, UpdateTaskCategory,
    UpdateTaskList, User, WatchChannel, NewWatchChannel,
};

/// Errors surfaced by repository operations.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum RepoError {
    #[error("record not found")]
    NotFound,
    #[error("record conflicts with an existing one")]
    Conflict,
    #[error("database error: {0}")]
    Backend(String),
}

/// Identity persistence. No token methods here — tokens are [`TokenRepo`]'s job.
///
/// `#[async_trait(?Send)]`: the returned futures must not require `Send`
/// because the Worker's D1 (`js_sys` promises) and `worker::Fetch` futures are
/// `!Send` on wasm. The `Send + Sync` supertraits still hold — the D1 repos
/// wrap a `D1Database` (unsafe `Send + Sync` in the worker crate).
#[async_trait(?Send)]
pub trait UserRepo: Send + Sync {
    /// Returns the user with `id`, or `None` when absent or soft-deleted.
    async fn get_by_id(&self, id: &str) -> Result<Option<User>, RepoError>;
    /// Returns the user with `google_id`, or `None` when absent or soft-deleted.
    async fn get_by_google_id(&self, google_id: &str) -> Result<Option<User>, RepoError>;
    /// Inserts a new user or updates the existing one (keyed on `google_id`).
    /// Returns the stored row, including the DB-generated `id`.
    async fn upsert_by_google_id(&self, user: NewUser) -> Result<User, RepoError>;
}

/// Google OAuth token persistence.
#[async_trait(?Send)]
pub trait TokenRepo: Send + Sync {
    /// Returns the active token row for `user_id`, or `None` when absent or
    /// soft-deleted. Slice 4's token refresher will consume this.
    async fn get_by_user_id(&self, user_id: &str) -> Result<Option<GoogleOAuthToken>, RepoError>;
    /// Inserts or replaces the token row for `user_id`. A missing/empty
    /// `refresh_token` never blanks a stored one (see `TOKEN_UPSERT_SQL`).
    async fn upsert(&self, token: NewToken) -> Result<(), RepoError>;
    /// SOFT delete: stamps `deleted_at = now_rfc3339` on the active row.
    async fn delete(&self, user_id: &str, now_rfc3339: &str) -> Result<(), RepoError>;
}

/// Google Calendar persistence (`google_calendars` rows).
///
/// All deletes are SOFT: `deleted_at` is stamped, rows are never removed.
#[async_trait(?Send)]
pub trait CalendarRepo: Send + Sync {
    /// The user's calendars, primary first then by summary.
    async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<GoogleCalendar>, RepoError>;
    /// Every sync-enabled, non-deleted calendar across all users — the
    /// fallback cron's work list (ADR 0001 § Fallback cron).
    async fn list_sync_enabled(&self) -> Result<Vec<GoogleCalendar>, RepoError>;
    /// Returns the calendar with local `id`, or `None` when absent/soft-deleted.
    async fn get_by_id(&self, id: &str) -> Result<Option<GoogleCalendar>, RepoError>;
    /// Returns the calendar with `google_calendar_id`, or `None`.
    async fn get_by_google_cal_id(
        &self,
        user_id: &str,
        google_cal_id: &str,
    ) -> Result<Option<GoogleCalendar>, RepoError>;
    async fn upsert(&self, calendar: NewCalendar) -> Result<(), RepoError>;
    async fn upsert_batch(&self, calendars: Vec<NewCalendar>) -> Result<(), RepoError>;
    /// Stores the incremental sync cursor and the sync timestamp.
    async fn update_sync_state(
        &self,
        id: &str,
        sync_token: &str,
        last_synced_at_rfc3339: &str,
    ) -> Result<(), RepoError>;
    async fn set_sync_enabled(
        &self,
        id: &str,
        enabled: bool,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;
    /// SOFT delete: stamps `deleted_at = now_rfc3339`.
    async fn delete(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError>;
}

/// Cached Google Calendar event persistence (`calendar_events` rows).
///
/// All deletes are SOFT: `deleted_at` is stamped, rows are never removed
/// (fixing the old Go D1 implementation, which hard-deleted).
#[async_trait(?Send)]
pub trait CalendarEventRepo: Send + Sync {
    /// Inserts or updates one event and returns the generated `id`.
    async fn upsert(&self, event: NewCalendarEvent, now_rfc3339: &str) -> Result<String, RepoError>;
    /// Inserts or updates many events, chunked to respect D1's 100-bound-
    /// parameter limit (see `EVENT_UPSERT_CHUNK_SIZE`).
    async fn upsert_batch(
        &self,
        events: Vec<NewCalendarEvent>,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;
    async fn get_by_id(&self, id: &str) -> Result<Option<CalendarEvent>, RepoError>;
    /// Returns the *living* cached event by `(calendar_id, google_event_id)` —
    /// the exit path (`stop_running_event`) resolves the event a `started` log
    /// points at, then reads its `start_time` before PATCHing the end.
    async fn get_by_calendar_and_google_id(
        &self,
        calendar_id: &str,
        google_event_id: &str,
    ) -> Result<Option<CalendarEvent>, RepoError>;
    /// Events that *overlap* the half-open `[start, end)` window:
    /// `start_time < end AND end_time > start` (multi-day events are not
    /// clipped at window edges).
    async fn list_by_user_id_and_time_range(
        &self,
        user_id: &str,
        start_rfc3339: &str,
        end_rfc3339: &str,
    ) -> Result<Vec<CalendarEvent>, RepoError>;
    /// The user's *living* timed events (task-tagged, joined to their
    /// calendars) with `task_id` set AND `start_time <= now < end_time` —
    /// the derived "running" set (RFC 3339 UTC strings of this shape compare
    /// lexicographically). At most one such event per user is expected.
    async fn list_running_by_user_id(
        &self,
        user_id: &str,
        now_rfc3339: &str,
    ) -> Result<Vec<CalendarEvent>, RepoError>;
    /// SOFT delete by local id.
    async fn delete(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError>;
    /// SOFT delete by `(calendar_id, google_event_id)` — used when incremental
    /// sync reports a cancelled event.
    async fn delete_by_google_event_id(
        &self,
        calendar_id: &str,
        google_event_id: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;
    /// SOFT delete of rows whose `last_synced_at` is older than
    /// `older_than_rfc3339` (stale-event cleanup).
    async fn delete_stale(
        &self,
        calendar_id: &str,
        older_than_rfc3339: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;
}

/// Task list persistence (`task_lists` rows).
///
/// All deletes are SOFT: `deleted_at` is stamped, rows are never removed.
/// Deletion is guarded in the service: a list with living ROOT categories
/// (`count_root_categories_for_list > 0`) cannot be deleted (409).
#[async_trait(?Send)]
pub trait TaskListRepo: Send + Sync {
    /// The user's living lists, ordered by `sort_order` then `name`.
    async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<TaskList>, RepoError>;
    /// Returns the list with local `id`, or `None` when absent or soft-deleted.
    /// NOT user-scoped; callers must verify `row.user_id` (the service does).
    async fn get_by_id(&self, id: &str) -> Result<Option<TaskList>, RepoError>;
    /// Inserts a new list and returns the stored row. The D1 implementation
    /// generates the UUID `id` and the `created_at`/`updated_at` timestamps.
    async fn insert(&self, list: NewTaskList) -> Result<TaskList, RepoError>;
    /// Updates `name`/`color`/`sort_order` on a living list (`None` fields are
    /// left unchanged) and returns the updated row, or `None` when the list is
    /// missing or soft-deleted.
    async fn update(&self, id: &str, updates: &UpdateTaskList) -> Result<Option<TaskList>, RepoError>;
    /// SOFT delete: stamps `deleted_at = now_rfc3339`.
    async fn soft_delete(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError>;
    /// Number of living lists for the user (first-visit seed check).
    async fn count_by_user_id(&self, user_id: &str) -> Result<i64, RepoError>;
    /// Number of living ROOT categories (`parent_id IS NULL`) still
    /// referencing the list — the delete guard (409 when non-zero).
    async fn count_root_categories_for_list(&self, list_id: &str) -> Result<i64, RepoError>;
}

/// Task category persistence (`task_categories` + `task_category_patterns`
/// rows).
///
/// Categories form a one-level forest: a root (`parent_id` NULL) owns a
/// `list_id`; children hang off roots with `list_id NULL` (inherited on read).
/// `untracked` is the only root allowed `list_id NULL` and can never be
/// deleted. All category deletes are SOFT; pattern deletes are HARD (`PATCH`
/// replaces the whole set, so stale rows must actually disappear).
#[async_trait(?Send)]
pub trait TaskCategoryRepo: Send + Sync {
    /// The user's living categories, ordered by `sort_order` then `title`.
    async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<TaskCategory>, RepoError>;
    /// Returns the category with local `id`, or `None` when absent or
    /// soft-deleted. NOT user-scoped; callers must verify `row.user_id`.
    async fn get_by_id(&self, id: &str) -> Result<Option<TaskCategory>, RepoError>;
    /// Inserts a new category and returns the stored row. The D1
    /// implementation generates the UUID `id` and the timestamps.
    async fn insert(&self, category: NewTaskCategory) -> Result<TaskCategory, RepoError>;
    /// Updates the mutable columns on a living category (`None` fields are
    /// left unchanged, including `parent_id`/`list_id` — a `COALESCE` update
    /// can never clear them) and returns the updated row, or `None` when the
    /// category is missing or soft-deleted.
    async fn update(
        &self,
        id: &str,
        updates: &UpdateTaskCategory,
    ) -> Result<Option<TaskCategory>, RepoError>;
    /// SOFT delete: stamps `deleted_at = now_rfc3339`.
    async fn soft_delete(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError>;
    /// Number of living categories for the user (first-visit seed check).
    async fn count_by_user_id(&self, user_id: &str) -> Result<i64, RepoError>;
    /// Number of living children — the delete/reparent guard (409 when
    /// non-zero).
    async fn count_children(&self, category_id: &str) -> Result<i64, RepoError>;
    /// The user's `untracked` sink category (a root with `list_id NULL`), or
    /// `None` before the first seed.
    async fn get_untracked(&self, user_id: &str) -> Result<Option<TaskCategory>, RepoError>;
    /// The category's patterns in `sort_order` order.
    async fn list_patterns_by_category_id(
        &self,
        category_id: &str,
    ) -> Result<Vec<TaskCategoryPattern>, RepoError>;
    /// Every living category's patterns for the user, ordered by category then
    /// `sort_order`. Categories with no patterns are absent from the result.
    async fn list_patterns_by_user_id(
        &self,
        user_id: &str,
    ) -> Result<Vec<TaskCategoryPattern>, RepoError>;
    /// HARD-deletes the existing patterns and inserts `patterns` in order
    /// (each gets `sort_order` = its Vec index). The D1 implementation
    /// generates UUID ids and timestamps.
    async fn replace_patterns(
        &self,
        category_id: &str,
        patterns: Vec<NewTaskCategoryPattern>,
    ) -> Result<(), RepoError>;
    /// HARD-deletes every pattern row for the category (used on category
    /// delete).
    async fn delete_patterns_by_category_id(&self, category_id: &str) -> Result<(), RepoError>;
}

/// Task persistence (`tasks` rows).
///
/// Tasks carry NO `category_id`/`list_id` column: the category is computed per
/// title by the service (`classify`), so the repo stays deliberately thin.
/// All deletes are SOFT: `deleted_at` is stamped, rows are never removed.
#[async_trait(?Send)]
pub trait TaskRepo: Send + Sync {
    /// The user's living tasks, grouped per status and ranked inside it by
    /// `sort_order` then `created_at` (per-status board order; the frontend
    /// regroups by computed category anyway).
    async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<Task>, RepoError>;
    /// Every living `IN_PROGRESS` task across ALL users — the elongate cron's
    /// work list (`tasks.status` is the one-running lock, slice 1, so a
    /// stale/expired event cache neither adds nor drops work here).
    async fn list_in_progress(&self) -> Result<Vec<Task>, RepoError>;
    /// Returns the task with local `id`, or `None` when absent or soft-deleted.
    /// NOT user-scoped; callers must verify `row.user_id` (the service does).
    async fn get_by_id(&self, id: &str) -> Result<Option<Task>, RepoError>;
    /// Inserts a new task and returns the stored row. The D1 implementation
    /// generates the UUID `id`, stamps `created_at`/`updated_at`, and forces
    /// `status = "OPEN"` (this slice creates no other status).
    async fn insert(&self, task: NewTask) -> Result<Task, RepoError>;
    /// Shifts the living tasks of `user_id` in `status` whose `sort_order` is
    /// `>= from_inclusive` up by one — the peer shift behind the move
    /// endpoint's cross-column placement. Never touches `updated_at`:
    /// re-ranking is not a content change.
    async fn shift_sort_order(
        &self,
        user_id: &str,
        status: &str,
        from_inclusive: i64,
    ) -> Result<(), RepoError>;
    /// Signed peer shift over the closed `[from_inclusive, to_inclusive]`
    /// range (`delta` +1 up, -1 down) — the move endpoint's same-column
    /// neighbor reorder (the moving task itself never falls inside its own
    /// shift range). Never touches `updated_at`.
    async fn shift_sort_order_by(
        &self,
        user_id: &str,
        status: &str,
        from_inclusive: i64,
        to_inclusive: i64,
        delta: i64,
    ) -> Result<(), RepoError>;
    /// Updates `title`/`description`/`duration_minutes`/`priority`/
    /// `difficulty` on a living task (`None` fields are left unchanged; status
    /// is never touched here) and returns the updated row, or `None` when the
    /// task is missing or soft-deleted.
    async fn update(&self, id: &str, updates: &UpdateTask) -> Result<Option<Task>, RepoError>;
    /// Transitions `status` on a living task (slice 4 timer: start/stop/pause/
    /// complete/discard). Returns the updated row, or `None` when the task is
    /// missing or soft-deleted. Deliberately NOT part of `UpdateTask` — the
    /// public PATCH /api/tasks/:id never touches status.
    async fn set_status(
        &self,
        id: &str,
        status: &str,
        now_rfc3339: &str,
    ) -> Result<Option<Task>, RepoError>;
    /// Sets the task's board rank on a living task and returns the updated
    /// row, or `None` when missing/soft-deleted. Deliberately touches NEITHER
    /// `status` NOR `updated_at`: the move endpoint places cards (cross-column
    /// via `set_status` first, then this) without marking them content-updated.
    async fn set_sort_order(&self, id: &str, sort_order: i64) -> Result<Option<Task>, RepoError>;
    /// Highest living `sort_order` for `user_id` in `status`, or `None` when
    /// that pile is empty. Soft-deleted rows, other users/statuses, and the
    /// `exclude_id` row (when `Some`) are ignored — the no-drop `/move`
    /// passes the moving task id so its leftover rank from the source column
    /// never inflates its own append target. Used by `create_task` (`None`)
    /// and the no-drop `/move` to append: `max.map(|m| m + 1).unwrap_or(0)`.
    async fn max_sort_order(
        &self,
        user_id: &str,
        status: &str,
        exclude_id: Option<&str>,
    ) -> Result<Option<i64>, RepoError>;
    /// SOFT delete: stamps `deleted_at = now_rfc3339`.
    async fn soft_delete(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError>;
}

/// Task audit-trail persistence (`task_logs` rows).
///
/// Append-only by design — nothing ever updates or deletes a row. The only
/// read is [`TaskLogRepo::latest_started_by_task_id`]: closing a run PATCHes
/// the Google event the task's most recent `started` row points at, so the
/// timer never depends on the event window for identity.
#[async_trait(?Send)]
pub trait TaskLogRepo: Send + Sync {
    /// Inserts one log row. The D1 implementation generates the UUID `id` and
    /// the `created_at` timestamp; `at` (the transition instant) comes from
    /// the service. Returns the generated `id`.
    async fn insert(&self, log: NewTaskLog, now_rfc3339: &str) -> Result<String, RepoError>;
    /// The task's most recent `started` row (ties broken by insertion time),
    /// or `None` when the task never started. Its `calendar_id`/
    /// `google_event_id` name the event the exit verbs PATCH.
    async fn latest_started_by_task_id(&self, task_id: &str) -> Result<Option<TaskLog>, RepoError>;
}

/// Google Calendar watch channel persistence (`google_calendars_watch_channels`
/// rows).
///
/// Unlike every other table, deletes are HARD: rows are physically removed,
/// never soft-deleted. This table is a subscription, not a domain entity (see
/// ADR 0001).
#[async_trait(?Send)]
pub trait WatchChannelRepo: Send + Sync {
    /// Inserts a new watch channel and returns the generated `id`.
    /// `now_rfc3339` is stamped into `created_at`/`updated_at`.
    async fn insert(
        &self,
        channel: NewWatchChannel,
        now_rfc3339: &str,
    ) -> Result<String, RepoError>;
    /// Returns the channel with `channel_id` (the UUID we minted and that
    /// Google echoes back as `X-Goog-Channel-ID`), or `None`.
    async fn get_by_channel_id(&self, channel_id: &str) -> Result<Option<WatchChannel>, RepoError>;
    /// All channels for `calendar_id`. Many rows per calendar are expected:
    /// renewal overlaps two channels briefly (ADR 0001).
    async fn list_by_calendar_id(&self, calendar_id: &str) -> Result<Vec<WatchChannel>, RepoError>;
    /// Channels for `calendar_id` whose `expiration` is still in the future.
    /// RFC 3339 UTC strings compare lexicographically, so `expiration > ?`
    /// is correct.
    async fn list_unexpired_by_calendar_id(
        &self,
        calendar_id: &str,
        now_rfc3339: &str,
    ) -> Result<Vec<WatchChannel>, RepoError>;
    /// HARD delete by local row id.
    async fn delete_by_id(&self, id: &str) -> Result<(), RepoError>;
    /// HARD delete of every channel row for `calendar_id` (used when a
    /// calendar is disabled or soft-deleted — see ADR 0001).
    async fn delete_by_calendar_id(&self, calendar_id: &str) -> Result<(), RepoError>;
}

pub const USER_GET_BY_ID_SQL: &str = "SELECT * FROM users WHERE id = ? AND deleted_at IS NULL";

pub const USER_GET_BY_GOOGLE_ID_SQL: &str =
    "SELECT * FROM users WHERE google_id = ? AND deleted_at IS NULL";

/// Upsert keyed on `google_id`; `deleted_at = NULL` on conflict resurrects a
/// soft-deleted row so the user can log in again.
pub const USER_UPSERT_SQL: &str = "
    INSERT INTO users (id, google_id, email, name, picture, created_at, updated_at)
    VALUES (?, ?, ?, ?, ?, ?, ?)
    ON CONFLICT(google_id) DO UPDATE SET
        email = excluded.email,
        name = excluded.name,
        picture = excluded.picture,
        updated_at = excluded.updated_at,
        deleted_at = NULL
    RETURNING *
";

/// Fallback for D1 deployments where `RETURNING` misbehaves: update the row
/// found by `get_by_google_id` instead. Mirrors the old Go d1_repo.go fallback.
pub const USER_UPDATE_BY_ID_SQL: &str =
    "UPDATE users SET email = ?, name = ?, picture = ?, updated_at = ? WHERE id = ?";

pub const TOKEN_GET_BY_USER_ID_SQL: &str =
    "SELECT * FROM google_oauth_tokens WHERE user_id = ? AND deleted_at IS NULL";

/// Upsert keyed on `user_id`; `refresh_token` is preserved when the incoming
/// value is NULL or an empty string, and `deleted_at = NULL` on conflict
/// resurrects a soft-deleted row.
pub const TOKEN_UPSERT_SQL: &str = "
    INSERT INTO google_oauth_tokens
        (id, user_id, access_token, refresh_token, expiry, token_type, scope, created_at, updated_at)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
    ON CONFLICT(user_id) DO UPDATE SET
        access_token = excluded.access_token,
        refresh_token = COALESCE(NULLIF(excluded.refresh_token, ''), google_oauth_tokens.refresh_token),
        expiry = excluded.expiry,
        scope = excluded.scope,
        token_type = excluded.token_type,
        updated_at = excluded.updated_at,
        deleted_at = NULL
";

/// SOFT delete: the row keeps its UNIQUE `user_id` slot but becomes invisible
/// to `deleted_at IS NULL` reads.
pub const TOKEN_DELETE_SQL: &str =
    "UPDATE google_oauth_tokens SET deleted_at = ?, updated_at = ? WHERE user_id = ? AND deleted_at IS NULL";

// ──────────────────────────────────────────
// Calendar SQL
// ──────────────────────────────────────────

pub const CALENDAR_LIST_BY_USER_ID_SQL: &str =
    "SELECT * FROM google_calendars WHERE user_id = ? AND deleted_at IS NULL ORDER BY is_primary DESC, summary ASC";

/// The fallback cron's work list: every sync-enabled calendar that is not
/// soft-deleted, ordered by user, then primary first, then summary.
pub const CALENDAR_LIST_SYNC_ENABLED_SQL: &str =
    "SELECT * FROM google_calendars WHERE sync_enabled = 1 AND deleted_at IS NULL ORDER BY user_id ASC, is_primary DESC, summary ASC";

pub const CALENDAR_GET_BY_ID_SQL: &str =
    "SELECT * FROM google_calendars WHERE id = ? AND deleted_at IS NULL";

pub const CALENDAR_GET_BY_GOOGLE_CAL_ID_SQL: &str =
    "SELECT * FROM google_calendars WHERE user_id = ? AND google_calendar_id = ? AND deleted_at IS NULL";

/// Upsert keyed on `(user_id, google_calendar_id)`. An empty incoming
/// `sync_token`/`last_synced_at` preserves the stored value (`COALESCE`), and
/// `deleted_at = NULL` on conflict resurrects a soft-deleted row so a
/// re-import of the calendar list brings it back.
pub const CALENDAR_UPSERT_SQL: &str = "
    INSERT INTO google_calendars
        (id, user_id, google_calendar_id, summary, time_zone, is_primary, access_role, sync_enabled, sync_token, last_synced_at, created_at, updated_at)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    ON CONFLICT(user_id, google_calendar_id) DO UPDATE SET
        summary = excluded.summary,
        time_zone = excluded.time_zone,
        is_primary = excluded.is_primary,
        access_role = excluded.access_role,
        sync_enabled = excluded.sync_enabled,
        sync_token = COALESCE(NULLIF(excluded.sync_token, ''), google_calendars.sync_token),
        last_synced_at = COALESCE(NULLIF(excluded.last_synced_at, ''), google_calendars.last_synced_at),
        updated_at = excluded.updated_at,
        deleted_at = NULL
";

pub const CALENDAR_UPDATE_SYNC_STATE_SQL: &str =
    "UPDATE google_calendars SET sync_token = ?, last_synced_at = ?, updated_at = ? WHERE id = ? AND deleted_at IS NULL";

pub const CALENDAR_SET_SYNC_ENABLED_SQL: &str =
    "UPDATE google_calendars SET sync_enabled = ?, updated_at = ? WHERE id = ? AND deleted_at IS NULL";

/// SOFT delete: stamps `deleted_at`, keeping the row's UNIQUE
/// `(user_id, google_calendar_id)` slot.
pub const CALENDAR_DELETE_SQL: &str =
    "UPDATE google_calendars SET deleted_at = ?, updated_at = ? WHERE id = ? AND deleted_at IS NULL";

// ──────────────────────────────────────────
// Calendar event SQL
// ──────────────────────────────────────────

pub const EVENT_GET_BY_ID_SQL: &str =
    "SELECT * FROM calendar_events WHERE id = ? AND deleted_at IS NULL";

/// The exit path's event lookup: the living cached row a `started` log points
/// at (its `start_time` decides the PATCH end / displace start, on the minute
/// grid).
pub const EVENT_GET_BY_CALENDAR_AND_GOOGLE_ID_SQL: &str =
    "SELECT * FROM calendar_events WHERE calendar_id = ? AND google_event_id = ? AND deleted_at IS NULL";

/// Overlap semantics: an event intersects `[start, end)` when it begins before
/// the window ends AND ends after it begins — multi-day and overnight events
/// are not clipped at window edges.
pub const EVENT_LIST_BY_USER_ID_AND_TIME_RANGE_SQL: &str = "
    SELECT e.* FROM calendar_events e
    JOIN google_calendars c ON c.id = e.calendar_id
    WHERE c.user_id = ? AND e.deleted_at IS NULL AND e.start_time < ? AND e.end_time > ?
    ORDER BY e.start_time ASC
";

/// The derived "running" set: task-tagged events joined to the user's (living)
/// calendars where `task_id` is set AND `start_time <= now < end_time`.
/// SQLite evaluates `NULL != ''` to NULL (falsy), so the NULL guard before the
/// empty-string test is required, not cosmetic. RFC 3339 UTC strings of this
/// shape (`…Z`, zero-padded, no fractions) compare lexicographically, so the
/// range test needs no timestamp function.
pub const EVENT_LIST_RUNNING_BY_USER_ID_SQL: &str = "
    SELECT e.* FROM calendar_events e
    JOIN google_calendars c ON c.id = e.calendar_id
    WHERE c.user_id = ?
      AND e.deleted_at IS NULL
      AND e.task_id IS NOT NULL AND e.task_id != ''
      AND e.start_time <= ? AND e.end_time > ?
    ORDER BY e.start_time ASC
";

/// SOFT delete by local id.
pub const EVENT_DELETE_SQL: &str =
    "UPDATE calendar_events SET deleted_at = ?, updated_at = ? WHERE id = ? AND deleted_at IS NULL";

/// SOFT delete by `(calendar_id, google_event_id)`.
pub const EVENT_DELETE_BY_GOOGLE_EVENT_ID_SQL: &str =
    "UPDATE calendar_events SET deleted_at = ?, updated_at = ? WHERE calendar_id = ? AND google_event_id = ? AND deleted_at IS NULL";

/// SOFT delete of stale rows (older than a cutoff).
pub const EVENT_DELETE_STALE_SQL: &str =
    "UPDATE calendar_events SET deleted_at = ?, updated_at = ? WHERE calendar_id = ? AND last_synced_at < ? AND deleted_at IS NULL";

/// D1 allows at most 100 bound parameters per SQL statement.
/// `calendar_events` upsert binds 14 columns per row → max 7 rows per statement.
pub const EVENT_UPSERT_COL_COUNT: usize = 14;
pub const EVENT_UPSERT_CHUNK_SIZE: usize = 100 / EVENT_UPSERT_COL_COUNT; // 7

const EVENT_UPSERT_ON_CONFLICT: &str = "
    ON CONFLICT(calendar_id, google_event_id) DO UPDATE SET
        google_etag = excluded.google_etag,
        google_updated_at = excluded.google_updated_at,
        last_synced_at = excluded.last_synced_at,
        title = excluded.title,
        description = excluded.description,
        start_time = excluded.start_time,
        end_time = excluded.end_time,
        recurrence = excluded.recurrence,
        task_id = COALESCE(NULLIF(excluded.task_id, ''), calendar_events.task_id),
        updated_at = excluded.updated_at
";

/// Builds a multi-row `INSERT … ON CONFLICT` statement for one chunk of
/// events (non-empty and ≤ `EVENT_UPSERT_CHUNK_SIZE`). `ids` supplies the new
/// UUID for each row and must match `events.len()` — the D1 implementation
/// generates them (api-core stays free of a UUID dependency).
///
/// Returns `(sql, args)` where every arg is a string; the D1 implementation
/// binds them as `D1Type::Text`. Mirrors the old Go `buildEventUpsertSQL`
/// (14 columns; COALESCE-free apart from the `task_id` guard — a Google event
/// without the `sanctuary_task_id` property must not wipe a stored link).
pub fn build_event_upsert_sql(
    events: &[NewCalendarEvent],
    now_rfc3339: &str,
    ids: Vec<String>,
) -> (String, Vec<String>) {
    assert!(!events.is_empty(), "event upsert chunk must not be empty");
    assert!(
        events.len() <= EVENT_UPSERT_CHUNK_SIZE,
        "event upsert chunk exceeds {EVENT_UPSERT_CHUNK_SIZE} rows"
    );
    assert_eq!(events.len(), ids.len(), "one id per event required");

    let mut sql = String::from(
        "INSERT INTO calendar_events
        (id, calendar_id, google_event_id, google_etag, google_updated_at, last_synced_at, title, description, start_time, end_time, recurrence, task_id, created_at, updated_at)
        VALUES ",
    );
    let mut args: Vec<String> = Vec::with_capacity(events.len() * EVENT_UPSERT_COL_COUNT);
    for (index, (event, id)) in events.iter().zip(ids).enumerate() {
        if index > 0 {
            sql.push(',');
        }
        sql.push_str("(?,?,?,?,?,?,?,?,?,?,?,?,?,?)");
        args.extend([
            id,
            event.calendar_id.clone(),
            event.google_event_id.clone(),
            event.google_etag.clone(),
            event.google_updated_at.clone(),
            event.last_synced_at.clone(),
            event.title.clone(),
            event.description.clone(),
            event.start_time.clone(),
            event.end_time.clone(),
            event.recurrence.clone(),
            event.task_id.clone(),
            now_rfc3339.to_string(),
            now_rfc3339.to_string(),
        ]);
    }
    sql.push(' ');
    sql.push_str(EVENT_UPSERT_ON_CONFLICT);
    (sql, args)
}

// ──────────────────────────────────────────
// Task list SQL
// ──────────────────────────────────────────

pub const TASK_LIST_LIST_BY_USER_ID_SQL: &str =
    "SELECT * FROM task_lists WHERE user_id = ? AND deleted_at IS NULL ORDER BY sort_order ASC, name ASC";

pub const TASK_LIST_GET_BY_ID_SQL: &str =
    "SELECT * FROM task_lists WHERE id = ? AND deleted_at IS NULL";

/// Plain INSERT (no `ON CONFLICT`): lists are user-authored, never upserted
/// (the seed inserts fresh rows per new user). The D1 implementation binds the
/// UUID `id` and the `created_at`/`updated_at` timestamps.
pub const TASK_LIST_INSERT_SQL: &str = "
    INSERT INTO task_lists (id, user_id, name, color, sort_order, created_at, updated_at)
    VALUES (?, ?, ?, ?, ?, ?, ?)
";

/// Partial update: NULL binds leave the column unchanged (`COALESCE`). The D1
/// implementation binds `D1Type::Null` for `None` fields; the service validates
/// non-empty name/color before this runs, so empty strings never reach the DB.
pub const TASK_LIST_UPDATE_SQL: &str = "
    UPDATE task_lists SET
        name = COALESCE(?, name),
        color = COALESCE(?, color),
        sort_order = COALESCE(?, sort_order),
        updated_at = ?
    WHERE id = ? AND deleted_at IS NULL
";

/// SOFT delete: stamps `deleted_at`, keeping the row's data for audit.
pub const TASK_LIST_DELETE_SQL: &str =
    "UPDATE task_lists SET deleted_at = ?, updated_at = ? WHERE id = ? AND deleted_at IS NULL";

pub const TASK_LIST_COUNT_BY_USER_ID_SQL: &str =
    "SELECT COUNT(*) AS count FROM task_lists WHERE user_id = ? AND deleted_at IS NULL";

/// The delete guard: living ROOT categories (`parent_id IS NULL`) still
/// referencing the list. Children store `list_id NULL` (they inherit on read),
/// so only roots matter here.
pub const TASK_LIST_COUNT_ROOT_CATEGORIES_SQL: &str = "
    SELECT COUNT(*) AS count FROM task_categories
    WHERE list_id = ? AND parent_id IS NULL AND deleted_at IS NULL
";

// ──────────────────────────────────────────
// Task category SQL
// ──────────────────────────────────────────

pub const TASK_CATEGORY_LIST_BY_USER_ID_SQL: &str =
    "SELECT * FROM task_categories WHERE user_id = ? AND deleted_at IS NULL ORDER BY sort_order ASC, title ASC";

pub const TASK_CATEGORY_GET_BY_ID_SQL: &str =
    "SELECT * FROM task_categories WHERE id = ? AND deleted_at IS NULL";

/// Plain INSERT (no `ON CONFLICT`): categories are user-authored, never
/// upserted (the seed inserts fresh rows per new user; the unique
/// `(user_id, slug)` partial index guards duplicate slugs). The D1
/// implementation binds the UUID `id`, the timestamps, and INTEGER 0/1 bools
/// for `is_productive`/`is_untracked`.
pub const TASK_CATEGORY_INSERT_SQL: &str = "
    INSERT INTO task_categories
        (id, user_id, list_id, parent_id, title, slug, color, is_productive, google_calendar_id, google_color_id, sort_order, is_untracked, created_at, updated_at)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
";

/// Partial update: NULL binds leave the column unchanged (`COALESCE`). A
/// category can never be *cleared* of `parent_id` or `list_id` through this
/// statement (no UI for promotion/detachment today).
///
/// One exception — moving a root under a parent must NULL its `list_id`:
/// children are stored with `list_id NULL` (they inherit on read), so the
/// update sets `list_id = NULL` whenever a new `parent_id` is bound and only
/// `COALESCE`s otherwise. The D1 implementation binds `parent_id` twice.
pub const TASK_CATEGORY_UPDATE_SQL: &str = "
    UPDATE task_categories SET
        title = COALESCE(?, title),
        slug = COALESCE(?, slug),
        color = COALESCE(?, color),
        is_productive = COALESCE(?, is_productive),
        google_calendar_id = COALESCE(?, google_calendar_id),
        google_color_id = COALESCE(?, google_color_id),
        sort_order = COALESCE(?, sort_order),
        parent_id = COALESCE(?, parent_id),
        list_id = CASE WHEN ? IS NOT NULL THEN NULL ELSE COALESCE(?, list_id) END,
        updated_at = ?
    WHERE id = ? AND deleted_at IS NULL
";

/// SOFT delete: stamps `deleted_at`, keeping the row's data for audit.
/// Patterns are hard-deleted first (see `TASK_CATEGORY_PATTERNS_DELETE_SQL`).
pub const TASK_CATEGORY_DELETE_SQL: &str =
    "UPDATE task_categories SET deleted_at = ?, updated_at = ? WHERE id = ? AND deleted_at IS NULL";

pub const TASK_CATEGORY_COUNT_BY_USER_ID_SQL: &str =
    "SELECT COUNT(*) AS count FROM task_categories WHERE user_id = ? AND deleted_at IS NULL";

/// The delete/reparent guard: living children are still hanging off this
/// category, so it cannot be deleted (or reparented) until they are gone.
pub const TASK_CATEGORY_COUNT_CHILDREN_SQL: &str =
    "SELECT COUNT(*) AS count FROM task_categories WHERE parent_id = ? AND deleted_at IS NULL";

/// The `untracked` sink: a root with `list_id NULL` and `is_untracked = 1`,
/// seeded on first visit; there is at most one per user (`slug` unique).
pub const TASK_CATEGORY_GET_UNTRACKED_SQL: &str = "
    SELECT * FROM task_categories
    WHERE user_id = ? AND is_untracked = 1 AND deleted_at IS NULL
    ORDER BY sort_order ASC
    LIMIT 1
";

pub const TASK_CATEGORY_PATTERNS_LIST_SQL: &str =
    "SELECT * FROM task_category_patterns WHERE category_id = ? ORDER BY sort_order ASC";

/// Every living category's patterns for the user in one statement: the
/// patterns table has no `user_id` column, so the JOIN onto `task_categories`
/// carries the user scope and the soft-delete filter (categories with no
/// patterns are simply absent from the result).
pub const TASK_CATEGORY_PATTERNS_LIST_BY_USER_ID_SQL: &str = "
    SELECT p.*
    FROM task_category_patterns p
    INNER JOIN task_categories c ON c.id = p.category_id
    WHERE c.user_id = ? AND c.deleted_at IS NULL
    ORDER BY p.category_id, p.sort_order ASC
";

/// HARD delete: replace_patterns wipes then re-inserts, so stale rows must
/// actually disappear (same rule as watch channels — but note the FKs here
/// are plain, so a hard delete is also required on category delete).
pub const TASK_CATEGORY_PATTERNS_DELETE_SQL: &str =
    "DELETE FROM task_category_patterns WHERE category_id = ?";

/// Plain INSERT: `replace_patterns` deletes first, and the D1 implementation
/// binds the UUID `id`, the timestamps, and `sort_order` = Vec index.
pub const TASK_CATEGORY_PATTERNS_INSERT_SQL: &str = "
    INSERT INTO task_category_patterns (id, category_id, regex, google_calendar_id, sort_order, created_at, updated_at)
    VALUES (?, ?, ?, ?, ?, ?, ?)
";

// ──────────────────────────────────────────
// Task SQL
// ──────────────────────────────────────────

/// Living tasks for the user, grouped per status and ranked inside it by
/// `sort_order` (ties by creation time). The `sort_order` segment carries the
/// per-status board rank; `status ASC` keeps each rank contiguous.
pub const TASK_LIST_BY_USER_ID_SQL: &str =
    "SELECT * FROM tasks WHERE user_id = ? AND deleted_at IS NULL ORDER BY status ASC, sort_order ASC, created_at ASC";

/// The elongate cron's work list: every living IN_PROGRESS task, all users
/// (the status is the one-running lock — soft-deleted rows are filtered).
pub const TASK_LIST_IN_PROGRESS_SQL: &str =
    "SELECT * FROM tasks WHERE status = 'IN_PROGRESS' AND deleted_at IS NULL";

pub const TASK_GET_BY_ID_SQL: &str =
    "SELECT * FROM tasks WHERE id = ? AND deleted_at IS NULL";

/// Plain INSERT (no `ON CONFLICT`): tasks are user-authored, never upserted.
/// The D1 implementation binds the UUID `id`, the timestamps, the hardcoded
/// `status = 'OPEN'`, and the append `sort_order` — `create_task` queries
/// `max(sort_order)+1` (0 on an empty Backlog) and never shifts peers.
pub const TASK_INSERT_SQL: &str = "
    INSERT INTO tasks (id, user_id, title, description, duration_minutes, priority, difficulty, status, sort_order, created_at, updated_at)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
";

/// The cross-column peer shift: every living task of the user in `status`
/// ranked at or after `from_inclusive` moves up one. `updated_at` is left
/// alone on purpose — re-ranking is a position change, not a content update.
pub const TASK_SHIFT_SORT_ORDER_SQL: &str = "
    UPDATE tasks
    SET sort_order = sort_order + 1
    WHERE user_id = ? AND status = ? AND deleted_at IS NULL AND sort_order >= ?
";

/// Signed peer shift over `[from_inclusive, to_inclusive]` (`delta` +1 up,
/// -1 down) — the move endpoint's same-column neighbor reorder. The bounds
/// keep the moving card out of its own shift: front drags shift `[new, old)`
/// up, back drags shift `(old, new]` down. `updated_at` is left alone, same
/// as `TASK_SHIFT_SORT_ORDER_SQL`.
pub const TASK_SHIFT_SORT_ORDER_RANGE_SQL: &str = "
    UPDATE tasks
    SET sort_order = sort_order + ?
    WHERE user_id = ? AND status = ? AND deleted_at IS NULL AND sort_order >= ? AND sort_order <= ?
";

/// Sets the task's board rank on a living row. No `status`, no `updated_at`:
/// placement is a position change, not a content update.
pub const TASK_SET_SORT_ORDER_SQL: &str =
    "UPDATE tasks SET sort_order = ? WHERE id = ? AND deleted_at IS NULL";

/// The highest living `sort_order` in the user's `status` pile — the append
/// target behind `create_task` and the no-drop `/move` (which binds its own
/// id to the `id != ?` filter so the mover's leftover rank never inflates
/// its own append target; `create_task` binds an empty string, which never
/// matches a UUID). `LIMIT 1` over `MAX()` so an empty pile reads as `None`
/// via `first()`, matching the other get-by-id queries.
pub const TASK_MAX_SORT_ORDER_SQL: &str = "
    SELECT sort_order FROM tasks
    WHERE user_id = ? AND status = ? AND deleted_at IS NULL AND id != ?
    ORDER BY sort_order DESC
    LIMIT 1
";

/// Partial update: NULL binds leave the column unchanged (`COALESCE`). Status
/// is intentionally not updatable — slice 4 adds the status transitions. The
/// D1 implementation binds `D1Type::Null` for `None` fields; the service
/// validates values before this runs, so invalid priorities/empty titles never
/// reach the DB.
pub const TASK_UPDATE_SQL: &str = "
    UPDATE tasks SET
        title = COALESCE(?, title),
        description = COALESCE(?, description),
        duration_minutes = COALESCE(?, duration_minutes),
        priority = COALESCE(?, priority),
        difficulty = COALESCE(?, difficulty),
        updated_at = ?
    WHERE id = ? AND deleted_at IS NULL
";

/// SOFT delete: stamps `deleted_at`, keeping the row's data for audit.
pub const TASK_DELETE_SQL: &str =
    "UPDATE tasks SET deleted_at = ?, updated_at = ? WHERE id = ? AND deleted_at IS NULL";

/// Timer transitions only (slice 4): `status` is deliberately not part of the
/// public `TASK_UPDATE_SQL`; this statement is the repo's internal status
/// channel, called by the service's start/stop/pause/complete/discard.
pub const TASK_SET_STATUS_SQL: &str =
    "UPDATE tasks SET status = ?, updated_at = ? WHERE id = ? AND deleted_at IS NULL";

/// Append-only audit insert: the D1 implementation binds the UUID `id`, the
/// timestamps, and NULLs for absent `calendar_id`/`google_event_id`.
pub const TASK_LOG_INSERT_SQL: &str = "
    INSERT INTO task_logs (id, task_id, user_id, type, at, calendar_id, google_event_id, created_at)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?)
";

/// The exit verbs' event identity: the task's most recent `started` row (the
/// `type` column is TEXT, so the literal needs no quotes gymnastics; ties by
/// `created_at` keep the D1 insertion order).
pub const TASK_LOG_LATEST_STARTED_BY_TASK_ID_SQL: &str = "
    SELECT * FROM task_logs
    WHERE task_id = ? AND type = 'started'
    ORDER BY at DESC, created_at DESC
    LIMIT 1
";

// ──────────────────────────────────────────
// Watch channel SQL
// ──────────────────────────────────────────

/// Plain INSERT, not an upsert: `channel_id` is UNIQUE and renewal mints a new
/// row rather than replacing an old one — overlap of two rows per calendar is
/// expected (ADR 0001). The D1 implementation supplies `id` (UUIDv4) and
/// `created_at`/`updated_at` from the passed `now_rfc3339`.
pub const WATCH_CHANNEL_INSERT_SQL: &str = "
    INSERT INTO google_calendars_watch_channels
        (id, calendar_id, channel_id, resource_id, token, expiration, created_at, updated_at)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?)
";

pub const WATCH_CHANNEL_GET_BY_CHANNEL_ID_SQL: &str =
    "SELECT * FROM google_calendars_watch_channels WHERE channel_id = ?";

pub const WATCH_CHANNEL_LIST_BY_CALENDAR_ID_SQL: &str =
    "SELECT * FROM google_calendars_watch_channels WHERE calendar_id = ? ORDER BY created_at ASC";

/// RFC 3339 UTC strings compare correctly as text, so `expiration > ?` finds
/// channels that are still valid.
pub const WATCH_CHANNEL_LIST_UNEXPIRED_BY_CALENDAR_ID_SQL: &str = "
    SELECT * FROM google_calendars_watch_channels
    WHERE calendar_id = ? AND expiration > ?
    ORDER BY created_at ASC
";

/// HARD delete: this table has no `deleted_at` (ADR 0001), so rows are
/// physically removed.
pub const WATCH_CHANNEL_DELETE_BY_ID_SQL: &str =
    "DELETE FROM google_calendars_watch_channels WHERE id = ?";

/// HARD delete of every row for a calendar. `channels.stop` runs per row
/// before this; overlap rows are removed together (ADR 0001).
pub const WATCH_CHANNEL_DELETE_BY_CALENDAR_ID_SQL: &str =
    "DELETE FROM google_calendars_watch_channels WHERE calendar_id = ?";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_reads_filter_soft_deleted_rows() {
        assert!(USER_GET_BY_ID_SQL.contains("deleted_at IS NULL"), "{USER_GET_BY_ID_SQL}");
        assert!(USER_GET_BY_GOOGLE_ID_SQL.contains("deleted_at IS NULL"));
        assert!(TOKEN_GET_BY_USER_ID_SQL.contains("deleted_at IS NULL"));
        assert!(CALENDAR_LIST_BY_USER_ID_SQL.contains("deleted_at IS NULL"));
        assert!(CALENDAR_LIST_SYNC_ENABLED_SQL.contains("deleted_at IS NULL"));
        assert!(CALENDAR_GET_BY_ID_SQL.contains("deleted_at IS NULL"));
        assert!(CALENDAR_GET_BY_GOOGLE_CAL_ID_SQL.contains("deleted_at IS NULL"));
        assert!(EVENT_GET_BY_ID_SQL.contains("deleted_at IS NULL"));
        assert!(EVENT_LIST_BY_USER_ID_AND_TIME_RANGE_SQL.contains("deleted_at IS NULL"));
    }

    #[test]
    fn token_delete_is_soft_not_hard() {
        assert!(TOKEN_DELETE_SQL.starts_with("UPDATE"), "{TOKEN_DELETE_SQL}");
        assert!(TOKEN_DELETE_SQL.contains("SET deleted_at = ?"), "{TOKEN_DELETE_SQL}");
        assert!(!TOKEN_DELETE_SQL.contains("DELETE FROM"), "{TOKEN_DELETE_SQL}");
    }

    #[test]
    fn calendar_and_event_deletes_are_soft_not_hard() {
        for sql in [
            CALENDAR_DELETE_SQL,
            EVENT_DELETE_SQL,
            EVENT_DELETE_BY_GOOGLE_EVENT_ID_SQL,
            EVENT_DELETE_STALE_SQL,
        ] {
            assert!(sql.starts_with("UPDATE"), "{sql}");
            assert!(sql.contains("SET deleted_at = ?"), "{sql}");
            assert!(!sql.contains("DELETE FROM"), "{sql}");
        }
    }

    #[test]
    fn token_upsert_preserves_existing_refresh_token() {
        assert!(
            TOKEN_UPSERT_SQL.contains(
                "COALESCE(NULLIF(excluded.refresh_token, ''), google_oauth_tokens.refresh_token)"
            ),
            "{TOKEN_UPSERT_SQL}"
        );
    }

    #[test]
    fn upserts_resurrect_soft_deleted_rows() {
        assert!(USER_UPSERT_SQL.contains("deleted_at = NULL"), "{USER_UPSERT_SQL}");
        assert!(TOKEN_UPSERT_SQL.contains("deleted_at = NULL"), "{TOKEN_UPSERT_SQL}");
        assert!(CALENDAR_UPSERT_SQL.contains("deleted_at = NULL"), "{CALENDAR_UPSERT_SQL}");
    }

    #[test]
    fn calendar_upsert_preserves_sync_token_and_last_synced_at() {
        assert!(
            CALENDAR_UPSERT_SQL.contains(
                "sync_token = COALESCE(NULLIF(excluded.sync_token, ''), google_calendars.sync_token)"
            ),
            "{CALENDAR_UPSERT_SQL}"
        );
        assert!(
            CALENDAR_UPSERT_SQL.contains(
                "last_synced_at = COALESCE(NULLIF(excluded.last_synced_at, ''), google_calendars.last_synced_at)"
            ),
            "{CALENDAR_UPSERT_SQL}"
        );
    }

    #[test]
    fn calendar_list_orders_primary_first_then_summary() {
        let sql = CALENDAR_LIST_BY_USER_ID_SQL;
        let order_start = sql.find("ORDER BY").expect("has ORDER BY");
        assert_eq!(
            &sql[order_start..],
            "ORDER BY is_primary DESC, summary ASC"
        );
    }

    #[test]
    fn calendar_list_sync_enabled_filters_and_orders() {
        // The fallback cron's work list: only sync-enabled, non-deleted rows,
        // ordered by user then primary first then summary.
        let sql = CALENDAR_LIST_SYNC_ENABLED_SQL;
        assert!(sql.contains("sync_enabled = 1"), "{sql}");
        assert!(sql.contains("deleted_at IS NULL"), "{sql}");
        let order_start = sql.find("ORDER BY").expect("has ORDER BY");
        assert_eq!(
            &sql[order_start..],
            "ORDER BY user_id ASC, is_primary DESC, summary ASC"
        );
    }

    #[test]
    fn event_range_query_uses_overlap_semantics() {
        let sql = EVENT_LIST_BY_USER_ID_AND_TIME_RANGE_SQL;
        assert!(sql.contains("e.start_time < ?"), "{sql}");
        assert!(sql.contains("e.end_time > ?"), "{sql}");
        assert!(sql.contains("c.user_id = ?"), "{sql}");
        assert!(sql.contains("ORDER BY e.start_time ASC"), "{sql}");
    }

    #[test]
    fn event_upsert_chunk_size_respects_d1_100_param_limit() {
        assert_eq!(EVENT_UPSERT_COL_COUNT, 14);
        assert_eq!(EVENT_UPSERT_CHUNK_SIZE, 7);
        assert!(EVENT_UPSERT_CHUNK_SIZE * EVENT_UPSERT_COL_COUNT <= 100);
    }

    #[test]
    fn event_upsert_sql_has_14_placeholders_per_row_and_on_conflict() {
        let event = NewCalendarEvent {
            calendar_id: "cal-1".to_string(),
            google_event_id: "g-1".to_string(),
            google_etag: "etag".to_string(),
            google_updated_at: "2026-08-17T10:00:00Z".to_string(),
            last_synced_at: "2026-08-17T12:00:00Z".to_string(),
            title: "Standup".to_string(),
            description: String::new(),
            start_time: "2026-08-18T09:00:00Z".to_string(),
            end_time: "2026-08-18T09:30:00Z".to_string(),
            recurrence: String::new(),
            task_id: "task-1".to_string(),
        };
        let (sql, args) = build_event_upsert_sql(
            &[event.clone()],
            "2026-08-17T12:00:00Z",
            vec!["evt-1".to_string()],
        );

        assert!(sql.starts_with("INSERT INTO calendar_events"), "{sql}");
        assert!(sql.contains("ON CONFLICT(calendar_id, google_event_id)"), "{sql}");
        assert!(sql.contains("google_etag = excluded.google_etag"), "{sql}");
        assert!(sql.contains("updated_at = excluded.updated_at"), "{sql}");

        assert_eq!(args.len(), 14);
        assert_eq!(args[0], "evt-1");
        assert_eq!(args[1], "cal-1");
        assert_eq!(args[5], "2026-08-17T12:00:00Z", "last_synced_at bound");
        assert_eq!(args[11], "task-1", "task_id bound");
        assert_eq!(args[12], "2026-08-17T12:00:00Z", "created_at bound");
        assert_eq!(args[13], "2026-08-17T12:00:00Z", "updated_at bound");

        // Exactly 14 placeholders for the single row (no trailing/extra commas).
        assert_eq!(sql.matches("(?,?,?,?,?,?,?,?,?,?,?,?,?,?)").count(), 1);
        assert!(!sql.contains("(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?"), "no 15th placeholder");
    }

    #[test]
    fn event_upsert_sql_chunks_7_rows_with_98_placeholders() {
        let event = NewCalendarEvent {
            calendar_id: "cal-1".to_string(),
            google_event_id: "g".to_string(),
            google_etag: String::new(),
            google_updated_at: String::new(),
            last_synced_at: "2026-08-17T12:00:00Z".to_string(),
            title: "T".to_string(),
            description: String::new(),
            start_time: "2026-08-18T09:00:00Z".to_string(),
            end_time: "2026-08-18T09:30:00Z".to_string(),
            recurrence: String::new(),
            task_id: "task-1".to_string(),
        };
        let events: Vec<NewCalendarEvent> = (0..7).map(|_| event.clone()).collect();
        let ids: Vec<String> = (0..7).map(|i| format!("evt-{i}")).collect();
        let (sql, args) = build_event_upsert_sql(&events, "2026-08-17T12:00:00Z", ids);

        assert_eq!(args.len(), 7 * 14);
        assert_eq!(sql.matches('?').count(), 7 * 14);
        assert_eq!(sql.matches("(?,?,?,?,?,?,?,?,?,?,?,?,?,?)").count(), 7);
        assert_eq!(args[0], "evt-0");
        assert_eq!(args[14], "evt-1");
        assert_eq!(args[14 * 6], "evt-6");
    }

    #[test]
    fn event_upsert_preserves_existing_task_id_when_incoming_is_empty() {
        // A sync of an untagged Google event must never wipe a stored task
        // link: the COALESCE keeps the existing value.
        assert!(
            EVENT_UPSERT_ON_CONFLICT.contains(
                "task_id = COALESCE(NULLIF(excluded.task_id, ''), calendar_events.task_id)"
            ),
            "{EVENT_UPSERT_ON_CONFLICT}"
        );
    }

    #[test]
    fn task_list_reads_filter_soft_deleted_rows_and_order_by_sort_then_name() {
        assert!(TASK_LIST_LIST_BY_USER_ID_SQL.contains("deleted_at IS NULL"), "{}", TASK_LIST_LIST_BY_USER_ID_SQL);
        let order_start = TASK_LIST_LIST_BY_USER_ID_SQL
            .find("ORDER BY")
            .expect("has ORDER BY");
        assert_eq!(
            &TASK_LIST_LIST_BY_USER_ID_SQL[order_start..],
            "ORDER BY sort_order ASC, name ASC"
        );
        assert!(TASK_LIST_GET_BY_ID_SQL.contains("deleted_at IS NULL"), "{}", TASK_LIST_GET_BY_ID_SQL);
        assert!(TASK_LIST_COUNT_BY_USER_ID_SQL.contains("deleted_at IS NULL"), "{}", TASK_LIST_COUNT_BY_USER_ID_SQL);
    }

    #[test]
    fn task_list_insert_binds_all_columns() {
        assert!(TASK_LIST_INSERT_SQL.contains("INSERT INTO task_lists"), "{}", TASK_LIST_INSERT_SQL);
        assert!(!TASK_LIST_INSERT_SQL.contains("ON CONFLICT"), "{}", TASK_LIST_INSERT_SQL);
        assert_eq!(
            TASK_LIST_INSERT_SQL.matches('?').count(),
            7,
            "one placeholder per column: {}",
            TASK_LIST_INSERT_SQL
        );
    }

    #[test]
    fn task_list_update_uses_coalesce_for_partial_updates() {
        let sql = TASK_LIST_UPDATE_SQL;
        assert!(sql.trim_start().starts_with("UPDATE"), "{sql}");
        assert!(sql.contains("COALESCE(?, name)"), "{sql}");
        assert!(sql.contains("COALESCE(?, color)"), "{sql}");
        assert!(sql.contains("COALESCE(?, sort_order)"), "{sql}");
        assert!(sql.contains("WHERE id = ? AND deleted_at IS NULL"), "{sql}");
    }

    #[test]
    fn task_list_delete_is_soft_not_hard() {
        assert!(TASK_LIST_DELETE_SQL.starts_with("UPDATE"), "{}", TASK_LIST_DELETE_SQL);
        assert!(TASK_LIST_DELETE_SQL.contains("SET deleted_at = ?"), "{}", TASK_LIST_DELETE_SQL);
        assert!(!TASK_LIST_DELETE_SQL.contains("DELETE FROM"), "{}", TASK_LIST_DELETE_SQL);
    }

    #[test]
    fn task_list_root_category_guard_filters_living_roots() {
        let sql = TASK_LIST_COUNT_ROOT_CATEGORIES_SQL;
        assert!(sql.contains("list_id = ?"), "{sql}");
        assert!(sql.contains("parent_id IS NULL"), "{sql}");
        assert!(sql.contains("deleted_at IS NULL"), "{sql}");
    }

    #[test]
    fn task_category_reads_filter_soft_deleted_rows_and_order_by_sort_then_title() {
        assert!(TASK_CATEGORY_LIST_BY_USER_ID_SQL.contains("deleted_at IS NULL"), "{}", TASK_CATEGORY_LIST_BY_USER_ID_SQL);
        let order_start = TASK_CATEGORY_LIST_BY_USER_ID_SQL
            .find("ORDER BY")
            .expect("has ORDER BY");
        assert_eq!(
            &TASK_CATEGORY_LIST_BY_USER_ID_SQL[order_start..],
            "ORDER BY sort_order ASC, title ASC"
        );
        assert!(TASK_CATEGORY_GET_BY_ID_SQL.contains("deleted_at IS NULL"), "{}", TASK_CATEGORY_GET_BY_ID_SQL);
        assert!(TASK_CATEGORY_COUNT_BY_USER_ID_SQL.contains("deleted_at IS NULL"), "{}", TASK_CATEGORY_COUNT_BY_USER_ID_SQL);
        assert!(TASK_CATEGORY_GET_UNTRACKED_SQL.contains("deleted_at IS NULL"), "{}", TASK_CATEGORY_GET_UNTRACKED_SQL);
    }

    #[test]
    fn task_category_insert_binds_all_14_columns() {
        assert!(TASK_CATEGORY_INSERT_SQL.contains("INSERT INTO task_categories"), "{}", TASK_CATEGORY_INSERT_SQL);
        assert!(!TASK_CATEGORY_INSERT_SQL.contains("ON CONFLICT"), "{}", TASK_CATEGORY_INSERT_SQL);
        assert_eq!(
            TASK_CATEGORY_INSERT_SQL.matches('?').count(),
            14,
            "one placeholder per column: {}",
            TASK_CATEGORY_INSERT_SQL
        );
        for column in [
            "id", "user_id", "list_id", "parent_id", "title", "slug", "color",
            "is_productive", "google_calendar_id", "google_color_id", "sort_order",
            "is_untracked", "created_at", "updated_at",
        ] {
            assert!(TASK_CATEGORY_INSERT_SQL.contains(column), "missing {column}");
        }
    }

    #[test]
    fn task_category_update_uses_coalesce_and_nulls_list_id_on_reparent() {
        let sql = TASK_CATEGORY_UPDATE_SQL;
        assert!(sql.trim_start().starts_with("UPDATE"), "{sql}");
        assert!(sql.contains("COALESCE(?, title)"), "{sql}");
        assert!(sql.contains("COALESCE(?, parent_id)"), "{sql}");
        assert!(sql.contains("COALESCE(?, list_id)"), "{sql}");
        assert!(sql.contains("CASE WHEN ? IS NOT NULL THEN NULL"), "{sql}");
        assert!(sql.contains("WHERE id = ? AND deleted_at IS NULL"), "{sql}");
        // 11 mutable/now bindings + the id: nothing else.
        assert_eq!(sql.matches('?').count(), 12, "{sql}");
    }

    #[test]
    fn task_category_delete_is_soft_not_hard() {
        assert!(TASK_CATEGORY_DELETE_SQL.starts_with("UPDATE"), "{}", TASK_CATEGORY_DELETE_SQL);
        assert!(TASK_CATEGORY_DELETE_SQL.contains("SET deleted_at = ?"), "{}", TASK_CATEGORY_DELETE_SQL);
        assert!(!TASK_CATEGORY_DELETE_SQL.contains("DELETE FROM"), "{}", TASK_CATEGORY_DELETE_SQL);
    }

    #[test]
    fn task_category_children_guard_filters_living_children() {
        let sql = TASK_CATEGORY_COUNT_CHILDREN_SQL;
        assert!(sql.contains("parent_id = ?"), "{sql}");
        assert!(sql.contains("deleted_at IS NULL"), "{sql}");
    }

    #[test]
    fn task_category_untracked_query_scopes_user_and_flag() {
        let sql = TASK_CATEGORY_GET_UNTRACKED_SQL;
        assert!(sql.contains("user_id = ?"), "{sql}");
        assert!(sql.contains("is_untracked = 1"), "{sql}");
        assert!(sql.contains("deleted_at IS NULL"), "{sql}");
        assert!(sql.contains("LIMIT 1"), "{sql}");
    }

    #[test]
    fn task_category_patterns_are_hard_deleted_and_replaced_in_order() {
        assert!(TASK_CATEGORY_PATTERNS_DELETE_SQL.starts_with("DELETE FROM"), "{}", TASK_CATEGORY_PATTERNS_DELETE_SQL);
        assert!(TASK_CATEGORY_PATTERNS_DELETE_SQL.contains("WHERE category_id = ?"), "{}", TASK_CATEGORY_PATTERNS_DELETE_SQL);
        let insert = TASK_CATEGORY_PATTERNS_INSERT_SQL;
        assert!(insert.contains("INSERT INTO task_category_patterns"), "{insert}");
        assert_eq!(insert.matches('?').count(), 7, "{insert}");
        assert!(TASK_CATEGORY_PATTERNS_LIST_SQL.contains("ORDER BY sort_order ASC"), "{}", TASK_CATEGORY_PATTERNS_LIST_SQL);
    }

    #[test]
    fn task_category_patterns_by_user_join_living_categories() {
        // The bulk list has no user_id column of its own: the JOIN onto
        // task_categories carries the user scope and the soft-delete filter.
        let sql = TASK_CATEGORY_PATTERNS_LIST_BY_USER_ID_SQL;
        assert!(sql.contains("INNER JOIN task_categories c ON c.id = p.category_id"), "{sql}");
        assert!(sql.contains("c.user_id = ?"), "{sql}");
        assert!(sql.contains("c.deleted_at IS NULL"), "{sql}");
        assert!(sql.contains("ORDER BY p.category_id, p.sort_order ASC"), "{sql}");
        assert!(!sql.contains("p.user_id"), "patterns table has no user_id: {sql}");
    }

    #[test]
    fn watch_channel_insert_is_insert_not_upsert_and_lists_adr_columns() {
        let sql = WATCH_CHANNEL_INSERT_SQL;
        assert!(sql.contains("INSERT INTO google_calendars_watch_channels"), "{sql}");
        assert!(!sql.contains("ON CONFLICT"), "{sql}");
        // Columns from the ADR DDL: every NOT NULL business column plus the
        // D1-generated `id` and timestamps.
        for column in [
            "id",
            "calendar_id",
            "channel_id",
            "resource_id",
            "token",
            "expiration",
            "created_at",
            "updated_at",
        ] {
            assert!(sql.contains(column), "missing {column} in {sql}");
        }
        assert_eq!(sql.matches('?').count(), 8, "one placeholder per column: {sql}");
        assert!(!sql.contains("deleted_at"), "{sql}");
    }

    #[test]
    fn watch_channel_deletes_are_hard_not_soft() {
        for sql in [WATCH_CHANNEL_DELETE_BY_ID_SQL, WATCH_CHANNEL_DELETE_BY_CALENDAR_ID_SQL] {
            assert!(sql.starts_with("DELETE FROM"), "{sql}");
            assert!(!sql.contains("UPDATE"), "{sql}");
            assert!(!sql.contains("deleted_at"), "{sql}");
        }
        assert!(WATCH_CHANNEL_DELETE_BY_ID_SQL.contains("WHERE id = ?"), "{}", WATCH_CHANNEL_DELETE_BY_ID_SQL);
        assert!(
            WATCH_CHANNEL_DELETE_BY_CALENDAR_ID_SQL.contains("WHERE calendar_id = ?"),
            "{}",
            WATCH_CHANNEL_DELETE_BY_CALENDAR_ID_SQL
        );
    }

    #[test]
    fn watch_channel_reads_have_no_deleted_at_filter() {
        // Hard-delete table: reads must not reference `deleted_at`.
        for sql in [
            WATCH_CHANNEL_GET_BY_CHANNEL_ID_SQL,
            WATCH_CHANNEL_LIST_BY_CALENDAR_ID_SQL,
            WATCH_CHANNEL_LIST_UNEXPIRED_BY_CALENDAR_ID_SQL,
        ] {
            assert!(!sql.contains("deleted_at"), "{sql}");
        }
        assert!(WATCH_CHANNEL_GET_BY_CHANNEL_ID_SQL.contains("WHERE channel_id = ?"), "{}", WATCH_CHANNEL_GET_BY_CHANNEL_ID_SQL);
        assert!(WATCH_CHANNEL_LIST_BY_CALENDAR_ID_SQL.contains("WHERE calendar_id = ?"), "{}", WATCH_CHANNEL_LIST_BY_CALENDAR_ID_SQL);
    }

    #[test]
    fn watch_channel_unexpired_query_filters_on_expiration() {
        let sql = WATCH_CHANNEL_LIST_UNEXPIRED_BY_CALENDAR_ID_SQL;
        assert!(sql.contains("expiration > ?"), "{sql}");
        assert!(sql.contains("calendar_id = ?"), "{sql}");
    }

    #[test]
    fn task_reads_filter_soft_deleted_rows_and_order_by_status_sort_then_created() {
        assert!(TASK_LIST_BY_USER_ID_SQL.contains("deleted_at IS NULL"), "{}", TASK_LIST_BY_USER_ID_SQL);
        let order_start = TASK_LIST_BY_USER_ID_SQL
            .find("ORDER BY")
            .expect("has ORDER BY");
        assert_eq!(
            &TASK_LIST_BY_USER_ID_SQL[order_start..],
            "ORDER BY status ASC, sort_order ASC, created_at ASC"
        );
        assert!(TASK_GET_BY_ID_SQL.contains("deleted_at IS NULL"), "{}", TASK_GET_BY_ID_SQL);
    }

    #[test]
    fn task_list_in_progress_filters_status_and_living_rows() {
        // The elongate cron's work list: IN_PROGRESS status is the lock
        // (`status` TEXT compares to the quoted literal), soft-deleted rows
        // are filtered, and there is deliberately no user filter — all users.
        let sql = TASK_LIST_IN_PROGRESS_SQL;
        assert!(sql.starts_with("SELECT * FROM tasks"), "{sql}");
        assert!(sql.contains("status = 'IN_PROGRESS'"), "{sql}");
        assert!(sql.contains("deleted_at IS NULL"), "{sql}");
        assert!(!sql.contains("user_id"), "all users, not one: {sql}");
    }

    #[test]
    fn task_insert_binds_all_11_columns_with_status_open() {
        assert!(TASK_INSERT_SQL.contains("INSERT INTO tasks"), "{}", TASK_INSERT_SQL);
        assert!(!TASK_INSERT_SQL.contains("ON CONFLICT"), "{}", TASK_INSERT_SQL);
        assert_eq!(
            TASK_INSERT_SQL.matches('?').count(),
            11,
            "one placeholder per column: {}",
            TASK_INSERT_SQL
        );
        // The status column exists and is not user-bound — the D1 impl binds
        // the literal 'OPEN'.
        assert!(TASK_INSERT_SQL.contains("status"), "{TASK_INSERT_SQL}");
        for column in [
            "id", "user_id", "title", "description", "duration_minutes",
            "priority", "difficulty", "status", "sort_order", "created_at",
            "updated_at",
        ] {
            assert!(TASK_INSERT_SQL.contains(column), "missing {column}");
        }
    }

    #[test]
    fn task_shift_sort_order_shifts_living_peers_without_touching_updated_at() {
        let sql = TASK_SHIFT_SORT_ORDER_SQL;
        assert!(sql.trim_start().starts_with("UPDATE"), "{sql}");
        assert!(sql.contains("sort_order = sort_order + 1"), "{sql}");
        assert!(sql.contains("user_id = ?"), "{sql}");
        assert!(sql.contains("status = ?"), "{sql}");
        assert!(sql.contains("sort_order >= ?"), "{sql}");
        assert!(sql.contains("deleted_at IS NULL"), "{sql}");
        assert!(!sql.contains("updated_at"), "peer shifts never bump updated_at: {sql}");
    }

    #[test]
    fn task_set_sort_order_touches_only_rank_on_living_rows() {
        let sql = TASK_SET_SORT_ORDER_SQL;
        assert!(sql.trim_start().starts_with("UPDATE"), "{sql}");
        assert!(sql.contains("SET sort_order = ?"), "{sql}");
        assert!(sql.contains("WHERE id = ? AND deleted_at IS NULL"), "{sql}");
        assert!(!sql.contains("status"), "placement never touches status: {sql}");
        assert!(!sql.contains("updated_at"), "placement never bumps updated_at: {sql}");
        assert_eq!(sql.matches('?').count(), 2, "{sql}");
    }

    #[test]
    fn task_shift_sort_order_range_binds_signed_delta_and_both_bounds() {
        let sql = TASK_SHIFT_SORT_ORDER_RANGE_SQL;
        assert!(sql.trim_start().starts_with("UPDATE"), "{sql}");
        assert!(sql.contains("sort_order = sort_order + ?"), "{sql}");
        assert!(sql.contains("sort_order >= ? AND sort_order <= ?"), "{sql}");
        assert!(sql.contains("user_id = ?"), "{sql}");
        assert!(sql.contains("status = ?"), "{sql}");
        assert!(sql.contains("deleted_at IS NULL"), "{sql}");
        assert!(!sql.contains("updated_at"), "peer shifts never bump updated_at: {sql}");
        assert_eq!(sql.matches('?').count(), 5, "{sql}");
    }

    #[test]
    fn task_update_uses_coalesce_and_never_touches_status() {
        let sql = TASK_UPDATE_SQL;
        assert!(sql.trim_start().starts_with("UPDATE"), "{sql}");
        assert!(sql.contains("COALESCE(?, title)"), "{sql}");
        assert!(sql.contains("COALESCE(?, description)"), "{sql}");
        assert!(sql.contains("COALESCE(?, duration_minutes)"), "{sql}");
        assert!(sql.contains("COALESCE(?, priority)"), "{sql}");
        assert!(sql.contains("COALESCE(?, difficulty)"), "{sql}");
        assert!(!sql.contains("status"), "status is not updatable this slice: {sql}");
        assert!(sql.contains("WHERE id = ? AND deleted_at IS NULL"), "{sql}");
        // 5 COALESCE binds + now + id: nothing else.
        assert_eq!(sql.matches('?').count(), 7, "{sql}");
    }

    #[test]
    fn task_delete_is_soft_not_hard() {
        assert!(TASK_DELETE_SQL.starts_with("UPDATE"), "{}", TASK_DELETE_SQL);
        assert!(TASK_DELETE_SQL.contains("SET deleted_at = ?"), "{}", TASK_DELETE_SQL);
        assert!(!TASK_DELETE_SQL.contains("DELETE FROM"), "{}", TASK_DELETE_SQL);
    }

    #[test]
    fn task_set_status_updates_only_status_on_living_rows() {
        let sql = TASK_SET_STATUS_SQL;
        assert!(sql.trim_start().starts_with("UPDATE"), "{sql}");
        assert!(sql.contains("SET status = ?"), "{sql}");
        assert!(sql.contains("updated_at = ?"), "{sql}");
        assert!(sql.contains("WHERE id = ? AND deleted_at IS NULL"), "{sql}");
        assert_eq!(sql.matches('?').count(), 3, "{sql}");
        assert!(!sql.contains("title"), "status transitions only: {sql}");
    }

    #[test]
    fn running_events_query_filters_task_tagged_living_events_in_now_window() {
        let sql = EVENT_LIST_RUNNING_BY_USER_ID_SQL;
        assert!(sql.contains("c.user_id = ?"), "{sql}");
        assert!(sql.contains("e.deleted_at IS NULL"), "{sql}");
        assert!(sql.contains("e.task_id IS NOT NULL AND e.task_id != ''"), "{sql}");
        assert!(sql.contains("e.start_time <= ? AND e.end_time > ?"), "{sql}");
        assert!(sql.contains("JOIN google_calendars c ON c.id = e.calendar_id"), "{sql}");
    }

    #[test]
    fn task_log_insert_binds_all_8_columns() {
        let sql = TASK_LOG_INSERT_SQL;
        assert!(sql.contains("INSERT INTO task_logs"), "{sql}");
        assert!(!sql.contains("ON CONFLICT"), "append-only: {sql}");
        for column in [
            "id",
            "task_id",
            "user_id",
            "type",
            "at",
            "calendar_id",
            "google_event_id",
            "created_at",
        ] {
            assert!(sql.contains(column), "missing {column} in {sql}");
        }
        assert_eq!(sql.matches('?').count(), 8, "{sql}");
    }

    #[test]
    fn task_log_latest_started_query_filters_type_and_takes_newest() {
        let sql = TASK_LOG_LATEST_STARTED_BY_TASK_ID_SQL;
        assert!(sql.contains("SELECT * FROM task_logs"), "{sql}");
        assert!(sql.contains("task_id = ?"), "{sql}");
        assert!(sql.contains("type = 'started'"), "only started rows: {sql}");
        assert!(
            sql.contains("ORDER BY at DESC, created_at DESC"),
            "{sql}"
        );
        assert!(sql.contains("LIMIT 1"), "{sql}");
    }

    #[test]
    fn event_get_by_calendar_and_google_id_filters_living_rows() {
        let sql = EVENT_GET_BY_CALENDAR_AND_GOOGLE_ID_SQL;
        assert!(sql.starts_with("SELECT * FROM calendar_events"), "{sql}");
        assert!(sql.contains("calendar_id = ?"), "{sql}");
        assert!(sql.contains("google_event_id = ?"), "{sql}");
        assert!(sql.contains("deleted_at IS NULL"), "{sql}");
    }
}
