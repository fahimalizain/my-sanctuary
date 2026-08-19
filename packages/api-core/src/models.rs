//! Database models.
//!
//! Timestamps are RFC 3339 UTC strings (`TEXT` columns in D1). Nullable
//! columns are `Option<T>`. The `User`/`GoogleOAuthToken` types double as D1
//! row projections: they derive `Deserialize` so `D1PreparedStatement::first`
//! can map rows straight onto them (field names match the schema).

use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;

/// Deserializes a D1 `INTEGER 0/1` column (a JS number) — or a plain JSON
/// boolean — into a `bool`. D1 does NOT store booleans as JSON `true`/`false`;
/// `is_primary`/`sync_enabled` come back as `0`/`1`, which serde would reject
/// for a `bool` field without this visitor.
pub fn de_d1_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    struct D1BoolVisitor;

    impl<'de> serde::de::Visitor<'de> for D1BoolVisitor {
        type Value = bool;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a boolean or an integer 0/1")
        }

        fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
            Ok(value)
        }
        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
            Ok(value != 0)
        }
        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
            Ok(value != 0)
        }
        fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E> {
            Ok(value != 0.0)
        }
    }

    deserializer.deserialize_any(D1BoolVisitor)
}

/// Deserializes a nullable TEXT column as a plain `String`, mapping D1 `NULL`
/// (a JS `null`) to `""`. Mirrors the old Go D1 scanner, which read NULL
/// strings as empty strings. Required because `serde`'s built-in `String`
/// deserializer rejects `null`, and `D1Result::results` unwraps row
/// deserialization (a NULL would panic the Worker).
pub fn de_empty_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    struct EmptyStringVisitor;

    impl<'de> serde::de::Visitor<'de> for EmptyStringVisitor {
        type Value = String;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a string or null")
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(String::new())
        }
        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
            Ok(value.to_string())
        }
        fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
            Ok(value)
        }
    }

    deserializer.deserialize_any(EmptyStringVisitor)
}

/// A user identity as stored in the `users` table.
///
/// Identity only — Google OAuth tokens live in [`GoogleOAuthToken`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct User {
    pub id: String,
    pub google_id: String,
    pub email: String,
    pub name: String,
    pub picture: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// Soft-delete marker; reads filter on `deleted_at IS NULL`.
    pub deleted_at: Option<String>,
}

/// Insert/update input for [`crate::repo::UserRepo::upsert_by_google_id`].
///
/// The D1 implementation generates the UUID `id` and the timestamps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewUser {
    pub google_id: String,
    pub email: String,
    pub name: String,
    pub picture: Option<String>,
}

/// A Google OAuth token as stored in the `google_oauth_tokens` table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GoogleOAuthToken {
    pub id: String,
    pub user_id: String,
    pub access_token: String,
    /// `None` when Google only issued an access token (e.g. a later login);
    /// the upsert keeps any previously stored refresh token in that case.
    pub refresh_token: Option<String>,
    /// RFC 3339 UTC instant when the access token expires.
    pub expiry: String,
    pub token_type: String,
    pub scope: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// Soft-delete marker; reads filter on `deleted_at IS NULL`.
    pub deleted_at: Option<String>,
}

/// Insert/update input for [`crate::repo::TokenRepo::upsert`].
///
/// The D1 implementation generates the UUID `id` and the timestamps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewToken {
    pub user_id: String,
    pub access_token: String,
    /// `None` (or empty) when Google omitted `refresh_token`; the SQL
    /// `COALESCE(NULLIF(...))` keeps the stored value in that case.
    pub refresh_token: Option<String>,
    /// RFC 3339 UTC instant when the access token expires.
    pub expiry: String,
    pub token_type: String,
    pub scope: Option<String>,
}

/// A calendar from the user's `/users/me/calendarList`, as stored in
/// `google_calendars`. Doubles as the D1 row projection: field names match the
/// schema, `is_primary`/`sync_enabled` deserialize from D1's `INTEGER 0/1`
/// via [`de_d1_bool`], and nullable TEXT columns map to `""` via
/// [`de_empty_string`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GoogleCalendar {
    pub id: String,
    pub user_id: String,
    pub google_calendar_id: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub summary: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub time_zone: String,
    /// D1 stores this as `INTEGER 0/1`.
    #[serde(default, deserialize_with = "de_d1_bool")]
    pub is_primary: bool,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub access_role: String,
    /// D1 stores this as `INTEGER 0/1`.
    #[serde(default, deserialize_with = "de_d1_bool")]
    pub sync_enabled: bool,
    /// Incremental sync cursor (Google `nextSyncToken`); empty when never synced.
    #[serde(default, deserialize_with = "de_empty_string")]
    pub sync_token: String,
    /// RFC 3339 instant of the last successful sync; `None` when never synced.
    pub last_synced_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// Soft-delete marker; reads filter on `deleted_at IS NULL`.
    pub deleted_at: Option<String>,
}

/// Insert/update input for [`crate::repo::CalendarRepo::upsert`] /
/// `upsert_batch`. The D1 implementation generates the UUID `id` and the
/// `created_at`/`updated_at` timestamps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCalendar {
    pub user_id: String,
    pub google_calendar_id: String,
    pub summary: String,
    pub time_zone: String,
    pub is_primary: bool,
    pub access_role: String,
    /// Defaults to `true` when importing from calendarList; the upsert's
    /// `COALESCE` keeps an existing `sync_enabled` when re-imported.
    pub sync_enabled: bool,
    /// May be empty; the upsert's `COALESCE` keeps any stored sync token.
    pub sync_token: String,
    /// May be `None`; the upsert's `COALESCE` keeps any stored value.
    pub last_synced_at: Option<String>,
}

/// A cached Google Calendar event, as stored in `calendar_events`.
///
/// Doubles as the D1 row projection AND the API response payload: serde field
/// names are already snake_case and match the frontend `CalendarEvent` in
/// `apps/web/app/types.ts` (`id, calendar_id, google_event_id, title,
/// description, start_time, end_time, last_synced_at` — extra columns are
/// included, which the frontend ignores).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarEvent {
    pub id: String,
    pub calendar_id: String,
    pub google_event_id: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub google_etag: String,
    /// Google's `updated` field (RFC 3339); empty when absent.
    #[serde(default, deserialize_with = "de_empty_string")]
    pub google_updated_at: String,
    /// RFC 3339 instant of the last successful sync of this row.
    pub last_synced_at: String,
    pub title: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub description: String,
    /// RFC 3339 instant.
    pub start_time: String,
    /// RFC 3339 instant.
    pub end_time: String,
    /// JSON array of RRULE strings; empty for non-recurring events.
    #[serde(default, deserialize_with = "de_empty_string")]
    pub recurrence: String,
    /// The task this event was created for (slice 4: start writes
    /// `extendedProperties.shared.sanctuary_task_id`, sync maps it back).
    /// Empty when the event never had one.
    #[serde(default, deserialize_with = "de_empty_string")]
    pub task_id: String,
    pub created_at: String,
    pub updated_at: String,
    /// Soft-delete marker; reads filter on `deleted_at IS NULL`.
    pub deleted_at: Option<String>,
}

/// Insert/update input for [`crate::repo::CalendarEventRepo::upsert`] /
/// `upsert_batch`. The D1 implementation generates the UUID `id` and the
/// `created_at`/`updated_at` timestamps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCalendarEvent {
    pub calendar_id: String,
    pub google_event_id: String,
    pub google_etag: String,
    /// Google's `updated` field (RFC 3339); empty when absent.
    pub google_updated_at: String,
    /// RFC 3339 instant of this sync.
    pub last_synced_at: String,
    pub title: String,
    pub description: String,
    /// RFC 3339 instant.
    pub start_time: String,
    /// RFC 3339 instant.
    pub end_time: String,
    /// JSON array of RRULE strings; empty for non-recurring events.
    pub recurrence: String,
    /// Task link (from `extendedProperties.shared.sanctuary_task_id`); empty
    /// for events that never had one. The upsert NEVER wipes a stored value
    /// with an empty incoming one (`COALESCE` in SQL).
    pub task_id: String,
}

/// A task list (the former "stream"), as stored in `task_lists`. Doubles as
/// the D1 row projection AND the API response payload: serde field names are
/// already snake_case and match the frontend `TaskList` in
/// `apps/web/app/types.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskList {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub color: String,
    pub sort_order: i64,
    /// RFC 3339 instant.
    pub created_at: String,
    /// RFC 3339 instant.
    pub updated_at: String,
    /// Soft-delete marker; reads filter on `deleted_at IS NULL`.
    pub deleted_at: Option<String>,
}

/// Insert input for [`crate::repo::TaskListRepo::insert`]. The D1
/// implementation generates the UUID `id` and the `created_at`/`updated_at`
/// timestamps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTaskList {
    pub user_id: String,
    pub name: String,
    pub color: String,
    pub sort_order: i64,
}

/// Update input for [`crate::repo::TaskListRepo::update`] (`PATCH
/// /api/lists/:id`). `None` fields are left unchanged; the service rejects a
/// body where every field is `None` (400 "nothing to update").
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
pub struct UpdateTaskList {
    pub name: Option<String>,
    pub color: Option<String>,
    pub sort_order: Option<i64>,
}

/// A category in the one-level task taxonomy, as stored in `task_categories`.
///
/// Doubles as the D1 row projection: field names match the schema,
/// `is_productive`/`is_untracked` deserialize from D1's `INTEGER 0/1` via
/// [`de_d1_bool`], and the nullable columns stay `Option<String>` (`NULL` in
/// D1). Not the HTTP response shape — [`crate::categories`] wraps rows into
/// `CategoryView` (plus patterns and the inherited list id).
///
/// Tree rules (enforced by the service): a category is either a root
/// (`parent_id` NULL) or a child of a root. `list_id` is meaningful only on
/// roots; children store NULL and inherit the parent's list on read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCategory {
    pub id: String,
    pub user_id: String,
    /// Root's owning list; NULL for children and for `untracked`.
    pub list_id: Option<String>,
    /// NULL for roots; otherwise the parent root's id.
    pub parent_id: Option<String>,
    pub title: String,
    /// Unique per user among living categories; `work`/`fitness`/`family`/
    /// `personal`/`untracked` for seeds.
    pub slug: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub color: String,
    /// D1 stores this as `INTEGER 0/1`.
    #[serde(default, deserialize_with = "de_d1_bool")]
    pub is_productive: bool,
    /// Optional Google Calendar id that anchors this category to a calendar.
    pub google_calendar_id: Option<String>,
    pub google_color_id: Option<String>,
    pub sort_order: i64,
    /// `1` only for the undeletable `untracked` sink category.
    #[serde(default, deserialize_with = "de_d1_bool")]
    pub is_untracked: bool,
    /// RFC 3339 instant.
    pub created_at: String,
    /// RFC 3339 instant.
    pub updated_at: String,
    /// Soft-delete marker; reads filter on `deleted_at IS NULL`.
    pub deleted_at: Option<String>,
}

/// Insert input for [`crate::repo::TaskCategoryRepo::insert`]. The D1
/// implementation generates the UUID `id` and the timestamps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTaskCategory {
    pub user_id: String,
    /// `Some` only for roots; children are stored with `None`.
    pub list_id: Option<String>,
    /// `Some` only for children (of a root).
    pub parent_id: Option<String>,
    pub title: String,
    pub slug: String,
    pub color: String,
    pub is_productive: bool,
    pub google_calendar_id: Option<String>,
    pub google_color_id: Option<String>,
    pub sort_order: i64,
    /// `true` only for `untracked` (seeded by the system, never via the API).
    pub is_untracked: bool,
}

/// Request body for `POST /api/categories`.
///
/// `is_untracked` is accepted so the service can reject it (400): the sink
/// category is seeded by the system and never user-creatable.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct NewTaskCategoryInput {
    pub title: String,
    /// Omitted (or empty) → derived from `title` via slugification.
    pub slug: Option<String>,
    pub color: String,
    pub is_productive: Option<bool>,
    pub google_calendar_id: Option<String>,
    pub google_color_id: Option<String>,
    /// Required for roots, forbidden for children (service 400).
    pub list_id: Option<String>,
    /// Required for children, forbidden for grandchildren (service 400).
    pub parent_id: Option<String>,
    pub sort_order: Option<i64>,
    #[serde(default)]
    pub is_untracked: Option<bool>,
    #[serde(default)]
    pub patterns: Vec<NewTaskCategoryPattern>,
}

/// Update input for [`crate::repo::TaskCategoryRepo::update`] (`PATCH
/// /api/categories/:id`). `None` fields are left unchanged (COALESCE in SQL);
/// the service rejects a body where every field is `None` (400).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
pub struct UpdateTaskCategory {
    pub title: Option<String>,
    pub slug: Option<String>,
    pub color: Option<String>,
    pub is_productive: Option<bool>,
    pub google_calendar_id: Option<String>,
    pub google_color_id: Option<String>,
    /// `Some` only for roots; a child given a non-null `list_id` is a 400.
    pub list_id: Option<String>,
    /// `Some` moves this category under a root; never settable to a child.
    pub parent_id: Option<String>,
    pub sort_order: Option<i64>,
    /// Accepted so the service can reject it (400).
    pub is_untracked: Option<bool>,
    /// `Some` replaces the category's whole pattern set; `None` leaves it.
    #[serde(default)]
    pub patterns: Option<Vec<NewTaskCategoryPattern>>,
}

/// A title-matching regex attached to a category, as stored in
/// `task_category_patterns`. Doubles as the D1 row projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCategoryPattern {
    pub id: String,
    pub category_id: String,
    /// The user-authored regex. Stored patterns are validated on write, but
    /// the matcher skips any that fail to compile on read regardless.
    pub regex: String,
    /// When set, the pattern only applies to events from this Google calendar;
    /// task titles (no calendar) skip it.
    pub google_calendar_id: Option<String>,
    pub sort_order: i64,
    /// RFC 3339 instant.
    pub created_at: String,
    /// RFC 3339 instant.
    pub updated_at: String,
}

/// Insert input for one pattern row. `sort_order` is derived from the Vec
/// index by the D1 implementation; ids/timestamps are generated there too.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct NewTaskCategoryPattern {
    pub regex: String,
    #[serde(default)]
    pub google_calendar_id: Option<String>,
}

/// A task, as stored in `tasks`.
///
/// Doubles as the D1 row projection AND part of the API response payload:
/// serde field names are snake_case and match the frontend `TaskRecord`
/// (`title, description, duration_minutes, priority, difficulty, status` plus
/// ids and timestamps). Classification lives on the **category** side only —
/// there is deliberately no `category_id`/`list_id` column; the service
/// computes the category per title via `classify` and attaches it to the task
/// view.
///
/// Create stamps `status = "OPEN"`. Transitions — `IN_PROGRESS`, back to
/// `OPEN`, `COMPLETED`, `DISCARDED` — happen exclusively through the timer
/// endpoints (start/stop/pause/complete/discard); the public `UpdateTask`
/// has no status field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub user_id: String,
    pub title: String,
    /// Nullable column mapped to `""` via [`de_empty_string`].
    #[serde(default, deserialize_with = "de_empty_string")]
    pub description: String,
    /// Planned duration in minutes; service enforces `>= 1` (default 15).
    pub duration_minutes: i64,
    /// `high|medium|low`; service enforces the enum (default `medium`).
    pub priority: String,
    /// `easy|medium|hard`; service enforces the enum (default `easy`).
    pub difficulty: String,
    /// `OPEN` for every task in this slice; slice 4 adds transitions.
    pub status: String,
    /// RFC 3339 instant.
    pub created_at: String,
    /// RFC 3339 instant.
    pub updated_at: String,
    /// Soft-delete marker; reads filter on `deleted_at IS NULL`.
    pub deleted_at: Option<String>,
}

/// Insert input for [`crate::repo::TaskRepo::insert`]. The D1 implementation
/// generates the UUID `id`, the `created_at`/`updated_at` timestamps, and
/// stamps `status = "OPEN"` (tasks are never created in any other state).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTask {
    pub user_id: String,
    pub title: String,
    /// Empty string when the caller omitted a description.
    pub description: String,
    pub duration_minutes: i64,
    pub priority: String,
    pub difficulty: String,
}

/// Request body for `POST /api/tasks`. `duration_minutes`/`priority`/
/// `difficulty` default server-side (15, `medium`, `easy`); `description`
/// defaults to `""`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct NewTaskInput {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub duration_minutes: Option<i64>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub difficulty: Option<String>,
}

/// One entry of the task audit trail, as stored in `task_logs`.
///
/// Doubles as the D1 row projection: field names match the schema (the column
/// is `type`, hence the raw identifier, which serde serializes as `"type"`).
/// Append-only by construction — nothing ever updates or deletes these rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLog {
    pub id: String,
    pub task_id: String,
    pub user_id: String,
    /// `started|stopped|paused|completed|discarded`.
    pub r#type: String,
    /// RFC 3339 instant of the transition.
    pub at: String,
    /// Local calendar id of the Google event the transition touched; `""`
    /// when no calendar was involved (e.g. `completed` without a running
    /// event). Nullable column mapped via [`de_empty_string`].
    #[serde(default, deserialize_with = "de_empty_string")]
    pub calendar_id: String,
    /// Google event id of the touched event; `""` when none.
    #[serde(default, deserialize_with = "de_empty_string")]
    pub google_event_id: String,
    pub created_at: String,
}

/// Insert input for [`crate::repo::TaskLogRepo::insert`]. The D1
/// implementation generates the UUID `id` and the `created_at` timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTaskLog {
    pub task_id: String,
    pub user_id: String,
    /// `started|stopped|paused|completed|discarded`.
    pub r#type: String,
    /// RFC 3339 instant of the transition.
    pub at: String,
    /// Local calendar id of the touched event (when any).
    pub calendar_id: Option<String>,
    /// Google event id of the touched event (when any).
    pub google_event_id: Option<String>,
}

/// Update input for [`crate::repo::TaskRepo::update`] (`PATCH /api/tasks/:id`).
/// `None` fields are left unchanged (COALESCE in SQL); the service rejects a
/// body where every field is `None` (400 "nothing to update"). Status is
/// deliberately absent — transitions go through the timer endpoints only
/// (`TaskRepo::set_status`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
pub struct UpdateTask {
    pub title: Option<String>,
    pub description: Option<String>,
    pub duration_minutes: Option<i64>,
    pub priority: Option<String>,
    pub difficulty: Option<String>,
}

/// A Google Calendar watch channel (`events.watch` subscription), as stored in
/// `google_calendars_watch_channels`. Doubles as the D1 row projection: field
/// names match the schema exactly. All columns are NOT NULL TEXT, and — unlike
/// every other table — there is **no** `deleted_at`: channels are hard-deleted
/// on stop (see ADR 0001).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WatchChannel {
    pub id: String,
    /// Owning calendar (`google_calendars.id`); many rows per calendar, since
    /// renewal briefly overlaps two channels.
    pub calendar_id: String,
    /// The UUID we mint; the webhook lookup key (`X-Goog-Channel-ID`). UNIQUE.
    pub channel_id: String,
    /// Google's resource id; required to call `channels.stop`.
    pub resource_id: String,
    /// Secret we mint; compared to `X-Goog-Channel-Token`.
    pub token: String,
    /// RFC 3339 UTC instant when the channel expires.
    pub expiration: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Insert input for [`crate::repo::WatchChannelRepo::insert`]. The D1
/// implementation generates the UUID `id` and the `created_at`/`updated_at`
/// timestamps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewWatchChannel {
    pub calendar_id: String,
    pub channel_id: String,
    pub resource_id: String,
    pub token: String,
    /// RFC 3339 UTC instant when the channel expires.
    pub expiration: String,
}

/// Request body for `POST /api/calendar/events`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct NewEventInput {
    /// Local DB calendar id (`google_calendars.id`), not the Google id.
    pub calendar_id: String,
    pub summary: String,
    #[serde(default)]
    pub description: Option<String>,
    /// dateTime string (RFC 3339) passed through to Google.
    pub start: String,
    /// dateTime string (RFC 3339) passed through to Google.
    pub end: String,
    /// When set, the created event carries
    /// `extendedProperties.shared.sanctuary_task_id` (the task timer's
    /// carrier — slice 4). `None` for hand-created events.
    #[serde(default)]
    pub task_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn d1_integer_bools_deserialize_to_bool() {
        // D1 returns 0/1 as JS numbers; serde_json numbers exercise the same
        // visitor path (visit_i64).
        let calendar: GoogleCalendar = serde_json::from_str(
            r#"{
                "id": "cal-1", "user_id": "u-1", "google_calendar_id": "primary@example.com",
                "summary": "Work", "time_zone": "UTC", "is_primary": 1, "access_role": "owner",
                "sync_enabled": 0, "sync_token": "tok", "last_synced_at": null,
                "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z",
                "deleted_at": null
            }"#,
        )
        .unwrap();
        assert!(calendar.is_primary);
        assert!(!calendar.sync_enabled);
        assert_eq!(calendar.last_synced_at, None);
    }

    #[test]
    fn json_booleans_also_deserialize() {
        // Real JSON booleans (e.g. tests, or future JSON-backed stores) work too.
        let calendar: GoogleCalendar = serde_json::from_str(
            r#"{"id":"cal-1","user_id":"u-1","google_calendar_id":"c","is_primary":true,"sync_enabled":false,"created_at":"x","updated_at":"x"}"#,
        )
        .unwrap();
        assert!(calendar.is_primary);
        assert!(!calendar.sync_enabled);
        assert_eq!(calendar.summary, "", "missing fields default");
    }

    #[test]
    fn null_text_columns_map_to_empty_string() {
        let calendar: GoogleCalendar = serde_json::from_str(
            r#"{
                "id": "cal-1", "user_id": "u-1", "google_calendar_id": "c",
                "summary": null, "time_zone": null, "is_primary": 0, "access_role": null,
                "sync_enabled": 1, "sync_token": null, "last_synced_at": null,
                "created_at": "x", "updated_at": "x", "deleted_at": null
            }"#,
        )
        .unwrap();
        assert_eq!(calendar.summary, "");
        assert_eq!(calendar.time_zone, "");
        assert_eq!(calendar.access_role, "");
        assert_eq!(calendar.sync_token, "");
        assert!(!calendar.is_primary);
        assert!(calendar.sync_enabled);
    }

    #[test]
    fn calendar_event_serializes_with_frontend_field_names() {
        let event = CalendarEvent {
            id: "evt-1".to_string(),
            calendar_id: "cal-1".to_string(),
            google_event_id: "google-evt-1".to_string(),
            google_etag: "etag".to_string(),
            google_updated_at: "2026-08-17T10:00:00Z".to_string(),
            last_synced_at: "2026-08-17T12:00:00Z".to_string(),
            title: "Standup".to_string(),
            description: "Daily".to_string(),
            start_time: "2026-08-18T09:00:00Z".to_string(),
            end_time: "2026-08-18T09:30:00Z".to_string(),
            recurrence: String::new(),
            task_id: "task-1".to_string(),
            created_at: "2026-08-17T12:00:00Z".to_string(),
            updated_at: "2026-08-17T12:00:00Z".to_string(),
            deleted_at: None,
        };
        let value: serde_json::Value = serde_json::to_value(&event).unwrap();
        // The seven fields the frontend `CalendarEvent` requires.
        for key in [
            "id",
            "calendar_id",
            "google_event_id",
            "title",
            "description",
            "start_time",
            "end_time",
            "last_synced_at",
        ] {
            assert!(value.get(key).is_some(), "missing {key}: {value}");
        }
        assert_eq!(value["start_time"], "2026-08-18T09:00:00Z");
        assert_eq!(value["task_id"], "task-1", "task link is part of the payload");
    }

    #[test]
    fn task_list_deserializes_from_d1_row_shape() {
        // D1 returns `sort_order` as a JS number and `deleted_at` as null.
        let list: TaskList = serde_json::from_str(
            r##"{
                "id": "l-1", "user_id": "u-1", "name": "Work", "color": "#2a5c8a",
                "sort_order": 0, "created_at": "2026-08-18T00:00:00Z",
                "updated_at": "2026-08-18T00:00:00Z", "deleted_at": null
            }"##,
        )
        .unwrap();
        assert_eq!(list.name, "Work");
        assert_eq!(list.color, "#2a5c8a");
        assert_eq!(list.sort_order, 0);
        assert_eq!(list.deleted_at, None);
    }

    #[test]
    fn task_list_serializes_snake_case_for_frontend() {
        let list = TaskList {
            id: "l-1".to_string(),
            user_id: "u-1".to_string(),
            name: "Work".to_string(),
            color: "#2a5c8a".to_string(),
            sort_order: 2,
            created_at: "2026-08-18T00:00:00Z".to_string(),
            updated_at: "2026-08-18T00:00:00Z".to_string(),
            deleted_at: None,
        };
        let value: serde_json::Value = serde_json::to_value(&list).unwrap();
        // Every field the frontend `TaskList` requires.
        for key in [
            "id",
            "user_id",
            "name",
            "color",
            "sort_order",
            "created_at",
            "updated_at",
        ] {
            assert!(value.get(key).is_some(), "missing {key}: {value}");
        }
        assert_eq!(value["sort_order"], 2);
    }

    #[test]
    fn update_task_list_missing_fields_deserialize_to_none() {
        // A PATCH body that omits a field must not wipe it; an empty `{}`
        // body yields all-`None` (the service rejects that as 400).
        let updates: UpdateTaskList = serde_json::from_str("{}").unwrap();
        assert_eq!(
            updates,
            UpdateTaskList {
                name: None,
                color: None,
                sort_order: None
            }
        );
    }

    #[test]
    fn task_category_deserializes_from_d1_row_shape() {
        // D1 returns INTEGER 0/1 bools and NULL for the nullable columns.
        let category: TaskCategory = serde_json::from_str(
            r##"{
                "id": "c-1", "user_id": "u-1", "list_id": "l-1", "parent_id": null,
                "title": "Deep Work", "slug": "deep-work", "color": "#2a5c8a",
                "is_productive": 1, "google_calendar_id": null, "google_color_id": null,
                "sort_order": 0, "is_untracked": 0,
                "created_at": "2026-08-18T00:00:00Z", "updated_at": "2026-08-18T00:00:00Z",
                "deleted_at": null
            }"##,
        )
        .unwrap();
        assert_eq!(category.list_id.as_deref(), Some("l-1"));
        assert_eq!(category.parent_id, None);
        assert!(category.is_productive);
        assert!(!category.is_untracked);
        assert_eq!(category.google_calendar_id, None);
        assert_eq!(category.sort_order, 0);
    }

    #[test]
    fn task_category_child_row_has_null_list_id() {
        let category: TaskCategory = serde_json::from_str(
            r##"{
                "id": "c-2", "user_id": "u-1", "list_id": null, "parent_id": "c-1",
                "title": "Code Reviews", "slug": "code-reviews", "color": "",
                "is_productive": 0, "google_calendar_id": "work@x.com", "google_color_id": "7",
                "sort_order": 1, "is_untracked": 0,
                "created_at": "2026-08-18T00:00:00Z", "updated_at": "2026-08-18T00:00:00Z",
                "deleted_at": null
            }"##,
        )
        .unwrap();
        assert_eq!(category.list_id, None);
        assert_eq!(category.parent_id.as_deref(), Some("c-1"));
        assert_eq!(category.color, "", "color defaults to empty string");
        assert_eq!(category.google_calendar_id.as_deref(), Some("work@x.com"));
    }

    #[test]
    fn task_category_pattern_deserializes_from_d1_row_shape() {
        let pattern: TaskCategoryPattern = serde_json::from_str(
            r#"{
                "id": "p-1", "category_id": "c-1", "regex": "^Work$",
                "google_calendar_id": null, "sort_order": 0,
                "created_at": "x", "updated_at": "x"
            }"#,
        )
        .unwrap();
        assert_eq!(pattern.regex, "^Work$");
        assert_eq!(pattern.google_calendar_id, None);
        assert_eq!(pattern.sort_order, 0);
    }

    #[test]
    fn task_deserializes_from_d1_row_shape() {
        // D1 returns numbers as JS numbers and NULL for `deleted_at`.
        let task: Task = serde_json::from_str(
            r##"{
                "id": "t-1", "user_id": "u-1", "title": "Review | Work",
                "description": null, "duration_minutes": 15, "priority": "high",
                "difficulty": "hard", "status": "OPEN",
                "created_at": "2026-08-18T00:00:00Z",
                "updated_at": "2026-08-18T00:00:00Z", "deleted_at": null
            }"##,
        )
        .unwrap();
        assert_eq!(task.title, "Review | Work");
        assert_eq!(task.description, "", "NULL maps to empty string");
        assert_eq!(task.duration_minutes, 15);
        assert_eq!(task.priority, "high");
        assert_eq!(task.difficulty, "hard");
        assert_eq!(task.status, "OPEN");
        assert_eq!(task.deleted_at, None);
    }

    #[test]
    fn task_serializes_snake_case_for_frontend() {
        let task = Task {
            id: "t-1".to_string(),
            user_id: "u-1".to_string(),
            title: "Work".to_string(),
            description: "Deep focus".to_string(),
            duration_minutes: 25,
            priority: "medium".to_string(),
            difficulty: "easy".to_string(),
            status: "OPEN".to_string(),
            created_at: "2026-08-18T00:00:00Z".to_string(),
            updated_at: "2026-08-18T00:00:00Z".to_string(),
            deleted_at: None,
        };
        let value: serde_json::Value = serde_json::to_value(&task).unwrap();
        // Every field the frontend `TaskRecord` requires.
        for key in [
            "id",
            "user_id",
            "title",
            "description",
            "duration_minutes",
            "priority",
            "difficulty",
            "status",
            "created_at",
            "updated_at",
        ] {
            assert!(value.get(key).is_some(), "missing {key}: {value}");
        }
        assert_eq!(value["duration_minutes"], 25);
        assert_eq!(value["priority"], "medium");
        assert_eq!(value["difficulty"], "easy");
        assert_eq!(value["status"], "OPEN");
    }

    #[test]
    fn new_task_input_defaults_to_none_for_optional_fields() {
        // A body with only the title must not fail: description/duration/
        // priority/difficulty default server-side.
        let input: NewTaskInput = serde_json::from_str(r#"{"title": "Work"}"#).unwrap();
        assert_eq!(input.title, "Work");
        assert_eq!(input.description, None);
        assert_eq!(input.duration_minutes, None);
        assert_eq!(input.priority, None);
        assert_eq!(input.difficulty, None);
    }

    #[test]
    fn update_task_missing_fields_deserialize_to_none() {
        // An empty `{}` body yields all-`None` (the service rejects that 400).
        let updates: UpdateTask = serde_json::from_str("{}").unwrap();
        assert_eq!(
            updates,
            UpdateTask {
                title: None,
                description: None,
                duration_minutes: None,
                priority: None,
                difficulty: None,
            }
        );
    }

    #[test]
    fn calendar_event_accepts_null_optional_columns() {
        let event: CalendarEvent = serde_json::from_str(
            r#"{
                "id": "evt-1", "calendar_id": "cal-1", "google_event_id": "g-1",
                "google_etag": null, "google_updated_at": null, "last_synced_at": "x",
                "title": "T", "description": null, "start_time": "s", "end_time": "e",
                "recurrence": null, "task_id": null, "created_at": "x", "updated_at": "x",
                "deleted_at": null
            }"#,
        )
        .unwrap();
        assert_eq!(event.description, "");
        assert_eq!(event.google_etag, "");
        assert_eq!(event.recurrence, "");
        assert_eq!(event.task_id, "", "NULL task_id maps to empty string");
    }

    #[test]
    fn task_log_deserializes_from_d1_row_shape() {
        // D1 returns NULL for the nullable columns and a plain `type` column.
        let log: TaskLog = serde_json::from_str(
            r#"{
                "id": "log-1", "task_id": "t-1", "user_id": "u-1", "type": "started",
                "at": "2026-08-18T09:00:00Z", "calendar_id": null, "google_event_id": null,
                "created_at": "2026-08-18T09:00:00Z"
            }"#,
        )
        .unwrap();
        assert_eq!(log.r#type, "started");
        assert_eq!(log.calendar_id, "");
        assert_eq!(log.google_event_id, "");
    }

    #[test]
    fn task_log_serializes_type_column_name() {
        let log = TaskLog {
            id: "log-1".to_string(),
            task_id: "t-1".to_string(),
            user_id: "u-1".to_string(),
            r#type: "stopped".to_string(),
            at: "2026-08-18T09:30:00Z".to_string(),
            calendar_id: "cal-1".to_string(),
            google_event_id: "g-1".to_string(),
            created_at: "2026-08-18T09:30:00Z".to_string(),
        };
        let value: serde_json::Value = serde_json::to_value(&log).unwrap();
        assert_eq!(value["type"], "stopped", "raw identifier serializes as `type`");
    }
}
