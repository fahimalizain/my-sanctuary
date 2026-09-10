//! Journaled patch/delete with If-Match / 412 retry (issue #50 / Vertical 4).
//!
//! Minimal `events.patch` bodies only (start/end/summary/description or
//! status:cancelled). Journals `pending` before Google, preconditions on the
//! stored etag, retries 412 up to [`IF_MATCH_MAX_ATTEMPTS`] with the same
//! payload + fresh etag, then marks the operation `conflict` without disabling
//! the calendar.

use super::apply::{map_google_event, row_from_new_event};
use super::google::encode_path_segment;
use super::journal::{parse_google_event, payload_fingerprint};
use super::write::CreateEventOutput;
use super::{CalendarError, GOOGLE_EVENTS_BASE_URL};
use crate::models::{
    GoogleCalendar, NewCalendarEventOperation, PatchEventFields, OP_STATUS_CACHE_APPLIED,
    OP_STATUS_CONFLICT, OP_STATUS_FAILED, OP_STATUS_GOOGLE_COMMITTED, OP_STATUS_PENDING,
    OP_VERB_DELETE, OP_VERB_MOVE, OP_VERB_PATCH,
};
use crate::oauth::HttpClient;
use crate::repo::{CalendarEventOperationRepo, CalendarEventRepo, CalendarRepo};
use crate::time::unix_secs_to_rfc3339;
use crate::token::GoogleAccess;

/// Max PATCH HTTP calls per journaled patch/delete (If-Match). After this
/// many 412s the operation is marked `conflict` — no fourth PATCH.
pub(crate) const IF_MATCH_MAX_ATTEMPTS: u32 = 3;

/// Builds the minimal `events.patch` JSON object (only present fields).
/// Empty map → caller returns [`CalendarError::Invalid`] before journal.
/// `description: Some("")` is emitted as `""` so Google notes can be cleared.
///
/// `calendar_id` is intentionally omitted — calendar moves use Google
/// `events.move` via [`move_event_with_journal`], not `events.patch`.
pub(crate) fn build_patch_payload(fields: &PatchEventFields) -> serde_json::Map<String, serde_json::Value> {
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
    if let Some(description) = fields.description.as_ref() {
        payload.insert("description".to_string(), serde_json::json!(description));
    }
    payload
}

fn is_writable_role(access_role: &str) -> bool {
    access_role == "owner" || access_role == "writer"
}

/// Journaled Google `events.move` — reassigns the replica row in place
/// (same local id, new `calendar_id`).
///
/// Journal `calendar_id` stays the **source** calendar. Payload is
/// `{"destination":"<dest google_calendar_id>"}`.
pub(crate) async fn move_event_with_journal(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    operations: &dyn CalendarEventOperationRepo,
    access: &GoogleAccess,
    source: &GoogleCalendar,
    event_local_id: &str,
    google_event_id: &str,
    dest_local_id: &str,
    now_unix: i64,
) -> Result<CreateEventOutput, CalendarError> {
    if !is_writable_role(&source.access_role) {
        return Err(CalendarError::Invalid(
            "source calendar is not writable".to_string(),
        ));
    }

    let Some(dest) = calendars.get_by_id(dest_local_id).await? else {
        return Err(CalendarError::NotFound);
    };
    if dest.user_id != source.user_id {
        return Err(CalendarError::NotFound);
    }
    if !is_writable_role(&dest.access_role) {
        return Err(CalendarError::Invalid(
            "destination calendar is not writable".to_string(),
        ));
    }

    // Idempotent same-calendar move: return current row, no journal/HTTP.
    if dest.id == source.id {
        let Some(current) = events.get_by_id(event_local_id).await? else {
            return Err(CalendarError::NotFound);
        };
        return Ok(CreateEventOutput {
            event: current,
            source: "cache".to_string(),
            cache_error: None,
        });
    }

    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);
    let payload = serde_json::json!({ "destination": dest.google_calendar_id });
    let body = serde_json::to_vec(&payload)
        .map_err(|err| CalendarError::InvalidResponse(err.to_string()))?;

    let op_id = insert_pending_op(
        operations,
        source,
        event_local_id,
        google_event_id,
        OP_VERB_MOVE,
        &body,
        "",
        &now_rfc3339,
    )
    .await?;

    let url = format!(
        "{GOOGLE_EVENTS_BASE_URL}/{}/events/{}/move?destination={}",
        encode_path_segment(&source.google_calendar_id),
        encode_path_segment(google_event_id),
        encode_path_segment(&dest.google_calendar_id),
    );

    // Empty JSON body — destination is only in the query string.
    let (status, response_bytes) = match http
        .post_json(&url, &access.access_token, b"{}")
        .await
    {
        Ok(pair) => pair,
        Err(err) => {
            let err = CalendarError::from(err);
            mark_failed(operations, &op_id, &err, &now_rfc3339).await;
            return Err(err);
        }
    };

    if !(200..300).contains(&status) {
        let err = if status == 404 || status == 410 {
            CalendarError::GoogleNotFound
        } else {
            CalendarError::GoogleApi(format!("google events.move returned {status}"))
        };
        mark_failed(operations, &op_id, &err, &now_rfc3339).await;
        return Err(err);
    }

    let moved = match parse_google_event(&response_bytes, "events.move") {
        Ok(ev) => ev,
        Err(err) => {
            mark_failed(operations, &op_id, &err, &now_rfc3339).await;
            return Err(err);
        }
    };
    let moved_google_id = moved.id.clone();
    let etag = moved.etag.clone().unwrap_or_default();

    operations
        .update_progress(
            &op_id,
            OP_STATUS_GOOGLE_COMMITTED,
            &moved_google_id,
            event_local_id,
            &etag,
            "",
            false,
            &now_rfc3339,
        )
        .await?;

    if let Err(err) = events
        .reassign_calendar(event_local_id, &dest.id, &now_rfc3339)
        .await
    {
        return Err(CalendarError::Repo(err));
    }

    let new_event = map_google_event(&moved, &dest.id, &now_rfc3339);
    let id = match events.upsert(new_event.clone(), &now_rfc3339).await {
        Ok(id) => id,
        Err(err) => {
            // Leave google_committed — repair will finish.
            return Err(CalendarError::Repo(err));
        }
    };

    operations
        .update_progress(
            &op_id,
            OP_STATUS_CACHE_APPLIED,
            &moved_google_id,
            &id,
            &etag,
            "",
            false,
            &now_rfc3339,
        )
        .await?;

    let _ = calendars.bump_dirty_requested(&source.id, &now_rfc3339).await;
    let _ = calendars.bump_dirty_requested(&dest.id, &now_rfc3339).await;

    Ok(CreateEventOutput {
        event: row_from_new_event(new_event, id, &now_rfc3339),
        source: "google".to_string(),
        cache_error: None,
    })
}

/// Journaled `events.patch` with If-Match / 412 retry (issue #50 / V4).
pub(crate) async fn patch_event_fields_with_journal(
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
    let payload = build_patch_payload(fields);
    if payload.is_empty() {
        return Err(CalendarError::Invalid("empty patch".to_string()));
    }

    let Some(cal) = calendars.get_by_id(calendar_id).await? else {
        return Err(CalendarError::NotFound);
    };

    let body = serde_json::to_vec(&serde_json::Value::Object(payload))
        .map_err(|err| CalendarError::InvalidResponse(err.to_string()))?;
    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);

    let stored = events
        .get_by_calendar_and_google_id(calendar_id, google_event_id)
        .await?;
    let local_event_id = stored
        .as_ref()
        .map(|e| e.id.clone())
        .unwrap_or_default();
    let mut etag = stored
        .as_ref()
        .map(|e| e.google_etag.clone())
        .unwrap_or_default();

    let op_id = insert_pending_op(
        operations,
        &cal,
        &local_event_id,
        google_event_id,
        OP_VERB_PATCH,
        &body,
        &etag,
        &now_rfc3339,
    )
    .await?;

    let url = event_url(&cal.google_calendar_id, google_event_id);

    if etag.is_empty() {
        match fetch_etag(http, access, &url, "events.get before patch").await {
            Ok(fresh) => etag = fresh,
            Err(err) => {
                mark_failed(operations, &op_id, &err, &now_rfc3339).await;
                return Err(err);
            }
        }
    }

    let response_bytes = match patch_with_if_match(
        http,
        access,
        operations,
        &op_id,
        google_event_id,
        &local_event_id,
        &url,
        &body,
        &mut etag,
        false,
        &now_rfc3339,
    )
    .await
    {
        Ok(bytes) => bytes,
        Err(err) => return Err(err),
    };

    let patched = match parse_google_event(&response_bytes, "events.patch") {
        Ok(ev) => ev,
        Err(err) => {
            mark_failed(operations, &op_id, &err, &now_rfc3339).await;
            return Err(err);
        }
    };
    let etag = patched.etag.clone().unwrap_or(etag);

    operations
        .update_progress(
            &op_id,
            OP_STATUS_GOOGLE_COMMITTED,
            google_event_id,
            &local_event_id,
            &etag,
            "",
            false,
            &now_rfc3339,
        )
        .await?;

    let new_event = map_google_event(&patched, &cal.id, &now_rfc3339);
    let id = match events.upsert(new_event.clone(), &now_rfc3339).await {
        Ok(id) => id,
        Err(err) => {
            // Leave google_committed — repair (slice 4) will finish.
            return Err(CalendarError::Repo(err));
        }
    };

    operations
        .update_progress(
            &op_id,
            OP_STATUS_CACHE_APPLIED,
            google_event_id,
            &id,
            &etag,
            "",
            false,
            &now_rfc3339,
        )
        .await?;

    let _ = calendars.bump_dirty_requested(&cal.id, &now_rfc3339).await;

    Ok(CreateEventOutput {
        event: row_from_new_event(new_event, id, &now_rfc3339),
        source: "google".to_string(),
        cache_error: None,
    })
}

/// Journaled cancel (`events.patch` with `status: cancelled`) + local soft-delete.
pub(crate) async fn delete_event_with_journal(
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
    let Some(cal) = calendars.get_by_id(calendar_id).await? else {
        return Err(CalendarError::NotFound);
    };

    let payload = serde_json::json!({ "status": "cancelled" });
    let body =
        serde_json::to_vec(&payload).map_err(|err| CalendarError::InvalidResponse(err.to_string()))?;
    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);

    let stored = events
        .get_by_calendar_and_google_id(calendar_id, google_event_id)
        .await?;
    let mut etag = stored
        .as_ref()
        .map(|e| e.google_etag.clone())
        .unwrap_or_default();
    // Prefer the caller-supplied local id; fall back to cache lookup.
    let local_event_id = if local_id.is_empty() {
        stored
            .as_ref()
            .map(|e| e.id.clone())
            .unwrap_or_default()
    } else {
        local_id.to_string()
    };

    let op_id = insert_pending_op(
        operations,
        &cal,
        &local_event_id,
        google_event_id,
        OP_VERB_DELETE,
        &body,
        &etag,
        &now_rfc3339,
    )
    .await?;

    let url = event_url(&cal.google_calendar_id, google_event_id);

    // Empty stored etag (pre-V1 rows): bootstrap via GET. 404/410 means
    // Google already dropped the event — same as PATCH 404/410: skip cancel
    // and still local-delete. Other GET failures stay failed + GoogleApi.
    let mut google_already_gone = false;
    if etag.is_empty() {
        let (status, body_bytes) = match http
            .get_bearer_raw(&url, &access.access_token)
            .await
        {
            Ok(pair) => pair,
            Err(err) => {
                let err = CalendarError::from(err);
                mark_failed(operations, &op_id, &err, &now_rfc3339).await;
                return Err(err);
            }
        };
        if status == 404 || status == 410 {
            google_already_gone = true;
        } else if !(200..300).contains(&status) {
            let err = CalendarError::GoogleApi(format!(
                "google events.get before delete returned {status}"
            ));
            mark_failed(operations, &op_id, &err, &now_rfc3339).await;
            return Err(err);
        } else {
            match parse_google_event(&body_bytes, "events.get before delete") {
                Ok(ev) => etag = ev.etag.unwrap_or_default(),
                Err(err) => {
                    mark_failed(operations, &op_id, &err, &now_rfc3339).await;
                    return Err(err);
                }
            }
        }
    }

    if !google_already_gone {
        match patch_with_if_match(
            http,
            access,
            operations,
            &op_id,
            google_event_id,
            &local_event_id,
            &url,
            &body,
            &mut etag,
            true, // delete: 404/410 = already gone
            &now_rfc3339,
        )
        .await
        {
            Ok(_) => {}
            Err(err) => return Err(err),
        }
    }

    operations
        .update_progress(
            &op_id,
            OP_STATUS_GOOGLE_COMMITTED,
            google_event_id,
            &local_event_id,
            &etag,
            "",
            false,
            &now_rfc3339,
        )
        .await?;

    if let Err(err) = events.delete(&local_event_id, &now_rfc3339).await {
        return Err(CalendarError::Repo(err));
    }

    operations
        .update_progress(
            &op_id,
            OP_STATUS_CACHE_APPLIED,
            google_event_id,
            &local_event_id,
            &etag,
            "",
            false,
            &now_rfc3339,
        )
        .await?;

    let _ = calendars.bump_dirty_requested(&cal.id, &now_rfc3339).await;
    Ok(())
}

fn event_url(google_calendar_id: &str, google_event_id: &str) -> String {
    format!(
        "{GOOGLE_EVENTS_BASE_URL}/{}/events/{}",
        encode_path_segment(google_calendar_id),
        encode_path_segment(google_event_id)
    )
}

#[allow(clippy::too_many_arguments)]
async fn insert_pending_op(
    operations: &dyn CalendarEventOperationRepo,
    cal: &GoogleCalendar,
    local_event_id: &str,
    google_event_id: &str,
    verb: &str,
    body: &[u8],
    etag: &str,
    now_rfc3339: &str,
) -> Result<String, CalendarError> {
    let fingerprint = payload_fingerprint(body);
    let payload_json = String::from_utf8(body.to_vec()).map_err(|err| {
        CalendarError::InvalidResponse(format!("events.{verb} payload utf-8: {err}"))
    })?;
    Ok(operations
        .insert(
            NewCalendarEventOperation {
                user_id: cal.user_id.clone(),
                calendar_id: cal.id.clone(),
                local_event_id: local_event_id.to_string(),
                google_event_id: google_event_id.to_string(),
                verb: verb.to_string(),
                payload_fingerprint: fingerprint,
                payload_json,
                status: OP_STATUS_PENDING.to_string(),
                google_etag: etag.to_string(),
            },
            now_rfc3339,
        )
        .await?)
}

async fn fetch_etag(
    http: &dyn HttpClient,
    access: &GoogleAccess,
    url: &str,
    context: &str,
) -> Result<String, CalendarError> {
    let (status, body) = http
        .get_bearer_raw(url, &access.access_token)
        .await?;
    if !(200..300).contains(&status) {
        return Err(CalendarError::GoogleApi(format!(
            "google {context} returned {status}"
        )));
    }
    let event = parse_google_event(&body, context)?;
    Ok(event.etag.unwrap_or_default())
}

/// PATCH loop with If-Match. Returns response body bytes on success
/// (empty vec for delete 404/410). Marks journal failed/conflict on error.
#[allow(clippy::too_many_arguments)]
async fn patch_with_if_match(
    http: &dyn HttpClient,
    access: &GoogleAccess,
    operations: &dyn CalendarEventOperationRepo,
    op_id: &str,
    google_event_id: &str,
    local_event_id: &str,
    url: &str,
    body: &[u8],
    etag: &mut String,
    delete_gone_ok: bool,
    now_rfc3339: &str,
) -> Result<Vec<u8>, CalendarError> {
    for attempt in 1..=IF_MATCH_MAX_ATTEMPTS {
        let headers: Vec<(&str, &str)> = if etag.is_empty() {
            Vec::new()
        } else {
            vec![("If-Match", etag.as_str())]
        };
        let (status, response) = http
            .patch_json_with_headers(url, &access.access_token, body, &headers)
            .await?;

        if (200..300).contains(&status) {
            return Ok(response);
        }

        if delete_gone_ok && (status == 404 || status == 410) {
            // Already gone on Google — treat as commit success.
            return Ok(response);
        }

        if status == 412 {
            let _ = operations
                .update_progress(
                    op_id,
                    OP_STATUS_PENDING,
                    google_event_id,
                    local_event_id,
                    etag,
                    "412 precondition failed",
                    true,
                    now_rfc3339,
                )
                .await;
            if attempt == IF_MATCH_MAX_ATTEMPTS {
                let msg = "if-match retries exhausted";
                let _ = operations
                    .update_status(op_id, OP_STATUS_CONFLICT, msg, now_rfc3339)
                    .await;
                return Err(CalendarError::Conflict);
            }
            // Refresh etag via GET; retry same minimal payload.
            match fetch_etag(http, access, url, "events.get after 412").await {
                Ok(fresh) if !fresh.is_empty() => *etag = fresh,
                Ok(_) => {
                    // GET ok but no etag — still retry; next If-Match may be empty.
                }
                Err(err) => {
                    mark_failed(operations, op_id, &err, now_rfc3339).await;
                    return Err(err);
                }
            }
            continue;
        }

        // Body as UTF-8 lossy for forbiddenForNonOrganizer detection only
        // (never stored raw tokens).
        let body_text = String::from_utf8_lossy(&response);
        if status == 403 && body_text.contains("forbiddenForNonOrganizer") {
            let msg = format!("google events.patch returned {status} forbiddenForNonOrganizer");
            let _ = operations
                .update_status(op_id, OP_STATUS_FAILED, &msg, now_rfc3339)
                .await;
            return Err(CalendarError::GoogleApi(msg));
        }

        let msg = format!("google events.patch returned {status}");
        let _ = operations
            .update_status(op_id, OP_STATUS_FAILED, &msg, now_rfc3339)
            .await;
        return Err(CalendarError::GoogleApi(msg));
    }

    // Unreachable: loop always returns.
    Err(CalendarError::Conflict)
}

async fn mark_failed(
    operations: &dyn CalendarEventOperationRepo,
    op_id: &str,
    err: &CalendarError,
    now_rfc3339: &str,
) {
    let msg = err.to_string();
    let _ = operations
        .update_status(op_id, OP_STATUS_FAILED, &msg, now_rfc3339)
        .await;
}

