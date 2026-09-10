use super::CalendarError;
use crate::models::{CalendarEvent, GoogleCalendar, NewEventInput, PatchEventFields};
use crate::oauth::HttpClient;
use crate::repo::{CalendarEventOperationRepo, CalendarEventRepo, CalendarRepo};
use crate::token::GoogleAccess;

/// Result of [`create_event`]: the created event plus the response source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateEventOutput {
    pub event: CalendarEvent,
    pub source: String,
    /// Set when the local cache upsert failed on **patch** historically
    /// (logged, never fatal). Journaled create/patch/delete never return
    /// `Ok` with this set — a cache miss after Google commit is
    /// [`CalendarError::Repo`].
    pub cache_error: Option<String>,
}

/// Builds `extendedProperties.shared` for an `events.insert`.
///
/// Always includes `sanctuary_event_id` (the client-supplied Google event id).
/// A **task** carrier (`task_id`) adds `sanctuary_task_id` (plus
/// `sanctuary_focus` `"1"` only if focused, never `"0"`, and the
/// priority/difficulty snapshots only when non-empty). An **occurrence**
/// carrier (both `routine_id` and `occurrence_id` present) adds
/// `sanctuary_routine_id` + `sanctuary_occurrence_id` — no task_id, no
/// focus/priority/difficulty (focus stays task-only). Hand-created events
/// (no carrier) send **only** `sanctuary_event_id`. Never both carriers at
/// once; never a partial occurrence pair.
pub(crate) fn build_shared_properties(
    input: &NewEventInput,
    sanctuary_event_id: &str,
) -> super::apply::GoogleEventSharedProperties {
    use super::apply::GoogleEventSharedProperties;
    let trim_opt = |value: &Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let base = GoogleEventSharedProperties {
        sanctuary_event_id: Some(sanctuary_event_id.to_string()),
        ..Default::default()
    };
    if let Some(task_id) = input.task_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return GoogleEventSharedProperties {
            sanctuary_event_id: base.sanctuary_event_id,
            sanctuary_task_id: Some(task_id.to_string()),
            sanctuary_focus: input.sanctuary_focus.then(|| "1".to_string()),
            sanctuary_priority: trim_opt(&input.priority),
            sanctuary_difficulty: trim_opt(&input.difficulty),
            sanctuary_routine_id: None,
            sanctuary_occurrence_id: None,
        };
    }
    // Occurrence carrier: BOTH ids must be present (a partial pair is a
    // caller bug — never emit a partial map).
    let routine_id = input.routine_id.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let occurrence_id = input
        .occurrence_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match (routine_id, occurrence_id) {
        (Some(routine_id), Some(occurrence_id)) => GoogleEventSharedProperties {
            sanctuary_event_id: base.sanctuary_event_id,
            sanctuary_task_id: None,
            sanctuary_focus: None,
            sanctuary_priority: None,
            sanctuary_difficulty: None,
            sanctuary_routine_id: Some(routine_id.to_string()),
            sanctuary_occurrence_id: Some(occurrence_id.to_string()),
        },
        _ => base,
    }
}

/// Creates an event on Google (`events.insert`) via the outbound operation
/// journal (issue #50 / Vertical 4).
///
/// Journals a `pending` row **before** the Google call, mints a
/// client-supplied event id once (reused on 409 → GET), stamps
/// `sanctuary_event_id` on every insert, and returns the V2 persisted local
/// id after cache apply. A cache failure after Google commit is
/// [`CalendarError::Repo`] (status stays `google_committed`); create never
/// returns `Ok` with [`CreateEventOutput::cache_error`] set.
///
/// When `input.task_id` is set, the payload carries
/// `extendedProperties.shared.sanctuary_task_id` — the task timer's carrier
/// (slice 4); the sync path maps it back onto `calendar_events.task_id`. When
/// both `task_id` and `sanctuary_focus` are set (a focus segment, slice 3),
/// the shared map also carries `sanctuary_focus = "1"` — always with the
/// carrier, never a partial shared map; `sanctuary_focus` is never sent
/// without the carrier and never as `"0"`. `sanctuary_priority` and
/// `sanctuary_difficulty` are create-time snapshots of the task's values,
/// stamped next to the carrier at insert time only — they are never patched
/// when the task later changes. Unfocused creates send the carrier (plus any
/// snapshots) alone.
///
/// When instead `routine_id` AND `occurrence_id` are both set (slice 6 — a
/// started occurrence's one-shot log), the shared map carries
/// `sanctuary_routine_id` + `sanctuary_occurrence_id` (plus
/// `sanctuary_event_id`); `calendar_events.task_id` stays empty. `task_id`
/// and the occurrence pair are mutually exclusive at the call sites.
pub async fn create_event(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    operations: &dyn CalendarEventOperationRepo,
    access: &GoogleAccess,
    input: &NewEventInput,
    now_unix: i64,
) -> Result<CreateEventOutput, CalendarError> {
    super::journal::create_event_with_journal(
        http,
        calendars,
        events,
        operations,
        access,
        input,
        now_unix,
    )
    .await
}

/// Patches selected fields on Google (`events.patch`) via the outbound
/// operation journal (issue #50 / Vertical 4).
///
/// **Minimal body only:** `start.dateTime` / `end.dateTime` / `summary` /
/// `description` — whichever of `fields` are `Some` (`Some("")` clears
/// description). This is **not** `events.update` (full replace). Arrays
/// replace on patch, so we deliberately never send attendees, conferenceData,
/// extendedProperties, or recurrence — those stay on Google untouched.
/// Empty (all `None`) → [`CalendarError::Invalid`] before any journal row.
///
/// Journals `pending` before Google, sends `If-Match` with the stored
/// `google_etag` (or GETs one first when empty), retries 412 up to three
/// PATCH attempts with the **same** minimal payload + fresh etag, then
/// marks the operation `conflict` ([`CalendarError::Conflict`]) without
/// disabling the calendar. Cache failure after Google 2xx is
/// [`CalendarError::Repo`] (status stays `google_committed`).
pub async fn patch_event_fields(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    operations: &dyn CalendarEventOperationRepo,
    access: &GoogleAccess,
    calendar_id: &str,
    google_event_id: &str,
    fields: &PatchEventFields,
    now_unix: i64,
) -> Result<CreateEventOutput, CalendarError> {
    super::write_journal::patch_event_fields_with_journal(
        http,
        calendars,
        events,
        operations,
        access,
        calendar_id,
        google_event_id,
        fields,
        now_unix,
    )
    .await
}

/// Patches an event's `end` on Google — the task timer's stop/pause path.
/// Thin wrapper around [`patch_event_fields`].
pub async fn patch_event(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    operations: &dyn CalendarEventOperationRepo,
    access: &GoogleAccess,
    calendar_id: &str,
    google_event_id: &str,
    end_rfc3339: &str,
    now_unix: i64,
) -> Result<CreateEventOutput, CalendarError> {
    patch_event_fields(
        http,
        calendars,
        events,
        operations,
        access,
        calendar_id,
        google_event_id,
        &PatchEventFields {
            start: None,
            end: Some(end_rfc3339.to_string()),
            summary: None,
            description: None,
            calendar_id: None,
        },
        now_unix,
    )
    .await
}

/// Patches an event's `summary` on Google — the occurrence title PATCH's
/// Google write (slice 6). Thin wrapper around [`patch_event_fields`].
pub async fn patch_event_summary(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    operations: &dyn CalendarEventOperationRepo,
    access: &GoogleAccess,
    calendar_id: &str,
    google_event_id: &str,
    summary: &str,
    now_unix: i64,
) -> Result<CreateEventOutput, CalendarError> {
    patch_event_fields(
        http,
        calendars,
        events,
        operations,
        access,
        calendar_id,
        google_event_id,
        &PatchEventFields {
            start: None,
            end: None,
            summary: Some(summary.to_string()),
            description: None,
            calendar_id: None,
        },
        now_unix,
    )
    .await
}

/// Looks up a local event by id, verifies the owning calendar belongs to
/// `user_id`, then either:
/// - moves the event (`fields.calendar_id` only → Google `events.move`), or
/// - patches fields via [`patch_event_fields`].
///
/// `calendar_id` is exclusive and must not be combined with start/end/summary/
/// description. Wrong owner / missing → [`CalendarError::NotFound`] (no Google
/// call, no journal).
pub async fn update_event_for_user(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    operations: &dyn CalendarEventOperationRepo,
    access: &GoogleAccess,
    user_id: &str,
    event_id: &str,
    fields: &PatchEventFields,
    now_unix: i64,
) -> Result<CreateEventOutput, CalendarError> {
    let has_other = fields.start.is_some()
        || fields.end.is_some()
        || fields.summary.is_some()
        || fields.description.is_some();

    if let Some(dest_local_id) = fields.calendar_id.as_deref() {
        let dest_local_id = dest_local_id.trim();
        if dest_local_id.is_empty() {
            return Err(CalendarError::Invalid(
                "calendar_id must not be empty".to_string(),
            ));
        }
        if has_other {
            return Err(CalendarError::Invalid(
                "calendar_id cannot be combined with start, end, summary, or description"
                    .to_string(),
            ));
        }
        let (source, event) = lookup_owned_event(calendars, events, user_id, event_id).await?;
        return super::write_journal::move_event_with_journal(
            http,
            calendars,
            events,
            operations,
            access,
            &source,
            &event.id,
            &event.google_event_id,
            dest_local_id,
            now_unix,
        )
        .await;
    }

    let (cal, event) = lookup_owned_event(calendars, events, user_id, event_id).await?;
    patch_event_fields(
        http,
        calendars,
        events,
        operations,
        access,
        &cal.id,
        &event.google_event_id,
        fields,
        now_unix,
    )
    .await
}

/// Cancels an event on Google (`events.patch` with `status: "cancelled"`)
/// via the outbound journal and soft-deletes the local cache row.
///
/// Sends `If-Match` when an etag is known (same 412 retry cap as patch).
/// Google 404/410 still soft-deletes locally (already gone) and marks the
/// operation `cache_applied`. Does **not** use HTTP DELETE.
pub async fn delete_event(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    operations: &dyn CalendarEventOperationRepo,
    access: &GoogleAccess,
    calendar_id: &str,
    google_event_id: &str,
    local_id: &str,
    now_unix: i64,
) -> Result<(), CalendarError> {
    super::write_journal::delete_event_with_journal(
        http,
        calendars,
        events,
        operations,
        access,
        calendar_id,
        google_event_id,
        local_id,
        now_unix,
    )
    .await
}

/// Looks up a local event by id, verifies ownership, then cancels via
/// [`delete_event`]. Wrong owner / missing → [`CalendarError::NotFound`].
pub async fn delete_event_for_user(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    operations: &dyn CalendarEventOperationRepo,
    access: &GoogleAccess,
    user_id: &str,
    event_id: &str,
    now_unix: i64,
) -> Result<(), CalendarError> {
    let (cal, event) = lookup_owned_event(calendars, events, user_id, event_id).await?;
    delete_event(
        http,
        calendars,
        events,
        operations,
        access,
        &cal.id,
        &event.google_event_id,
        &event.id,
        now_unix,
    )
    .await
}

/// Resolve `(calendar, event)` for a local event id owned by `user_id`.
/// Missing event, missing calendar, or wrong owner → NotFound (no leak).
async fn lookup_owned_event(
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    user_id: &str,
    event_id: &str,
) -> Result<(GoogleCalendar, CalendarEvent), CalendarError> {
    let Some(event) = events.get_by_id(event_id).await? else {
        return Err(CalendarError::NotFound);
    };
    let Some(cal) = calendars.get_by_id(&event.calendar_id).await? else {
        return Err(CalendarError::NotFound);
    };
    if cal.user_id != user_id {
        return Err(CalendarError::NotFound);
    }
    Ok((cal, event))
}
