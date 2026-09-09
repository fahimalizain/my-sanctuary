use super::apply::{map_google_event, row_from_new_event, GoogleEvent, GoogleEventSharedProperties};
use super::google::encode_path_segment;
use super::labels::CachedEventLabel;
use super::{CalendarError, GOOGLE_EVENTS_BASE_URL};
use crate::google_color::snap_to_event_label_hex;
use crate::models::{CalendarEvent, GoogleCalendar, NewEventInput, PatchEventFields};
use crate::oauth::HttpClient;
use crate::repo::{CalendarEventRepo, CalendarRepo};
use crate::time::unix_secs_to_rfc3339;
use crate::token::GoogleAccess;

/// Result of [`create_event`]: the created event plus the response source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateEventOutput {
    pub event: CalendarEvent,
    pub source: String,
    /// Set when the local cache upsert failed (logged, never fatal).
    pub cache_error: Option<String>,
}

/// Builds `extendedProperties.shared` for an `events.insert`.
///
/// Returns `None` when there is no carrier — hand-created events send no
/// extendedProperties at all. A **task** carrier (`task_id`) is always
/// `sanctuary_task_id` (plus `sanctuary_focus` `"1"` only if focused, never
/// `"0"`, and the priority/difficulty snapshots only when non-empty). An
/// **occurrence** carrier (both `routine_id` and `occurrence_id` present)
/// sends exactly `sanctuary_routine_id` + `sanctuary_occurrence_id` — no
/// task_id, no focus/priority/difficulty (focus stays task-only). Never a
/// partial map without a carrier, and never both carriers at once.
pub(crate) fn build_shared_properties(input: &NewEventInput) -> Option<GoogleEventSharedProperties> {
    let trim_opt = |value: &Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    if let Some(task_id) = input.task_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return Some(GoogleEventSharedProperties {
            sanctuary_task_id: Some(task_id.to_string()),
            sanctuary_focus: input.sanctuary_focus.then(|| "1".to_string()),
            sanctuary_priority: trim_opt(&input.priority),
            sanctuary_difficulty: trim_opt(&input.difficulty),
            sanctuary_routine_id: None,
            sanctuary_occurrence_id: None,
        });
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
        (Some(routine_id), Some(occurrence_id)) => Some(GoogleEventSharedProperties {
            sanctuary_task_id: None,
            sanctuary_focus: None,
            sanctuary_priority: None,
            sanctuary_difficulty: None,
            sanctuary_routine_id: Some(routine_id.to_string()),
            sanctuary_occurrence_id: Some(occurrence_id.to_string()),
        }),
        _ => None,
    }
}

/// Creates an event on Google (`events.insert`) and upserts the returned row
/// into the local cache. A cache failure is logged (returned in
/// [`CreateEventOutput::cache_error`]), never fatal.
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
/// started occurrence's one-shot log), the shared map carries exactly
/// `sanctuary_routine_id` + `sanctuary_occurrence_id`; `calendar_events.task_id`
/// stays empty (occurrence events are resolved through the occurrence row,
/// never through the task column). `task_id` and the occurrence pair are
/// mutually exclusive at the call sites.
pub async fn create_event(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    access: &GoogleAccess,
    input: &NewEventInput,
    now_unix: i64,
) -> Result<CreateEventOutput, CalendarError> {
    let Some(cal) = calendars.get_by_id(&input.calendar_id).await? else {
        return Err(CalendarError::NotFound);
    };

    let url = format!(
        "{GOOGLE_EVENTS_BASE_URL}/{}/events",
        encode_path_segment(&cal.google_calendar_id)
    );
    let mut payload = serde_json::json!({
        "summary": input.summary,
        "description": input.description,
        "start": { "dateTime": input.start },
        "end": { "dateTime": input.end },
    });
    if let Some(shared) = build_shared_properties(input) {
        payload["extendedProperties"] = serde_json::json!({ "shared": shared });
    }
    // Category color → Google event label: snap the caller's hex onto the 24
    // event-label palette (chroma-first, persisted nowhere), then resolve the
    // label's id against the calendar's CACHED `event_labels`. A cache miss
    // or a missing match fails the start (400) — never a `calendars.get` and
    // never a label write. `colorId` is never sent. Hand-created events
    // (`None` / blank after trim) stay uncolored: no `eventLabelId`, and the
    // POST URL stays without `eventLabelVersion`.
    let url = match input.color_hex.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(color_hex) => {
            let snapped = snap_to_event_label_hex(color_hex)
                .map_err(|err| CalendarError::Invalid(err.to_string()))?;
            if cal.event_labels.is_empty() {
                return Err(CalendarError::Invalid(
                    "calendar event-label cache is empty".to_string(),
                ));
            }
            let labels: Vec<CachedEventLabel> = serde_json::from_str(&cal.event_labels)
                .map_err(|err| {
                    CalendarError::Invalid(format!("invalid calendar event-label cache: {err}"))
                })?;
            let label_id = labels
                .iter()
                .find(|label| label.background_color.eq_ignore_ascii_case(&snapped))
                .map(|label| label.id.clone())
                .ok_or_else(|| {
                    CalendarError::Invalid("no event label matches category color".to_string())
                })?;
            payload["eventLabelId"] = serde_json::json!(label_id);
            format!(
                "{GOOGLE_EVENTS_BASE_URL}/{}/events?eventLabelVersion=1",
                encode_path_segment(&cal.google_calendar_id)
            )
        }
        None => url,
    };
    let body =
        serde_json::to_vec(&payload).map_err(|err| CalendarError::InvalidResponse(err.to_string()))?;
    let (status, response) = http.post_json(&url, &access.access_token, &body).await?;
    if !(200..300).contains(&status) {
        return Err(CalendarError::GoogleApi(format!(
            "google events.insert returned {status}"
        )));
    }
    let created: GoogleEvent = serde_json::from_slice(&response)
        .map_err(|err| CalendarError::InvalidResponse(format!("events.insert body: {err}")))?;

    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);
    let new_event = map_google_event(&created, &cal.id, &now_rfc3339);
    let id = match events.upsert(new_event.clone(), &now_rfc3339).await {
        Ok(id) => id,
        Err(err) => {
            return Ok(CreateEventOutput {
                event: row_from_new_event(new_event, "".to_string(), &now_rfc3339),
                source: "google".to_string(),
                cache_error: Some(err.to_string()),
            });
        }
    };

    Ok(CreateEventOutput {
        event: row_from_new_event(new_event, id, &now_rfc3339),
        source: "google".to_string(),
        cache_error: None,
    })
}

/// Patches selected fields on Google (`events.patch`) and upserts the
/// returned row into the local cache. Builds a minimal JSON body from
/// whichever of `start` / `end` / `summary` are `Some`. Empty (all `None`)
/// → [`CalendarError::Invalid`]. Cache failures are logged
/// ([`CreateEventOutput::cache_error`]), never fatal.
pub async fn patch_event_fields(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    access: &GoogleAccess,
    calendar_id: &str,
    google_event_id: &str,
    fields: &PatchEventFields,
    now_unix: i64,
) -> Result<CreateEventOutput, CalendarError> {
    let mut payload = serde_json::Map::new();
    if let Some(start) = fields.start.as_ref() {
        payload.insert(
            "start".to_string(),
            serde_json::json!({ "dateTime": start }),
        );
    }
    if let Some(end) = fields.end.as_ref() {
        payload.insert(
            "end".to_string(),
            serde_json::json!({ "dateTime": end }),
        );
    }
    if let Some(summary) = fields.summary.as_ref() {
        payload.insert("summary".to_string(), serde_json::json!(summary));
    }
    if payload.is_empty() {
        return Err(CalendarError::Invalid("empty patch".to_string()));
    }

    let Some(cal) = calendars.get_by_id(calendar_id).await? else {
        return Err(CalendarError::NotFound);
    };

    let url = format!(
        "{GOOGLE_EVENTS_BASE_URL}/{}/events/{}",
        encode_path_segment(&cal.google_calendar_id),
        encode_path_segment(google_event_id)
    );
    let body = serde_json::to_vec(&serde_json::Value::Object(payload))
        .map_err(|err| CalendarError::InvalidResponse(err.to_string()))?;
    let (status, response) = http.patch_json(&url, &access.access_token, &body).await?;
    if !(200..300).contains(&status) {
        return Err(CalendarError::GoogleApi(format!(
            "google events.patch returned {status}"
        )));
    }
    let patched: GoogleEvent = serde_json::from_slice(&response)
        .map_err(|err| CalendarError::InvalidResponse(format!("events.patch body: {err}")))?;

    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);
    let new_event = map_google_event(&patched, &cal.id, &now_rfc3339);
    let id = match events.upsert(new_event.clone(), &now_rfc3339).await {
        Ok(id) => id,
        Err(err) => {
            return Ok(CreateEventOutput {
                event: row_from_new_event(new_event, "".to_string(), &now_rfc3339),
                source: "google".to_string(),
                cache_error: Some(err.to_string()),
            });
        }
    };

    Ok(CreateEventOutput {
        event: row_from_new_event(new_event, id, &now_rfc3339),
        source: "google".to_string(),
        cache_error: None,
    })
}

/// Patches an event's `end` on Google — the task timer's stop/pause path.
/// Thin wrapper around [`patch_event_fields`].
pub async fn patch_event(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
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
        access,
        calendar_id,
        google_event_id,
        &PatchEventFields {
            start: None,
            end: Some(end_rfc3339.to_string()),
            summary: None,
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
        access,
        calendar_id,
        google_event_id,
        &PatchEventFields {
            start: None,
            end: None,
            summary: Some(summary.to_string()),
        },
        now_unix,
    )
    .await
}

/// Looks up a local event by id, verifies the owning calendar belongs to
/// `user_id`, then patches via [`patch_event_fields`]. Wrong owner / missing
/// → [`CalendarError::NotFound`] (no Google call).
pub async fn update_event_for_user(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    access: &GoogleAccess,
    user_id: &str,
    event_id: &str,
    fields: &PatchEventFields,
    now_unix: i64,
) -> Result<CreateEventOutput, CalendarError> {
    let (cal, event) = lookup_owned_event(calendars, events, user_id, event_id).await?;
    patch_event_fields(
        http,
        calendars,
        events,
        access,
        &cal.id,
        &event.google_event_id,
        fields,
        now_unix,
    )
    .await
}

/// Cancels an event on Google (`events.patch` with `status: "cancelled"`)
/// and soft-deletes the local cache row. Google 404/410 still soft-deletes
/// locally (already gone). Does **not** use HTTP DELETE — reuses
/// [`HttpClient::patch_json`] so fakes across tasks/agenda stay unchanged.
pub async fn delete_event(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    access: &GoogleAccess,
    calendar_id: &str,
    google_event_id: &str,
    local_id: &str,
    now_unix: i64,
) -> Result<(), CalendarError> {
    let Some(cal) = calendars.get_by_id(calendar_id).await? else {
        return Err(CalendarError::NotFound);
    };

    let url = format!(
        "{GOOGLE_EVENTS_BASE_URL}/{}/events/{}",
        encode_path_segment(&cal.google_calendar_id),
        encode_path_segment(google_event_id)
    );
    let payload = serde_json::json!({ "status": "cancelled" });
    let body =
        serde_json::to_vec(&payload).map_err(|err| CalendarError::InvalidResponse(err.to_string()))?;
    let (status, _response) = http.patch_json(&url, &access.access_token, &body).await?;
    // 404/410 = already gone on Google; still soft-delete locally.
    if status != 404 && status != 410 && !(200..300).contains(&status) {
        return Err(CalendarError::GoogleApi(format!(
            "google events.patch (cancel) returned {status}"
        )));
    }

    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);
    events.delete(local_id, &now_rfc3339).await?;
    Ok(())
}

/// Looks up a local event by id, verifies ownership, then cancels via
/// [`delete_event`]. Wrong owner / missing → [`CalendarError::NotFound`].
pub async fn delete_event_for_user(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
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
