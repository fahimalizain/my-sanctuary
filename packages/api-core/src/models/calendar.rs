//! Calendar row types and request DTOs.

use serde::{Deserialize, Serialize};

use super::{de_d1_bool, de_empty_string};

/// A calendar from the user's `/users/me/calendarList`, as stored in
/// `google_calendars`. Doubles as the D1 row projection: field names match the
/// schema, `is_primary`/`sync_enabled` deserialize from D1's `INTEGER 0/1`
/// via [`de_d1_bool`], and nullable TEXT columns map to `""` via
/// [`de_empty_string`].
///
/// Sync *health* columns (`sync_status`, `last_success_at`, …) live on this
/// same row (not a side table) so token + events + health stay in one D1
/// database. calendarList upsert must never write health columns — same
/// invariant as [`GoogleCalendar::event_labels`].
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
    /// Compat field: RFC 3339 instant of the last successful sync; `None` when
    /// never synced. Prefer [`GoogleCalendar::last_success_at`] as the real
    /// success signal; both are written together on success.
    pub last_synced_at: Option<String>,
    /// Cached `calendars.get` `labelProperties.eventLabels` JSON.
    /// Empty string = never fetched. `"[]"` or `[{id, backgroundColor}]` = fetched.
    #[serde(default, deserialize_with = "de_empty_string")]
    pub event_labels: String,
    /// Hash/canonical string of the replica query shape (singleEvents,
    /// optional timeMin, eventTypes). Empty until the first successful sync
    /// records it.
    #[serde(default, deserialize_with = "de_empty_string")]
    pub sync_query_fingerprint: String,
    /// `never_initialized` | `ready` | `retrying` | `rebuilding` |
    /// `authorization_required` | `disabled`.
    #[serde(default, deserialize_with = "de_empty_string")]
    pub sync_status: String,
    /// D1 stores this as `INTEGER 0/1`.
    #[serde(default, deserialize_with = "de_d1_bool")]
    pub initial_sync_complete: bool,
    /// RFC 3339 instant of the most recent sync attempt start; `None` when never attempted.
    #[serde(default)]
    pub last_attempt_at: Option<String>,
    /// RFC 3339 instant of the last successful apply; the real success signal.
    /// `None` when never successfully synced.
    #[serde(default)]
    pub last_success_at: Option<String>,
    /// Last failure code (empty when healthy / never failed).
    #[serde(default, deserialize_with = "de_empty_string")]
    pub last_error_code: String,
    #[serde(default)]
    pub failure_streak: i64,
    /// RFC 3339 instant when the next retry may run; `None` when not backing off.
    #[serde(default)]
    pub next_retry_at: Option<String>,
    #[serde(default)]
    pub dirty_requested_generation: i64,
    #[serde(default)]
    pub dirty_applied_generation: i64,
    /// D1 stores this as `INTEGER 0/1`.
    #[serde(default, deserialize_with = "de_d1_bool")]
    pub full_sync_requested: bool,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub lease_owner: String,
    #[serde(default)]
    pub lease_expires_at: Option<String>,
    /// Bumped on every successful apply so clients can detect cache changes.
    #[serde(default)]
    pub cache_revision: i64,
    /// Current product projection; default `timed_masters_and_exceptions`
    /// (all-day still out of projection).
    #[serde(default, deserialize_with = "de_empty_string")]
    pub projection: String,
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
    /// Defaults to `true` for newly imported calendarList rows. On conflict
    /// for a **living** row the upsert keeps the stored `sync_enabled` (so a
    /// deliberate user disable is never clobbered). On conflict when
    /// resurrecting a soft-deleted row, the incoming value is written.
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
///
/// **Ownership:**
/// - [`CalendarEvent::task_id`] is app-owned and must never be in the
///   Google-overwrite set (SQL `COALESCE` on upsert).
/// - Identity columns (`ical_uid`, `sequence`, `status`, `recurring_event_id`,
///   `original_start`, `start_time_zone`, `end_time_zone`, `is_all_day`,
///   `raw_json`) are Google-owned; a merge may overwrite them.
/// - [`CalendarEvent::raw_json`] is the Google payload for later schema
///   migrations; it is `skip_serializing` so the HTTP envelope never leaks it.
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
    /// App-owned task link (from `extendedProperties.shared.sanctuary_task_id`).
    /// Empty when the event never had one. Must never be in the Google-overwrite
    /// set — upsert uses `COALESCE(NULLIF(excluded.task_id, ''), …)`.
    #[serde(default, deserialize_with = "de_empty_string")]
    pub task_id: String,
    /// Google `iCalUID`; Google-owned, merge may overwrite.
    #[serde(default, deserialize_with = "de_empty_string")]
    pub ical_uid: String,
    /// Google `sequence`; Google-owned, merge may overwrite.
    #[serde(default)]
    pub sequence: i64,
    /// Google `status` (`confirmed` / `tentative` / `cancelled`); Google-owned.
    #[serde(default, deserialize_with = "de_empty_string")]
    pub status: String,
    /// Google `recurringEventId` for exceptions; empty for masters / singles.
    #[serde(default, deserialize_with = "de_empty_string")]
    pub recurring_event_id: String,
    /// Google `originalStartTime` (RFC 3339 or date); empty when not an exception.
    #[serde(default, deserialize_with = "de_empty_string")]
    pub original_start: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub start_time_zone: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub end_time_zone: String,
    /// D1 stores this as `INTEGER 0/1`. Google-owned.
    #[serde(default, deserialize_with = "de_d1_bool")]
    pub is_all_day: bool,
    /// Full Google event JSON for later schema migrations. Never expose on the
    /// HTTP envelope (`skip_serializing`).
    #[serde(default, deserialize_with = "de_empty_string", skip_serializing)]
    pub raw_json: String,
    pub created_at: String,
    pub updated_at: String,
    /// Soft-delete marker; reads filter on `deleted_at IS NULL`.
    pub deleted_at: Option<String>,
}

/// Insert/update input for [`crate::repo::CalendarEventRepo::upsert`] /
/// `upsert_batch`. The D1 implementation generates the UUID `id` and the
/// `created_at`/`updated_at` timestamps.
///
/// **Ownership:** [`NewCalendarEvent::task_id`] is app-owned (SQL COALESCE).
/// Identity columns are Google-owned and a merge may overwrite them.
/// [`NewCalendarEvent::raw_json`] is stored for later schema migrations and
/// must never be exposed on the HTTP envelope.
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
    /// with an empty incoming one (`COALESCE` in SQL). App-owned.
    pub task_id: String,
    /// Google `iCalUID`; Google-owned.
    pub ical_uid: String,
    /// Google `sequence`; Google-owned.
    pub sequence: i64,
    /// Google `status`; Google-owned.
    pub status: String,
    /// Google `recurringEventId`; Google-owned.
    pub recurring_event_id: String,
    /// Google `originalStartTime`; Google-owned.
    pub original_start: String,
    pub start_time_zone: String,
    pub end_time_zone: String,
    /// Google all-day flag; Google-owned.
    pub is_all_day: bool,
    /// Full Google event JSON; never expose on the HTTP envelope.
    pub raw_json: String,
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

/// Verb constants for [`CalendarEventOperation::verb`].
pub const OP_VERB_INSERT: &str = "insert";
pub const OP_VERB_PATCH: &str = "patch";
pub const OP_VERB_DELETE: &str = "delete";
pub const OP_VERB_MOVE: &str = "move";

/// Status constants for [`CalendarEventOperation::status`].
///
/// Machine: `pending` → `google_committed` → `cache_applied`; or
/// `pending` → `failed`; or `pending`/`google_committed` → `conflict`
/// after a 412 retry cap (issue #50 / Vertical 4).
pub const OP_STATUS_PENDING: &str = "pending";
pub const OP_STATUS_GOOGLE_COMMITTED: &str = "google_committed";
pub const OP_STATUS_CACHE_APPLIED: &str = "cache_applied";
pub const OP_STATUS_FAILED: &str = "failed";
pub const OP_STATUS_CONFLICT: &str = "conflict";

/// A durable outbound calendar write journal row
/// (`calendar_event_operations`). Issue #50 / Vertical 4.
///
/// Doubles as the D1 row projection: field names match the schema. TEXT
/// defaults map to `""` via [`de_empty_string`]. No soft-delete — this is a
/// journal, not a domain entity (same spirit as watch channels).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CalendarEventOperation {
    pub id: String,
    pub user_id: String,
    pub calendar_id: String,
    /// Known for patch/delete; filled after cache apply on insert.
    #[serde(default, deserialize_with = "de_empty_string")]
    pub local_event_id: String,
    /// Minted before insert HTTP; known for patch/delete.
    #[serde(default, deserialize_with = "de_empty_string")]
    pub google_event_id: String,
    /// `insert` | `patch` | `delete` | `move`.
    pub verb: String,
    /// Hex sha256 (or stable hex) of canonical payload JSON.
    pub payload_fingerprint: String,
    /// Intended Google JSON body (needed to replay/merge).
    pub payload_json: String,
    /// `pending` | `google_committed` | `cache_applied` | `failed` | `conflict`.
    pub status: String,
    /// Last known etag; empty until Google responds.
    #[serde(default, deserialize_with = "de_empty_string")]
    pub google_etag: String,
    /// 412 retries increment this. D1 INTEGER.
    #[serde(default)]
    pub attempt_count: i64,
    /// Never store tokens / raw OAuth.
    #[serde(default, deserialize_with = "de_empty_string")]
    pub last_error: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Insert input for [`crate::repo::CalendarEventOperationRepo::insert`].
/// The D1 implementation generates the UUID `id` and stamps
/// `created_at`/`updated_at`. Callers insert with
/// [`OP_STATUS_PENDING`]. `attempt_count` starts at 0 in SQL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCalendarEventOperation {
    pub user_id: String,
    pub calendar_id: String,
    pub local_event_id: String,
    pub google_event_id: String,
    pub verb: String,
    pub payload_fingerprint: String,
    pub payload_json: String,
    /// Callers insert as [`OP_STATUS_PENDING`].
    pub status: String,
    pub google_etag: String,
}

/// Request body for `PATCH /api/calendar/events/:id`.
/// At least one field must be `Some` (empty patch → 400).
///
/// `calendar_id` is exclusive: when set, it must be the only field (local
/// destination calendar id → Google `events.move`). Combining it with
/// `start` / `end` / `summary` / `description` / `is_all_day` /
/// `start_time_zone` is rejected as invalid.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct PatchEventFields {
    /// RFC 3339 dateTime or civil `YYYY-MM-DD` (all-day). Timed patches emit
    /// Google `start.dateTime`; all-day (`is_all_day: true`) emits `start.date`.
    #[serde(default)]
    pub start: Option<String>,
    /// RFC 3339 dateTime or civil `YYYY-MM-DD` (all-day). Timed patches emit
    /// Google `end.dateTime`; all-day (`is_all_day: true`) emits `end.date`
    /// (exclusive end date).
    #[serde(default)]
    pub end: Option<String>,
    /// Event title → Google `summary`.
    #[serde(default)]
    pub summary: Option<String>,
    /// Event notes → Google `description`. `Some("")` clears notes;
    /// `None` omits the field from the patch body.
    #[serde(default)]
    pub description: Option<String>,
    /// When `Some(true)`, start/end are written as Google all-day `date`
    /// fields (civil `YYYY-MM-DD`). Requires both start and end. When
    /// `Some(false)` or omitted with start/end, timed `dateTime` path.
    #[serde(default)]
    pub is_all_day: Option<bool>,
    /// IANA zone applied to both start and end `timeZone` on timed patches.
    /// Requires start and end. Ignored / not emitted on all-day date objects.
    /// Empty after trim is treated as omitted from the Google payload.
    #[serde(default)]
    pub start_time_zone: Option<String>,
    /// Local destination calendar id (`google_calendars.id`). Exclusive —
    /// cannot be combined with start/end/summary/description/is_all_day/
    /// start_time_zone. Routes to Google `events.move` (not `events.patch`).
    #[serde(default)]
    pub calendar_id: Option<String>,
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
    /// When set (with `occurrence_id`), the created event carries
    /// `extendedProperties.shared.sanctuary_routine_id` (slice 6 — a
    /// started occurrence's one-shot log). Mutually exclusive with
    /// `task_id` at the call sites; the two carriers never mix on one event.
    #[serde(default)]
    pub routine_id: Option<String>,
    /// When set (with `routine_id`), the created event carries
    /// `extendedProperties.shared.sanctuary_occurrence_id` (slice 6).
    #[serde(default)]
    pub occurrence_id: Option<String>,
    /// Category hex (any `#rgb`/`#rrggbb`); `create_event` snaps it onto the
    /// 24 event-label palette and sends the matching cached label id with
    /// `eventLabelVersion=1`. Omitted from the Google payload when `None` or
    /// blank after trim — hand-created events stay uncolored.
    #[serde(default)]
    pub color_hex: Option<String>,
    /// Focus segment flag (task-focus, slice 3): when `task_id` is set AND
    /// this is `true`, the insert payload also carries
    /// `extendedProperties.shared.sanctuary_focus = "1"` next to the task
    /// carrier — "never send a partial shared map" (the key is omitted, never
    /// `"0"`, when `false`). `start_task` stays unfocused (`false`).
    #[serde(default)]
    pub sanctuary_focus: bool,
    /// Create-time snapshot written to
    /// `extendedProperties.shared.sanctuary_priority`. Omitted from the
    /// shared map when `None`. Never patched when the task later changes.
    #[serde(default)]
    pub priority: Option<String>,
    /// Create-time snapshot written to
    /// `extendedProperties.shared.sanctuary_difficulty`. Omitted from the
    /// shared map when `None`. Never patched when the task later changes.
    #[serde(default)]
    pub difficulty: Option<String>,
}


#[cfg(test)]
mod tests {
    use super::*;

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
            ical_uid: "uid-1".to_string(),
            sequence: 2,
            status: "confirmed".to_string(),
            recurring_event_id: String::new(),
            original_start: String::new(),
            start_time_zone: "UTC".to_string(),
            end_time_zone: "UTC".to_string(),
            is_all_day: false,
            raw_json: r#"{"id":"google-evt-1","secret":"must-not-leak"}"#.to_string(),
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
        assert!(
            value.get("raw_json").is_none(),
            "raw_json must never serialize onto the HTTP envelope: {value}"
        );
    }

    #[test]
    fn calendar_event_serialization_never_includes_raw_json() {
        let event = CalendarEvent {
            id: "evt-1".to_string(),
            calendar_id: "cal-1".to_string(),
            google_event_id: "g-1".to_string(),
            google_etag: String::new(),
            google_updated_at: String::new(),
            last_synced_at: "2026-08-17T12:00:00Z".to_string(),
            title: "T".to_string(),
            description: String::new(),
            start_time: "2026-08-18T09:00:00Z".to_string(),
            end_time: "2026-08-18T09:30:00Z".to_string(),
            recurrence: String::new(),
            task_id: String::new(),
            ical_uid: String::new(),
            sequence: 0,
            status: String::new(),
            recurring_event_id: String::new(),
            original_start: String::new(),
            start_time_zone: String::new(),
            end_time_zone: String::new(),
            is_all_day: false,
            raw_json: r#"{"private":true}"#.to_string(),
            created_at: "2026-08-17T12:00:00Z".to_string(),
            updated_at: "2026-08-17T12:00:00Z".to_string(),
            deleted_at: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(
            !json.contains("raw_json") && !json.contains("private"),
            "raw_json must be skip_serializing: {json}"
        );
    }

    #[test]
    fn google_calendar_missing_health_fields_deserializes_with_defaults() {
        // Older JSON / pre-migration rows omit the health columns entirely.
        let calendar: GoogleCalendar = serde_json::from_str(
            r#"{
                "id": "cal-1", "user_id": "u-1", "google_calendar_id": "c",
                "summary": "Work", "time_zone": "UTC", "is_primary": 1, "access_role": "owner",
                "sync_enabled": 1, "sync_token": "tok", "last_synced_at": "2026-01-01T00:00:00Z",
                "created_at": "x", "updated_at": "x", "deleted_at": null
            }"#,
        )
        .unwrap();
        assert_eq!(calendar.sync_status, "");
        assert_eq!(calendar.sync_query_fingerprint, "");
        assert!(!calendar.initial_sync_complete);
        assert_eq!(calendar.last_attempt_at, None);
        assert_eq!(calendar.last_success_at, None);
        assert_eq!(calendar.last_error_code, "");
        assert_eq!(calendar.failure_streak, 0);
        assert_eq!(calendar.next_retry_at, None);
        assert_eq!(calendar.dirty_requested_generation, 0);
        assert_eq!(calendar.dirty_applied_generation, 0);
        assert!(!calendar.full_sync_requested);
        assert_eq!(calendar.lease_owner, "");
        assert_eq!(calendar.lease_expires_at, None);
        assert_eq!(calendar.cache_revision, 0);
        assert_eq!(calendar.projection, "");
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

}
