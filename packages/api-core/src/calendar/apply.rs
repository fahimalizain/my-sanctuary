//! Replica apply classification and Google event → cache row mapping.
//!
//! Owns the shared apply contract used by the replica walk (and, later, any
//! path that writes the same projection). Classification decides upsert vs
//! soft-delete; mapping populates Google-owned identity columns and `raw_json`.
//!
//! # Write-time category stamp
//!
//! [`map_cached_event`] also stamps the app-owned **leaf** `category_id` from
//! [`EventClassifyCtx`] so reports can read a stored category instead of
//! re-matching titles. The context carries the **Google** calendar id
//! (pattern scope), not the local `google_calendars.id` the row stores.
//!
//! Because the SQL upsert overwrites the column, every production upsert must
//! pass a real context — [`EventClassifyCtx::none`] is for tests only, and a
//! failed taxonomy load uses [`EventClassifyCtx::preserve`], which sets
//! `preserve_category` so the merge keeps the stored stamp instead of writing
//! `NULL`.
//!
//! # Projection and storage
//!
//! - **All-day civil dates** are stored as RFC 3339 midnight **UTC**
//!   (`YYYY-MM-DDT00:00:00Z`), not the calendar's time zone. GET overlap stays
//!   TEXT lexicographic on that shape. Residual risk: a non-UTC civil date
//!   stored as Z midnight is not the true instant — accepted this slice; do
//!   not invent INTEGER epoch columns here.
//! - **GET projection** name remains `timed_masters_and_exceptions` (health
//!   string unchanged). All-day rows are **stored** (so a timed→all-day
//!   overwrite hits the same natural key) and **included in range list SQL**
//!   so the week grid can paint them. Running-task list still excludes all-day.
//! - **Cancelled exceptions** (`status=cancelled` + non-empty `recurringEventId`)
//!   are stored as living sparse rows. Ordinary cancelled events (no
//!   `recurringEventId`) are soft-deleted.
//! - **Empty `items` apply** is a no-op. Success still belongs to the caller
//!   when a terminal `nextSyncToken` exists.
//!
//! Merge key is always `(calendar_id, google_event_id)`. Never unique on
//! `google_event_id` alone. Never use `iCalUID` as PK. `task_id` stays
//! COALESCE-protected at the SQL layer.

use serde::{Deserialize, Serialize};

use crate::categories::{classify_event_leaf, CategoryWithPatterns};
use crate::models::{CalendarEvent, NewCalendarEvent, TaskCategory};

/// Write-time classifier context for one calendar.
///
/// `google_calendar_id` is the **Google** calendar id
/// (`GoogleCalendar::google_calendar_id`) — never the local
/// `google_calendars.id` the row's `calendar_id` column holds. Pattern scope
/// compares Google ids.
///
/// Every production upsert must carry a real context: the SQL upsert
/// **overwrites** `category_id` (including back to `NULL`), so a walk that
/// forgets to stamp a previously classified row wipes it. Only tests use
/// [`EventClassifyCtx::none`]; a failed taxonomy load uses
/// [`EventClassifyCtx::preserve`], which keeps the stored stamp instead.
#[derive(Debug, Clone)]
pub struct EventClassifyCtx<'a> {
    /// Google calendar id of the inbound events (scoped-pattern gate).
    pub google_calendar_id: &'a str,
    /// Living categories (leaf lookup + `sort_order` tie-break).
    pub categories: &'a [TaskCategory],
    /// Compiled matchers built from those categories' patterns.
    pub matchers: &'a [CategoryWithPatterns],
    /// Keep the stored `category_id` on conflict — see
    /// [`NewCalendarEvent::preserve_category`]. Never a match against
    /// `matchers`: the title was not classified, it was not looked at.
    pub preserve: bool,
}

impl EventClassifyCtx<'_> {
    /// Empty context: no patterns match, so an upsert stamps
    /// `category_id = None`. Use only for tests that deliberately have no
    /// taxonomy (and where the stored stamp does not matter) — a production
    /// failure path uses [`Self::preserve`].
    pub fn none() -> Self {
        Self {
            google_calendar_id: "",
            categories: &[],
            matchers: &[],
            preserve: false,
        }
    }

    /// Context for a **failed** taxonomy load: nothing is classified and the
    /// stored stamp is kept (`preserve_category = true`), so a walk over a
    /// transient load failure cannot erase a good `category_id`. A later
    /// reclassify job fills any genuinely missing stamp.
    pub fn preserve() -> Self {
        Self {
            google_calendar_id: "",
            categories: &[],
            matchers: &[],
            preserve: true,
        }
    }
}

/// Action to take for one Google `events.list` item during replica apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplicaApplyAction {
    Upsert(NewCalendarEvent),
    SoftDelete { google_event_id: String },
}

/// Classify one Google list item into an apply action.
///
/// 1. `status == "cancelled"` **and** non-empty `recurringEventId` → upsert a
///    sparse cancelled exception (do **not** soft-delete).
/// 2. `status == "cancelled"` without `recurringEventId` → soft-delete by
///    google event id (summary not required).
/// 3. Otherwise → upsert via [`map_cached_event`] (living event, including
///    all-day and no-time), stamped with `ctx`'s leaf category.
pub fn classify_replica_item(
    item: &GoogleEvent,
    calendar_id: &str,
    now_rfc3339: &str,
    ctx: &EventClassifyCtx<'_>,
) -> ReplicaApplyAction {
    if item.status.as_deref() == Some("cancelled") {
        if is_cancelled_exception(item) {
            return ReplicaApplyAction::Upsert(map_cached_event(
                item,
                calendar_id,
                now_rfc3339,
                ctx,
            ));
        }
        return ReplicaApplyAction::SoftDelete {
            google_event_id: item.id.clone(),
        };
    }

    ReplicaApplyAction::Upsert(map_cached_event(item, calendar_id, now_rfc3339, ctx))
}

/// `status == "cancelled"` **and** a non-empty trimmed `recurringEventId`:
/// stored as a living sparse row instead of being soft-deleted.
fn is_cancelled_exception(item: &GoogleEvent) -> bool {
    item.status.as_deref() == Some("cancelled")
        && item
            .recurring_event_id
            .as_deref()
            .map(str::trim)
            .is_some_and(|id| !id.is_empty())
}

/// [`map_google_event`] (or the sparse cancelled-exception shape) plus the
/// app-owned leaf category stamp.
///
/// The category is the deterministic winner of [`classify_event_leaf`] under
/// the ctx's Google calendar scope; `None` (no match, or only the `untracked`
/// sink) writes SQL `NULL`. Called by every upsert path — replica walk, window
/// fetch, and the journaled create/patch/move cache applies.
///
/// A [`EventClassifyCtx::preserve`] context (a failed taxonomy load) skips the
/// matchers entirely and marks the row `preserve_category`: the upsert then
/// keeps the stored stamp instead of writing `NULL`.
pub fn map_cached_event(
    event: &GoogleEvent,
    calendar_id: &str,
    now_rfc3339: &str,
    ctx: &EventClassifyCtx<'_>,
) -> NewCalendarEvent {
    let mut row = if is_cancelled_exception(event) {
        map_cancelled_exception(event, calendar_id, now_rfc3339)
    } else {
        map_google_event(event, calendar_id, now_rfc3339)
    };
    if ctx.preserve {
        row.preserve_category = true;
        return row;
    }
    row.preserve_category = false;
    row.category_id = classify_event_leaf(
        &row.title,
        ctx.google_calendar_id,
        ctx.categories,
        ctx.matchers,
    );
    row
}

/// Converts a Google event API response into the local cache model.
///
/// Populates Google-owned identity columns and `raw_json`. `task_id` is copied
/// from `extendedProperties.shared.sanctuary_task_id`; events without the
/// property map to `""` and the upsert's `COALESCE` leaves any stored value
/// untouched. `sanctuary_focus` / priority / difficulty are create-time
/// snapshots on Google and are not cached. `category_id` is always `None` here
/// — the leaf stamp comes from [`map_cached_event`] — and
/// `preserve_category` is `true`, so a mapper that reaches the repo directly
/// (no classifier context at all) never blanks a stored stamp either.
pub fn map_google_event(
    event: &GoogleEvent,
    calendar_id: &str,
    now_rfc3339: &str,
) -> NewCalendarEvent {
    let (start_time, start_time_zone, start_all_day) = google_time_parts(&event.start);
    let (end_time, end_time_zone, end_all_day) = google_time_parts(&event.end);
    let is_all_day = start_all_day || end_all_day;
    let original_start = google_time_parts(&event.original_start_time).0;

    NewCalendarEvent {
        calendar_id: calendar_id.to_string(),
        google_event_id: event.id.clone(),
        google_etag: event.etag.clone().unwrap_or_default(),
        google_updated_at: event.updated.clone().unwrap_or_default(),
        last_synced_at: now_rfc3339.to_string(),
        title: event.summary.clone().unwrap_or_default(),
        description: event.description.clone().unwrap_or_default(),
        start_time,
        end_time,
        recurrence: event
            .recurrence
            .as_ref()
            .map(|rules| serde_json::to_string(rules).unwrap_or_default())
            .unwrap_or_default(),
        task_id: event
            .extended_properties
            .as_ref()
            .and_then(|props| props.shared.as_ref())
            .and_then(|shared| shared.sanctuary_task_id.clone())
            .unwrap_or_default(),
        ical_uid: event.ical_uid.clone().unwrap_or_default(),
        sequence: event.sequence.unwrap_or(0),
        status: event.status.clone().unwrap_or_default(),
        recurring_event_id: event
            .recurring_event_id
            .clone()
            .unwrap_or_default(),
        original_start,
        start_time_zone,
        end_time_zone,
        is_all_day,
        raw_json: serde_json::to_string(event).unwrap_or_default(),
        category_id: None,
        // No classifier context here: keep whatever stamp is already stored.
        preserve_category: true,
        is_self_organized: google_is_self_organized(event),
        event_type: event
            .event_type
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .to_string(),
    }
}

/// Google `organizer.self` as a boolean: an absent organizer means "no other
/// organizer" → organized. An organizer object **without** `self: true`
/// (including an empty `{}`) → not organized.
fn google_is_self_organized(event: &GoogleEvent) -> bool {
    match event.organizer.as_ref() {
        None => true,
        Some(organizer) => organizer.is_self == Some(true),
    }
}

/// Sparse cancelled exception: keep as a living row with `status=cancelled`.
///
/// Times prefer `originalStartTime` (the exception's slot). `end` falls back
/// to start when Google omits it (NOT NULL columns).
fn map_cancelled_exception(
    event: &GoogleEvent,
    calendar_id: &str,
    now_rfc3339: &str,
) -> NewCalendarEvent {
    let (orig_start, orig_start_tz, orig_all_day) = google_time_parts(&event.original_start_time);
    let (end_from_event, end_tz, _) = google_time_parts(&event.end);
    let end_time = if end_from_event.is_empty() {
        orig_start.clone()
    } else {
        end_from_event
    };

    let mut row = map_google_event(event, calendar_id, now_rfc3339);
    row.title = event.summary.clone().unwrap_or_default();
    row.start_time = orig_start.clone();
    row.end_time = end_time;
    row.status = "cancelled".to_string();
    row.original_start = orig_start;
    if !orig_start_tz.is_empty() {
        row.start_time_zone = orig_start_tz;
    }
    if !end_tz.is_empty() {
        row.end_time_zone = end_tz;
    }
    // All-day-ness follows the exception slot (`originalStartTime`).
    row.is_all_day = orig_all_day;
    row
}

/// Builds the full DB-shaped [`CalendarEvent`] (for API responses) from the
/// upsert input plus the generated/persisted id.
pub fn row_from_new_event(
    event: NewCalendarEvent,
    id: String,
    now_rfc3339: &str,
) -> CalendarEvent {
    CalendarEvent {
        id,
        calendar_id: event.calendar_id,
        google_event_id: event.google_event_id,
        google_etag: event.google_etag,
        google_updated_at: event.google_updated_at,
        last_synced_at: event.last_synced_at,
        title: event.title,
        description: event.description,
        start_time: event.start_time,
        end_time: event.end_time,
        recurrence: event.recurrence,
        task_id: event.task_id,
        ical_uid: event.ical_uid,
        sequence: event.sequence,
        status: event.status,
        recurring_event_id: event.recurring_event_id,
        original_start: event.original_start,
        start_time_zone: event.start_time_zone,
        end_time_zone: event.end_time_zone,
        is_all_day: event.is_all_day,
        raw_json: event.raw_json,
        category_id: event.category_id,
        is_self_organized: event.is_self_organized,
        event_type: event.event_type,
        created_at: now_rfc3339.to_string(),
        updated_at: now_rfc3339.to_string(),
        deleted_at: None,
    }
}

/// `(stored_rfc3339, time_zone, is_all_day_from_this_field)`.
///
/// Timed `dateTime` wins over `date`. All-day `date` becomes
/// `YYYY-MM-DDT00:00:00Z`. Missing → empty string / false.
fn google_time_parts(time: &Option<GoogleEventTime>) -> (String, String, bool) {
    let Some(t) = time else {
        return (String::new(), String::new(), false);
    };
    let tz = t.time_zone.clone().unwrap_or_default();
    let date_time = t.date_time.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if let Some(dt) = date_time {
        return (dt.to_string(), tz, false);
    }
    let date = t.date.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if let Some(d) = date {
        return (format!("{d}T00:00:00Z"), tz, true);
    }
    (String::new(), tz, false)
}

// ──────────────────────────────────────────
// Google event DTOs (list / insert / patch)
// ──────────────────────────────────────────

/// A Google Calendar event as returned by `events.list` / `events.insert` /
/// `events.patch`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleEvent {
    pub id: String,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub updated: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub recurrence: Option<Vec<String>>,
    #[serde(default)]
    pub start: Option<GoogleEventTime>,
    #[serde(default)]
    pub end: Option<GoogleEventTime>,
    /// Google `organizer`. Absent on most hand-created events; `self: true`
    /// marks the calendar owner as the organizer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organizer: Option<GoogleEventOrganizer>,
    /// Google `eventType` (`default` / `focusTime` / `outOfOffice` /
    /// `fromGmail`); never coerced, `None` when absent.
    #[serde(default, rename = "eventType", skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
    #[serde(default, rename = "extendedProperties")]
    pub extended_properties: Option<GoogleEventExtendedProperties>,
    #[serde(default, rename = "iCalUID")]
    pub ical_uid: Option<String>,
    #[serde(default)]
    pub sequence: Option<i64>,
    #[serde(default, rename = "recurringEventId")]
    pub recurring_event_id: Option<String>,
    #[serde(default, rename = "originalStartTime")]
    pub original_start_time: Option<GoogleEventTime>,
}

/// `organizer` of a Google event. Only the `self` flag is modelled — the
/// email/displayName are not cached.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleEventOrganizer {
    #[serde(default, rename = "self", skip_serializing_if = "Option::is_none")]
    pub is_self: Option<bool>,
}

/// `extendedProperties` of a Google event. Only the shared map is modelled —
/// the task timer's `sanctuary_task_id` lives under `shared`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleEventExtendedProperties {
    #[serde(default)]
    pub shared: Option<GoogleEventSharedProperties>,
}

/// `extendedProperties.shared` of a Google event. Every key we write is
/// modelled here. Absent keys deserialize to `None`; `None` values are skipped
/// on serialize, so the wire shape never carries `"0"`/empty placeholders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct GoogleEventSharedProperties {
    /// Client-supplied Google event id, stamped on every insert (issue #50).
    /// Stable across retry; useful for restore. Always set on create.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "sanctuary_event_id")]
    pub sanctuary_event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "sanctuary_task_id")]
    pub sanctuary_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "sanctuary_focus")]
    pub sanctuary_focus: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "sanctuary_priority")]
    pub sanctuary_priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "sanctuary_difficulty")]
    pub sanctuary_difficulty: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "sanctuary_routine_id")]
    pub sanctuary_routine_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "sanctuary_occurrence_id")]
    pub sanctuary_occurrence_id: Option<String>,
}

/// `start`/`end`/`originalStartTime` of a Google event. Timed events use
/// `dateTime`; all-day events use `date` (civil `YYYY-MM-DD`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleEventTime {
    #[serde(default, rename = "dateTime")]
    pub date_time: Option<String>,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default, rename = "timeZone")]
    pub time_zone: Option<String>,
}

/// One page of `events.list`.
#[derive(Debug, Deserialize)]
pub struct EventsPage {
    #[serde(default)]
    pub items: Option<Vec<GoogleEvent>>,
    #[serde(default, rename = "nextSyncToken")]
    pub next_sync_token: Option<String>,
    #[serde(default, rename = "nextPageToken")]
    pub next_page_token: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timed_living() -> GoogleEvent {
        GoogleEvent {
            id: "evt-1".into(),
            etag: Some("\"e1\"".into()),
            updated: Some("2026-08-17T10:00:00Z".into()),
            status: Some("confirmed".into()),
            summary: Some("Standup".into()),
            description: Some("Daily".into()),
            recurrence: None,
            organizer: None,
            event_type: None,
            start: Some(GoogleEventTime {
                date_time: Some("2026-08-18T09:00:00Z".into()),
                date: None,
                time_zone: Some("UTC".into()),
            }),
            end: Some(GoogleEventTime {
                date_time: Some("2026-08-18T09:30:00Z".into()),
                date: None,
                time_zone: Some("UTC".into()),
            }),
            extended_properties: Some(GoogleEventExtendedProperties {
                shared: Some(GoogleEventSharedProperties {
                    sanctuary_task_id: Some("task-42".into()),
                    ..Default::default()
                }),
            }),
            ical_uid: Some("uid-1@google.com".into()),
            sequence: Some(3),
            recurring_event_id: None,
            original_start_time: None,
        }
    }

    #[test]
    fn living_timed_maps_identity_raw_json_and_not_all_day() {
        let row = map_google_event(&timed_living(), "cal-1", "2026-08-17T12:00:00Z");
        assert_eq!(row.calendar_id, "cal-1");
        assert_eq!(row.google_event_id, "evt-1");
        assert_eq!(row.title, "Standup");
        assert_eq!(row.start_time, "2026-08-18T09:00:00Z");
        assert_eq!(row.end_time, "2026-08-18T09:30:00Z");
        assert_eq!(row.ical_uid, "uid-1@google.com");
        assert_eq!(row.sequence, 3);
        assert_eq!(row.status, "confirmed");
        assert_eq!(row.start_time_zone, "UTC");
        assert_eq!(row.end_time_zone, "UTC");
        assert!(!row.is_all_day);
        assert_eq!(row.task_id, "task-42");
        assert!(row.raw_json.contains("evt-1"), "{}", row.raw_json);
        assert!(row.raw_json.contains("Standup"), "{}", row.raw_json);
        assert!(row.recurring_event_id.is_empty());
        assert!(row.original_start.is_empty());
    }

    #[test]
    fn all_day_maps_midnight_utc_and_is_all_day_flag() {
        let event = GoogleEvent {
            id: "all-day".into(),
            etag: None,
            updated: None,
            status: Some("confirmed".into()),
            summary: Some("Holiday".into()),
            description: None,
            recurrence: None,
            organizer: None,
            event_type: None,
            start: Some(GoogleEventTime {
                date_time: None,
                date: Some("2026-08-01".into()),
                time_zone: None,
            }),
            end: Some(GoogleEventTime {
                date_time: None,
                date: Some("2026-08-02".into()),
                time_zone: None,
            }),
            extended_properties: None,
            ical_uid: Some("uid-ad".into()),
            sequence: Some(0),
            recurring_event_id: None,
            original_start_time: None,
        };
        let row = map_google_event(&event, "cal-1", "2026-08-17T12:00:00Z");
        assert!(row.is_all_day);
        assert_eq!(row.start_time, "2026-08-01T00:00:00Z");
        assert_eq!(row.end_time, "2026-08-02T00:00:00Z");
        assert_eq!(row.status, "confirmed");
        assert_eq!(row.ical_uid, "uid-ad");
        assert!(!row.raw_json.is_empty());
    }

    #[test]
    fn timed_fields_win_over_date() {
        let event = GoogleEvent {
            id: "both".into(),
            etag: None,
            updated: None,
            status: None,
            summary: Some("Mixed".into()),
            description: None,
            recurrence: None,
            organizer: None,
            event_type: None,
            start: Some(GoogleEventTime {
                date_time: Some("2026-08-18T09:00:00Z".into()),
                date: Some("2026-08-18".into()),
                time_zone: Some("America/New_York".into()),
            }),
            end: Some(GoogleEventTime {
                date_time: Some("2026-08-18T10:00:00Z".into()),
                date: Some("2026-08-18".into()),
                time_zone: Some("America/New_York".into()),
            }),
            extended_properties: None,
            ical_uid: None,
            sequence: None,
            recurring_event_id: None,
            original_start_time: None,
        };
        let row = map_google_event(&event, "cal-1", "2026-08-17T12:00:00Z");
        assert!(!row.is_all_day);
        assert_eq!(row.start_time, "2026-08-18T09:00:00Z");
        assert_eq!(row.end_time, "2026-08-18T10:00:00Z");
        assert_eq!(row.start_time_zone, "America/New_York");
        assert_eq!(row.sequence, 0);
        assert_eq!(row.status, "");
    }

    #[test]
    fn ordinary_cancelled_is_soft_delete_even_without_summary() {
        let event = GoogleEvent {
            id: "gone".into(),
            etag: None,
            updated: None,
            status: Some("cancelled".into()),
            summary: None,
            description: None,
            recurrence: None,
            organizer: None,
            event_type: None,
            start: None,
            end: None,
            extended_properties: None,
            ical_uid: None,
            sequence: None,
            recurring_event_id: None,
            original_start_time: None,
        };
        match classify_replica_item(&event, "cal-1", "2026-08-17T12:00:00Z", &EventClassifyCtx::none()) {
            ReplicaApplyAction::SoftDelete { google_event_id } => {
                assert_eq!(google_event_id, "gone");
            }
            other => panic!("expected SoftDelete, got {other:?}"),
        }
    }

    #[test]
    fn cancelled_exception_is_upsert_with_sparse_fields() {
        let event = GoogleEvent {
            id: "exc-1".into(),
            etag: Some("\"e2\"".into()),
            updated: Some("2026-08-18T01:00:00Z".into()),
            status: Some("cancelled".into()),
            summary: None,
            description: None,
            recurrence: None,
            organizer: None,
            event_type: None,
            start: None,
            end: None,
            extended_properties: None,
            ical_uid: Some("master-uid".into()),
            sequence: Some(1),
            recurring_event_id: Some("master-series".into()),
            original_start_time: Some(GoogleEventTime {
                date_time: Some("2026-08-20T15:00:00Z".into()),
                date: None,
                time_zone: Some("UTC".into()),
            }),
        };
        match classify_replica_item(&event, "cal-1", "2026-08-17T12:00:00Z", &EventClassifyCtx::none()) {
            ReplicaApplyAction::Upsert(row) => {
                assert_eq!(row.google_event_id, "exc-1");
                assert_eq!(row.status, "cancelled");
                assert_eq!(row.recurring_event_id, "master-series");
                assert_eq!(row.title, "");
                assert_eq!(row.start_time, "2026-08-20T15:00:00Z");
                assert_eq!(row.end_time, "2026-08-20T15:00:00Z", "end falls back to start");
                assert_eq!(row.original_start, "2026-08-20T15:00:00Z");
                assert_eq!(row.ical_uid, "master-uid");
                assert!(!row.raw_json.is_empty());
                assert!(!row.is_all_day);
            }
            other => panic!("expected Upsert, got {other:?}"),
        }
    }

    #[test]
    fn cancelled_exception_all_day_original_start() {
        let event = GoogleEvent {
            id: "exc-ad".into(),
            etag: None,
            updated: None,
            status: Some("cancelled".into()),
            summary: Some("Skip day".into()),
            description: None,
            recurrence: None,
            organizer: None,
            event_type: None,
            start: None,
            end: Some(GoogleEventTime {
                date_time: None,
                date: Some("2026-08-21".into()),
                time_zone: None,
            }),
            extended_properties: None,
            ical_uid: None,
            sequence: None,
            recurring_event_id: Some("  master  ".into()),
            original_start_time: Some(GoogleEventTime {
                date_time: None,
                date: Some("2026-08-20".into()),
                time_zone: None,
            }),
        };
        match classify_replica_item(&event, "cal-1", "2026-08-17T12:00:00Z", &EventClassifyCtx::none()) {
            ReplicaApplyAction::Upsert(row) => {
                assert_eq!(row.recurring_event_id, "  master  ");
                assert_eq!(row.start_time, "2026-08-20T00:00:00Z");
                assert_eq!(row.end_time, "2026-08-21T00:00:00Z");
                assert_eq!(row.original_start, "2026-08-20T00:00:00Z");
                assert!(row.is_all_day);
                assert_eq!(row.title, "Skip day");
            }
            other => panic!("expected Upsert, got {other:?}"),
        }
    }

    #[test]
    fn task_id_copied_from_shared_property() {
        let row = map_google_event(&timed_living(), "cal-1", "2026-08-17T12:00:00Z");
        assert_eq!(row.task_id, "task-42");

        let mut bare = timed_living();
        bare.extended_properties = None;
        let row = map_google_event(&bare, "cal-1", "2026-08-17T12:00:00Z");
        assert_eq!(row.task_id, "");
    }

    #[test]
    fn organizer_absent_or_self_is_organized() {
        // Absent organizer → organized ("no other organizer").
        let row = map_google_event(&timed_living(), "cal-1", "2026-08-17T12:00:00Z");
        assert!(row.is_self_organized);

        // `{"self": true}` → organized.
        let mut event = timed_living();
        event.organizer = Some(GoogleEventOrganizer { is_self: Some(true) });
        assert!(map_google_event(&event, "cal-1", "2026-08-17T12:00:00Z").is_self_organized);

        // Organizer present without `self: true` (an email, or `{}`) → not.
        for organizer in [
            GoogleEventOrganizer { is_self: Some(false) },
            GoogleEventOrganizer { is_self: None },
        ] {
            let mut event = timed_living();
            event.organizer = Some(organizer);
            assert!(!map_google_event(&event, "cal-1", "2026-08-17T12:00:00Z").is_self_organized);
        }
    }

    #[test]
    fn organizer_and_event_type_deserialize_and_reach_raw_json() {
        // The wire shape Google actually sends. `eventType` and `organizer`
        // were dropped before this slice — raw_json is the re-sync fallback.
        let event: GoogleEvent = serde_json::from_str(
            r#"{
                "id": "evt-9",
                "summary": "Deep work",
                "eventType": "focusTime",
                "organizer": {"email": "me@x.com", "self": true},
                "start": {"dateTime": "2026-08-18T09:00:00Z"},
                "end": {"dateTime": "2026-08-18T10:00:00Z"}
            }"#,
        )
        .expect("google payload parses");
        let row = map_google_event(&event, "cal-1", "2026-08-17T12:00:00Z");
        assert_eq!(row.event_type, "focusTime", "eventType is never coerced");
        assert!(row.is_self_organized);
        assert!(row.raw_json.contains("focusTime"), "{}", row.raw_json);
        assert!(row.raw_json.contains("organizer"), "{}", row.raw_json);
    }

    #[test]
    fn absent_event_type_maps_to_empty_and_is_trimmed() {
        let row = map_google_event(&timed_living(), "cal-1", "2026-08-17T12:00:00Z");
        assert_eq!(row.event_type, "");

        let mut event = timed_living();
        event.event_type = Some("  outOfOffice  ".into());
        let row = map_google_event(&event, "cal-1", "2026-08-17T12:00:00Z");
        assert_eq!(row.event_type, "outOfOffice");
    }

    #[test]
    fn map_google_event_leaves_category_unstamped() {
        // The leaf stamp is `map_cached_event`'s job — never the plain mapper's.
        let row = map_google_event(&timed_living(), "cal-1", "2026-08-17T12:00:00Z");
        assert_eq!(row.category_id, None);
    }

    #[test]
    fn map_cached_event_stamps_leaf_category_and_keeps_task_id() {
        let categories = vec![category_row("cat-work", 0, false)];
        let matchers = vec![crate::categories::CategoryWithPatterns::new(
            "cat-work".to_string(),
            None,
            vec![pattern_row("cat-work", "^Standup$")],
        )];
        let ctx = EventClassifyCtx {
            google_calendar_id: "work@x.com",
            categories: &categories,
            matchers: &matchers,
            preserve: false,
        };

        // A title that matches stamps the leaf id and still carries task_id.
        let row = map_cached_event(&timed_living(), "cal-1", "2026-08-17T12:00:00Z", &ctx);
        assert_eq!(row.category_id.as_deref(), Some("cat-work"));
        assert_eq!(row.task_id, "task-42");

        // A title that matches nothing writes None (SQL NULL), never a sink id.
        let mut other = timed_living();
        other.summary = Some("Lunch".into());
        let row = map_cached_event(&other, "cal-1", "2026-08-17T12:00:00Z", &ctx);
        assert_eq!(row.category_id, None);
    }

    #[test]
    fn map_cached_event_stamps_cancelled_exception_from_its_title() {
        // Cancelled exceptions keep their sparse slot fields and are stamped
        // from the (possibly empty) title.
        let categories = vec![category_row("cat-work", 0, false)];
        let matchers = vec![crate::categories::CategoryWithPatterns::new(
            "cat-work".to_string(),
            None,
            vec![pattern_row("cat-work", ".*")],
        )];
        let ctx = EventClassifyCtx {
            google_calendar_id: "work@x.com",
            categories: &categories,
            matchers: &matchers,
            preserve: false,
        };
        let event = GoogleEvent {
            id: "exc-1".into(),
            etag: None,
            updated: None,
            status: Some("cancelled".into()),
            summary: None,
            description: None,
            recurrence: None,
            organizer: Some(GoogleEventOrganizer { is_self: None }),
            event_type: None,
            start: None,
            end: None,
            extended_properties: None,
            ical_uid: None,
            sequence: None,
            recurring_event_id: Some("master-1".into()),
            original_start_time: Some(GoogleEventTime {
                date_time: Some("2026-08-20T15:00:00Z".into()),
                date: None,
                time_zone: None,
            }),
        };
        match classify_replica_item(&event, "cal-1", "2026-08-17T12:00:00Z", &ctx) {
            ReplicaApplyAction::Upsert(row) => {
                assert_eq!(row.status, "cancelled");
                assert_eq!(row.original_start, "2026-08-20T15:00:00Z");
                assert_eq!(
                    row.category_id.as_deref(),
                    Some("cat-work"),
                    "an empty title still classifies (catch-all) and stays coherent"
                );
                assert!(!row.is_self_organized, "organizer without self: not organized");
            }
            other => panic!("expected Upsert, got {other:?}"),
        }
    }

    fn category_row(id: &str, sort_order: i64, is_untracked: bool) -> TaskCategory {
        TaskCategory {
            id: id.to_string(),
            user_id: "u-1".to_string(),
            list_id: None,
            parent_id: None,
            title: id.to_string(),
            slug: id.to_string(),
            color: "#4285f4".to_string(),
            is_productive: false,
            google_calendar_id: None,
            sort_order,
            is_untracked,
            created_at: "2026-08-18T00:00:00Z".to_string(),
            updated_at: "2026-08-18T00:00:00Z".to_string(),
            deleted_at: None,
        }
    }

    fn pattern_row(category_id: &str, regex: &str) -> crate::models::TaskCategoryPattern {
        crate::models::TaskCategoryPattern {
            id: format!("p-{category_id}-{regex}"),
            category_id: category_id.to_string(),
            regex: regex.to_string(),
            google_calendar_id: None,
            sort_order: 0,
            created_at: "2026-08-18T00:00:00Z".to_string(),
            updated_at: "2026-08-18T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn whitespace_only_recurring_event_id_is_ordinary_cancel() {
        let event = GoogleEvent {
            id: "gone".into(),
            etag: None,
            updated: None,
            status: Some("cancelled".into()),
            summary: None,
            description: None,
            recurrence: None,
            organizer: None,
            event_type: None,
            start: None,
            end: None,
            extended_properties: None,
            ical_uid: None,
            sequence: None,
            recurring_event_id: Some("   ".into()),
            original_start_time: None,
        };
        match classify_replica_item(&event, "cal-1", "2026-08-17T12:00:00Z", &EventClassifyCtx::none()) {
            ReplicaApplyAction::SoftDelete { google_event_id } => {
                assert_eq!(google_event_id, "gone");
            }
            other => panic!("expected SoftDelete, got {other:?}"),
        }
    }
}
