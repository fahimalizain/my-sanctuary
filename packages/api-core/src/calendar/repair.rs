//! GET-only repair of stuck outbound calendar write journal rows
//! (`pending` / `google_committed`). Issue #50 / Vertical 4.
//!
//! Never POST/PATCH from this module. A pending insert that 404s is marked
//! `failed` (user retries with a new client id). A google_committed insert
//! whose Google row is gone is local-deleted and closed as `cache_applied`.

use super::apply::{classify_replica_item, ReplicaApplyAction};
use super::google::encode_path_segment;
use super::journal::parse_google_event;
use super::GOOGLE_EVENTS_BASE_URL;
use crate::models::{
    CalendarEventOperation, OP_STATUS_CACHE_APPLIED, OP_STATUS_FAILED, OP_STATUS_GOOGLE_COMMITTED,
    OP_STATUS_PENDING, OP_VERB_DELETE, OP_VERB_INSERT, OP_VERB_PATCH,
};
use crate::oauth::HttpClient;
use crate::repo::{CalendarEventOperationRepo, CalendarEventRepo, CalendarRepo};
use crate::time::unix_secs_to_rfc3339;
use crate::token::GoogleAccess;

/// Recover in-flight journal rows for one user by GET only.
///
/// Returns human-readable error strings (never tokens). One op failure does
/// not abort the rest. Status is left unchanged on transport / non-terminal
/// Google errors so the next tick can retry.
pub async fn repair_inflight_operations(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    operations: &dyn CalendarEventOperationRepo,
    access: &GoogleAccess,
    user_id: &str,
    now_unix: i64,
) -> Vec<String> {
    let mut errors = Vec::new();
    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);

    let ops = match operations
        .list_by_statuses(&[OP_STATUS_PENDING, OP_STATUS_GOOGLE_COMMITTED])
        .await
    {
        Ok(rows) => rows,
        Err(err) => {
            errors.push(format!("repair list_by_statuses failed for user {user_id}: {err}"));
            return errors;
        }
    };

    for op in ops
        .into_iter()
        .filter(|op| op.user_id == user_id && !op.google_event_id.is_empty())
    {
        if let Err(msg) = repair_one(
            http,
            calendars,
            events,
            operations,
            access,
            &op,
            &now_rfc3339,
        )
        .await
        {
            errors.push(msg);
        }
    }

    errors
}

async fn repair_one(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    operations: &dyn CalendarEventOperationRepo,
    access: &GoogleAccess,
    op: &CalendarEventOperation,
    now_rfc3339: &str,
) -> Result<(), String> {
    let Some(cal) = calendars
        .get_by_id(&op.calendar_id)
        .await
        .map_err(|err| format!("repair get calendar {} failed: {err}", op.calendar_id))?
    else {
        // Calendar gone — skip; leave journal row for operator visibility.
        return Ok(());
    };

    let url = format!(
        "{GOOGLE_EVENTS_BASE_URL}/{}/events/{}",
        encode_path_segment(&cal.google_calendar_id),
        encode_path_segment(&op.google_event_id)
    );

    let (status, body) = http
        .get_bearer_raw(&url, &access.access_token)
        .await
        .map_err(|err| {
            format!(
                "repair GET failed for op {} (calendar {}): {err}",
                op.id, op.calendar_id
            )
        })?;

    if (200..300).contains(&status) {
        return apply_get_ok(events, operations, op, &cal.id, &body, now_rfc3339).await;
    }

    if status == 404 || status == 410 {
        return apply_get_gone(events, operations, op, now_rfc3339).await;
    }

    // Leave status unchanged; surface for the cron report.
    Err(format!(
        "repair GET returned {status} for op {} (calendar {}, google_event_id {})",
        op.id, op.calendar_id, op.google_event_id
    ))
}

async fn apply_get_ok(
    events: &dyn CalendarEventRepo,
    operations: &dyn CalendarEventOperationRepo,
    op: &CalendarEventOperation,
    calendar_id: &str,
    body: &[u8],
    now_rfc3339: &str,
) -> Result<(), String> {
    let ge = parse_google_event(body, "repair events.get").map_err(|err| {
        format!(
            "repair parse failed for op {} (calendar {}): {err}",
            op.id, op.calendar_id
        )
    })?;

    let cancelled_ordinary = ge.status.as_deref() == Some("cancelled")
        && ge
            .recurring_event_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_none();

    if op.verb == OP_VERB_DELETE || cancelled_ordinary {
        local_delete(events, op, now_rfc3339)
            .await
            .map_err(|err| format!("repair local-delete failed for op {}: {err}", op.id))?;
        mark_cache_applied(operations, op, &op.local_event_id, &ge.etag.clone().unwrap_or_default(), now_rfc3339)
            .await
            .map_err(|err| format!("repair cache_applied failed for op {}: {err}", op.id))?;
        return Ok(());
    }

    let (local_id, etag) = match classify_replica_item(&ge, calendar_id, now_rfc3339) {
        ReplicaApplyAction::SoftDelete { google_event_id } => {
            events
                .delete_by_google_event_id(calendar_id, &google_event_id, now_rfc3339)
                .await
                .map_err(|err| format!("repair soft-delete failed for op {}: {err}", op.id))?;
            (op.local_event_id.clone(), ge.etag.clone().unwrap_or_default())
        }
        ReplicaApplyAction::Upsert(row) => {
            let etag = row.google_etag.clone();
            let local_id = events
                .upsert(row, now_rfc3339)
                .await
                .map_err(|err| format!("repair upsert failed for op {}: {err}", op.id))?;
            (local_id, etag)
        }
    };

    mark_cache_applied(operations, op, &local_id, &etag, now_rfc3339)
        .await
        .map_err(|err| format!("repair cache_applied failed for op {}: {err}", op.id))?;
    Ok(())
}

async fn apply_get_gone(
    events: &dyn CalendarEventRepo,
    operations: &dyn CalendarEventOperationRepo,
    op: &CalendarEventOperation,
    now_rfc3339: &str,
) -> Result<(), String> {
    match op.verb.as_str() {
        OP_VERB_DELETE => {
            local_delete(events, op, now_rfc3339)
                .await
                .map_err(|err| format!("repair local-delete failed for op {}: {err}", op.id))?;
            mark_cache_applied(operations, op, &op.local_event_id, &op.google_etag, now_rfc3339)
                .await
                .map_err(|err| format!("repair cache_applied failed for op {}: {err}", op.id))?;
            Ok(())
        }
        OP_VERB_INSERT if op.status == OP_STATUS_PENDING => {
            // Google never saw it — do not insert again.
            operations
                .update_status(
                    &op.id,
                    OP_STATUS_FAILED,
                    "repair: insert never reached Google (GET 404/410)",
                    now_rfc3339,
                )
                .await
                .map_err(|err| format!("repair mark failed for op {}: {err}", op.id))?;
            Ok(())
        }
        OP_VERB_INSERT if op.status == OP_STATUS_GOOGLE_COMMITTED => {
            // Vanished after commit — drop local echo and close the journal.
            local_delete(events, op, now_rfc3339)
                .await
                .map_err(|err| format!("repair local-delete failed for op {}: {err}", op.id))?;
            mark_cache_applied(operations, op, &op.local_event_id, &op.google_etag, now_rfc3339)
                .await
                .map_err(|err| format!("repair cache_applied failed for op {}: {err}", op.id))?;
            Ok(())
        }
        OP_VERB_PATCH if op.status == OP_STATUS_PENDING => {
            // Do not re-PATCH.
            operations
                .update_status(
                    &op.id,
                    OP_STATUS_FAILED,
                    "repair: patch never reached Google (GET 404/410)",
                    now_rfc3339,
                )
                .await
                .map_err(|err| format!("repair mark failed for op {}: {err}", op.id))?;
            Ok(())
        }
        OP_VERB_PATCH if op.status == OP_STATUS_GOOGLE_COMMITTED => {
            // Patch committed then vanished — local-delete + close.
            local_delete(events, op, now_rfc3339)
                .await
                .map_err(|err| format!("repair local-delete failed for op {}: {err}", op.id))?;
            mark_cache_applied(operations, op, &op.local_event_id, &op.google_etag, now_rfc3339)
                .await
                .map_err(|err| format!("repair cache_applied failed for op {}: {err}", op.id))?;
            Ok(())
        }
        other => Err(format!(
            "repair GET 404/410 for op {} with unexpected verb/status {other}/{}",
            op.id, op.status
        )),
    }
}

async fn local_delete(
    events: &dyn CalendarEventRepo,
    op: &CalendarEventOperation,
    now_rfc3339: &str,
) -> Result<(), crate::repo::RepoError> {
    if !op.local_event_id.is_empty() {
        events.delete(&op.local_event_id, now_rfc3339).await
    } else {
        events
            .delete_by_google_event_id(&op.calendar_id, &op.google_event_id, now_rfc3339)
            .await
    }
}

async fn mark_cache_applied(
    operations: &dyn CalendarEventOperationRepo,
    op: &CalendarEventOperation,
    local_event_id: &str,
    google_etag: &str,
    now_rfc3339: &str,
) -> Result<(), crate::repo::RepoError> {
    operations
        .update_progress(
            &op.id,
            OP_STATUS_CACHE_APPLIED,
            &op.google_event_id,
            local_event_id,
            google_etag,
            "",
            false,
            now_rfc3339,
        )
        .await
}
