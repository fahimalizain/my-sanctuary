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
    fn calendar_event_accepts_null_optional_columns() {
        let event: CalendarEvent = serde_json::from_str(
            r#"{
                "id": "evt-1", "calendar_id": "cal-1", "google_event_id": "g-1",
                "google_etag": null, "google_updated_at": null, "last_synced_at": "x",
                "title": "T", "description": null, "start_time": "s", "end_time": "e",
                "recurrence": null, "created_at": "x", "updated_at": "x", "deleted_at": null
            }"#,
        )
        .unwrap();
        assert_eq!(event.description, "");
        assert_eq!(event.google_etag, "");
        assert_eq!(event.recurrence, "");
    }
}
