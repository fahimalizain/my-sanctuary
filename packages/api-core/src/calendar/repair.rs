//! GET-only repair of stuck outbound calendar write journal rows
//! (`pending` / `google_committed`). Issue #50 / Vertical 4.
//!
//! Never POST/PATCH from this module. A pending insert that 404s is marked
//! `failed` (user retries with a new client id). A google_committed insert
//! whose Google row is gone is local-deleted and closed as `cache_applied`.

use super::apply::{classify_replica_item, EventClassifyCtx, ReplicaApplyAction};
use super::google::encode_path_segment;
use super::journal::parse_google_event;
use super::GOOGLE_EVENTS_BASE_URL;
use crate::calendar_color::{load_classify_taxonomy, PaintTaxonomy};
use crate::models::{
    CalendarEventOperation, OP_STATUS_CACHE_APPLIED, OP_STATUS_FAILED, OP_STATUS_GOOGLE_COMMITTED,
    OP_STATUS_PENDING, OP_VERB_DELETE, OP_VERB_INSERT, OP_VERB_MOVE, OP_VERB_PATCH,
};
use crate::oauth::HttpClient;
use crate::repo::{CalendarEventOperationRepo, CalendarEventRepo, CalendarRepo, TaskCategoryRepo};
use crate::time::unix_secs_to_rfc3339;
use crate::token::GoogleAccess;

/// Recover in-flight journal rows for one user by GET only.
///
/// `category_repo` feeds the write-time classifier: the taxonomy is loaded
/// **once per user** (this function is already per-user, no seeding) and
/// reused for every repaired op — repair writes the same `category_id` stamp
/// as the sync walks. A load failure only degrades the stamp
/// (`EventClassifyCtx::preserve`), never the repair.
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
    category_repo: &dyn TaskCategoryRepo,
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

    // One taxonomy load for the whole user (cached across ops); a failure
    // leaves `None` and every op repairs with the empty context.
    let taxonomy = load_classify_taxonomy(category_repo, user_id)
        .await
        .ok();

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
            taxonomy.as_ref(),
        )
        .await
        {
            errors.push(msg);
        }
    }

    errors
}

#[allow(clippy::too_many_arguments)]
async fn repair_one(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    operations: &dyn CalendarEventOperationRepo,
    access: &GoogleAccess,
    op: &CalendarEventOperation,
    now_rfc3339: &str,
    taxonomy: Option<&PaintTaxonomy>,
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
        // Source still has the event — move never landed (or was abandoned).
        let ctx = classify_ctx_for(taxonomy, &cal.google_calendar_id);
        return apply_get_ok(events, operations, op, &cal.id, &body, now_rfc3339, &ctx).await;
    }

    if status == 404 || status == 410 {
        if op.verb == OP_VERB_MOVE {
            return repair_move_source_gone(
                http,
                calendars,
                events,
                operations,
                access,
                op,
                now_rfc3339,
                taxonomy,
            )
            .await;
        }
        return apply_get_gone(events, operations, op, now_rfc3339).await;
    }

    // Leave status unchanged; surface for the cron report.
    Err(format!(
        "repair GET returned {status} for op {} (calendar {}, google_event_id {})",
        op.id, op.calendar_id, op.google_event_id
    ))
}

/// Source GET 404/410 for a `move` op: event left the source calendar.
/// Resolve destination from payload and GET there (never POST/PATCH).
#[allow(clippy::too_many_arguments)]
async fn repair_move_source_gone(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    operations: &dyn CalendarEventOperationRepo,
    access: &GoogleAccess,
    op: &CalendarEventOperation,
    now_rfc3339: &str,
    taxonomy: Option<&PaintTaxonomy>,
) -> Result<(), String> {
    let destination = parse_move_destination(&op.payload_json).ok_or_else(|| {
        format!(
            "repair move op {} missing destination in payload_json",
            op.id
        )
    })?;

    let Some(dest_cal) = calendars
        .get_by_google_cal_id(&op.user_id, &destination)
        .await
        .map_err(|err| {
            format!(
                "repair move get dest calendar {} failed for op {}: {err}",
                destination, op.id
            )
        })?
    else {
        return Err(format!(
            "repair move dest calendar {} not found for op {}",
            destination, op.id
        ));
    };

    let dest_url = format!(
        "{GOOGLE_EVENTS_BASE_URL}/{}/events/{}",
        encode_path_segment(&dest_cal.google_calendar_id),
        encode_path_segment(&op.google_event_id)
    );

    let (dest_status, dest_body) = http
        .get_bearer_raw(&dest_url, &access.access_token)
        .await
        .map_err(|err| {
            format!(
                "repair move dest GET failed for op {} (dest {}): {err}",
                op.id, dest_cal.id
            )
        })?;

    if (200..300).contains(&dest_status) {
        let ge = parse_google_event(&dest_body, "repair move events.get dest").map_err(|err| {
            format!(
                "repair move dest parse failed for op {} (dest {}): {err}",
                op.id, dest_cal.id
            )
        })?;
        let etag = ge.etag.clone().unwrap_or_default();
        let local_id = if !op.local_event_id.is_empty() {
            op.local_event_id.clone()
        } else {
            // Fall back to natural-key lookup on source (unlikely for move).
            events
                .get_by_calendar_and_google_id(&op.calendar_id, &op.google_event_id)
                .await
                .map_err(|err| format!("repair move lookup failed for op {}: {err}", op.id))?
                .map(|e| e.id)
                .unwrap_or_default()
        };

        if !local_id.is_empty() {
            events
                .reassign_calendar(&local_id, &dest_cal.id, now_rfc3339)
                .await
                .map_err(|err| {
                    format!("repair move reassign failed for op {}: {err}", op.id)
                })?;
        }

        let ctx = classify_ctx_for(taxonomy, &dest_cal.google_calendar_id);
        let row = match classify_replica_item(&ge, &dest_cal.id, now_rfc3339, &ctx) {
            ReplicaApplyAction::SoftDelete { google_event_id } => {
                events
                    .delete_by_google_event_id(&dest_cal.id, &google_event_id, now_rfc3339)
                    .await
                    .map_err(|err| {
                        format!("repair move dest soft-delete failed for op {}: {err}", op.id)
                    })?;
                mark_cache_applied(operations, op, &local_id, &etag, now_rfc3339)
                    .await
                    .map_err(|err| {
                        format!("repair move cache_applied failed for op {}: {err}", op.id)
                    })?;
                return Ok(());
            }
            ReplicaApplyAction::Upsert(row) => row,
        };

        let upserted_id = events
            .upsert(row, now_rfc3339)
            .await
            .map_err(|err| format!("repair move upsert failed for op {}: {err}", op.id))?;

        mark_cache_applied(operations, op, &upserted_id, &etag, now_rfc3339)
            .await
            .map_err(|err| format!("repair move cache_applied failed for op {}: {err}", op.id))?;
        return Ok(());
    }

    if dest_status == 404 || dest_status == 410 {
        return match op.status.as_str() {
            OP_STATUS_PENDING => {
                // Move never reached Google (or never completed) — do not delete local.
                operations
                    .update_status(
                        &op.id,
                        OP_STATUS_FAILED,
                        "repair: move never reached Google (source+dest GET 404/410)",
                        now_rfc3339,
                    )
                    .await
                    .map_err(|err| format!("repair mark failed for op {}: {err}", op.id))?;
                Ok(())
            }
            OP_STATUS_GOOGLE_COMMITTED => {
                // Vanished after commit — local-delete + close.
                local_delete(events, op, now_rfc3339)
                    .await
                    .map_err(|err| format!("repair local-delete failed for op {}: {err}", op.id))?;
                mark_cache_applied(
                    operations,
                    op,
                    &op.local_event_id,
                    &op.google_etag,
                    now_rfc3339,
                )
                .await
                .map_err(|err| format!("repair cache_applied failed for op {}: {err}", op.id))?;
                Ok(())
            }
            other => Err(format!(
                "repair move dest GET 404/410 for op {} with unexpected status {other}",
                op.id
            )),
        };
    }

    Err(format!(
        "repair move dest GET returned {dest_status} for op {} (dest {})",
        op.id, dest_cal.id
    ))
}

/// Classifier context for the calendar a repaired row lands on: the taxonomy
/// is shared, the Google id differs per calendar (pattern scope). A missing
/// taxonomy (`.ok()` above) degrades to the **preserving** context, so the
/// stored stamp survives the repair instead of being blanked.
fn classify_ctx_for<'a>(
    taxonomy: Option<&'a PaintTaxonomy>,
    google_calendar_id: &'a str,
) -> EventClassifyCtx<'a> {
    match taxonomy {
        Some(taxonomy) => taxonomy.event_ctx(google_calendar_id),
        // A failed load must not blank a good stamp.
        None => EventClassifyCtx::preserve(),
    }
}

fn parse_move_destination(payload_json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(payload_json).ok()?;
    value
        .get("destination")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

async fn apply_get_ok(
    events: &dyn CalendarEventRepo,
    operations: &dyn CalendarEventOperationRepo,
    op: &CalendarEventOperation,
    calendar_id: &str,
    body: &[u8],
    now_rfc3339: &str,
    ctx: &EventClassifyCtx<'_>,
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

    let (local_id, etag) = match classify_replica_item(&ge, calendar_id, now_rfc3339, ctx) {
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
