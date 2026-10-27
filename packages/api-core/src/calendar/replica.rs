//! Canonical Google Calendar **replica walk** (Path B).
//!
//! Owns `events.list` with `singleEvents=false&maxResults=250`, optional
//! `syncToken` / `pageToken`, and **no** `timeMin` / `timeMax` / `updatedMin`.
//! Only this walk may send or publish `syncToken`.
//!
//! # Invariants (ADR 0005)
//!
//! - **Never advance `sync_token` past uncommitted work.** Pages are applied as
//!   they arrive; the terminal `nextSyncToken` is published only via
//!   [`CalendarRepo::record_sync_success_if_owner`] after the last page commits.
//! - **One fenced owner per calendar.** Acquire a lease before the walk from
//!   a fresh wall clock (not the cron tick's start); renew **before** each
//!   page so the SQL fence timestamp sits inside the new TTL; apply
//!   upserts/tombstones via SQL-fenced `*_if_owner` writes; publish the
//!   cursor only via
//!   [`CalendarRepo::record_sync_success_if_owner`]. A lost owner must not write
//!   rows or publish a cursor.
//! - **410 is merge-full, not truncate.** Drop the in-memory token/page cursor
//!   and restart the list once. Persist `full_sync_requested = 1` and
//!   `sync_status = 'rebuilding'` via [`CalendarRepo::begin_replica_reseed`]
//!   so the next cron tick does not depend on in-memory state. Do **not** clear
//!   the stored `sync_token` until a successful publish. Do **not** call
//!   `delete_stale`, do **not** wipe already-applied rows mid-walk. On a
//!   **completed** fenced merge-full, soft-delete living Google ids absent
//!   from this walk's seen snapshot via
//!   [`CalendarEventRepo::sweep_absent_if_owner`] (generation membership —
//!   not a timestamp sweep). Mid-walk failure keeps the old published view;
//!   lost lease cannot sweep or publish. A second 410 in the same invocation
//!   is an error (not a loop); failure classification keeps `rebuilding`
//!   while leaving the stored token alone.
//! - **Fingerprint mismatch / `full_sync_requested` → merge-full.** A non-empty
//!   stored `sync_query_fingerprint` that differs from
//!   [`replica_query_fingerprint`], or a durable `full_sync_requested` flag,
//!   drops the in-memory token for this walk and persists reseed via
//!   [`CalendarRepo::begin_replica_reseed`]. The stored token is still not
//!   cleared until success. An **empty** stored fingerprint with an existing
//!   token is treated as compatible (production tokens stay valid).
//! - **Poison is not skip.** Invalid page JSON is `InvalidResponse`
//!   (`mapping_poison`); the page is not silently dropped and the token is not
//!   advanced.
//! - **App-owned columns** (`task_id`, …) stay COALESCE-protected at the SQL /
//!   fake layer; this module never overwrites them with empty Google values.
//!
//! Window fetch (`singleEvents=true`, time bounds) is a separate path and must
//! not bootstrap or persist `nextSyncToken` (slice 3).

use url::Url;

use super::{CalendarError, GOOGLE_EVENTS_BASE_URL};
use super::apply::{
    classify_replica_item, EventClassifyCtx, EventsPage, ReplicaApplyAction,
};
use super::diagnostics::{mint_run_id, CheckpointResult, ReplicaApplyReport, ReplicaWalkPhase};
use super::google::encode_path_segment;
use super::sync::replica_query_fingerprint;
use crate::models::GoogleCalendar;
use crate::oauth::HttpClient;
use crate::repo::{CalendarEventOperationRepo, CalendarEventRepo, CalendarRepo};
use crate::time::{rfc3339_to_unix_secs, unix_secs_to_rfc3339, Clock};
use crate::token::GoogleAccess;
use std::collections::HashSet;

/// Replica lease lifetime (seconds). Renewed before each page is applied,
/// from a fresh wall clock — not from the cron tick's start.
pub const REPLICA_LEASE_TTL_SECS: i64 = 90;

/// Stop starting another Google page this long after the invocation's
/// `now_unix`. A cron isolate is killed at about 15 minutes with no
/// unwind, so the walk must release its lease itself and leave the tick
/// time to finish (sibling calendars, watch renewal, agenda fill).
pub const REPLICA_WALK_BUDGET_SECS: i64 = 11 * 60;

/// How [`sync_replica`] stopped when it did not return an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicaWalkFinish {
    /// Terminal `nextSyncToken` was published under the lease.
    Published,
    /// Budget elapsed before a terminal page. No failure was recorded.
    /// The caller releases the lease and leaves health as the walk left
    /// it (`rebuilding` once a reseed has been stamped).
    Yielded,
}

/// Max bytes retained in `calendar_event_quarantine.replay_payload` (64 KiB).
const QUARANTINE_PAYLOAD_MAX_BYTES: usize = 64 * 1024;

/// Lossy UTF-8 decode of a Google page body, capped for quarantine storage.
/// Never log the result; never put it on diagnostics or the GET envelope.
fn quarantine_replay_payload(body: &[u8]) -> String {
    let slice = if body.len() > QUARANTINE_PAYLOAD_MAX_BYTES {
        &body[..QUARANTINE_PAYLOAD_MAX_BYTES]
    } else {
        body
    };
    String::from_utf8_lossy(slice).into_owned()
}

/// Fenced page-by-page replica walk for one calendar.
///
/// Caller must already hold the lease as `lease_owner`, have stamped
/// `record_sync_attempt`, and have run the event-label backfill. On `Err`, the
/// caller records failure and releases the lease. On
/// [`ReplicaWalkFinish::Yielded`] the caller releases the lease and does
/// **not** record a failure.
///
/// `cal` must be a **fresh** re-read after lease acquire (cursor + fingerprint),
/// not a stale request-path snapshot.
///
/// `report` is updated in place with page/upsert/delete/attempt counts,
/// phase, and checkpoint outcome. Secrets (tokens, URLs, bodies) are never
/// written onto the report.
///
/// `ctx` stamps the app-owned leaf `category_id` on every upsert (the SQL
/// upsert **overwrites** the column). Production callers must pass a real
/// context — [`EventClassifyCtx::preserve`] only when the taxonomy load
/// failed (it keeps any stored stamp instead of blanking it).
///
/// `deadline_unix` is an absolute Unix second. When `clock` reaches it
/// before a fetch, the walk returns [`ReplicaWalkFinish::Yielded`] without
/// recording a failure. Pass `invocation_now + REPLICA_WALK_BUDGET_SECS`.
pub async fn sync_replica(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    operations: &dyn CalendarEventOperationRepo,
    access: &GoogleAccess,
    cal: &GoogleCalendar,
    lease_owner: &str,
    clock: &dyn Clock,
    report: &mut ReplicaApplyReport,
    deadline_unix: i64,
    ctx: &EventClassifyCtx<'_>,
) -> Result<ReplicaWalkFinish, CalendarError> {
    let fingerprint = replica_query_fingerprint();
    // Empty stored fingerprint is compatible with an existing token (do not
    // invalidate production cursors). Only a non-empty mismatch, or a durable
    // full_sync_requested flag, forces merge-full. First-ever sync (empty
    // token, empty fingerprint, flag clear) stays never_initialized — do not
    // stamp rebuilding.
    let fingerprint_mismatch = !cal.sync_query_fingerprint.is_empty()
        && cal.sync_query_fingerprint != fingerprint;
    let reseed = cal.full_sync_requested || fingerprint_mismatch;
    if reseed {
        let now = clock.now_rfc3339();
        if let Err(err) = calendars.begin_replica_reseed(&cal.id, &now).await {
            report.checkpoint = CheckpointResult::Error;
            return Err(err.into());
        }
    }
    let mut token: Option<String> = if reseed || cal.sync_token.is_empty() {
        None
    } else {
        Some(cal.sync_token.clone())
    };

    let mut page_token: Option<String> = None;
    let mut retried_410 = false;
    // Merge-full = reseed start or 410 restart. First-ever and ordinary
    // incremental never mint a seen-set or sweep (window instance ids).
    let mut merge_full = reseed;
    let mut run_id: Option<String> = None;
    if merge_full {
        if let Err(err) = start_merge_full_run(events, &cal.id, &mut run_id).await {
            report.checkpoint = CheckpointResult::Error;
            return Err(err);
        }
    }

    // Snapshot once per walk: user writes in flight must not be clobbered by
    // a replica page that still carries the pre-write Google shape.
    let inflight: HashSet<String> = match operations.list_inflight_google_ids(&cal.id).await {
        Ok(ids) => ids.into_iter().filter(|id| !id.is_empty()).collect(),
        Err(err) => {
            report.checkpoint = CheckpointResult::Error;
            return Err(err.into());
        }
    };

    report.phase = ReplicaWalkPhase::Fetch;

    loop {
        if clock.now_unix() >= deadline_unix {
            // Voluntary stop. A platform kill would skip release_lease and
            // the rest of the cron tick. Health stays rebuilding when a
            // reseed was already stamped; the next tick starts again.
            report.checkpoint = CheckpointResult::Yielded;
            report.phase = ReplicaWalkPhase::Release;
            return Ok(ReplicaWalkFinish::Yielded);
        }
        let url = google_events_url(
            &cal.google_calendar_id,
            token.as_deref(),
            page_token.as_deref(),
        );
        report.attempts = report.attempts.saturating_add(1);
        let (status, body) = match http.get_bearer_raw(&url, &access.access_token).await {
            Ok(pair) => pair,
            Err(err) => {
                report.checkpoint = CheckpointResult::Error;
                return Err(err.into());
            }
        };

        if status == 410 {
            if retried_410 {
                report.checkpoint = CheckpointResult::Error;
                return Err(CalendarError::GoogleApi(
                    "google events.list returned 410".into(),
                ));
            }
            // Merge-full: durable reseed flag + drop in-memory cursor. Keep
            // already-applied rows and the stored token until success.
            // Never delete_stale / truncate mid-walk. Mint a fresh run so
            // incremental pages applied before the 410 do not count as
            // membership until they reappear on the full list.
            let now = clock.now_rfc3339();
            if let Err(err) = calendars.begin_replica_reseed(&cal.id, &now).await {
                report.checkpoint = CheckpointResult::Error;
                return Err(err.into());
            }
            merge_full = true;
            if let Err(err) = start_merge_full_run(events, &cal.id, &mut run_id).await {
                report.checkpoint = CheckpointResult::Error;
                return Err(err);
            }
            retried_410 = true;
            token = None;
            page_token = None;
            continue;
        }
        if status == 404 {
            report.checkpoint = CheckpointResult::Error;
            return Err(CalendarError::GoogleNotFound);
        }
        if !(200..300).contains(&status) {
            report.checkpoint = CheckpointResult::Error;
            return Err(CalendarError::GoogleApi(format!(
                "google events.list returned {status}"
            )));
        }

        let page: EventsPage = match serde_json::from_slice(&body) {
            Ok(p) => p,
            Err(err) => {
                report.checkpoint = CheckpointResult::Error;
                // Best-effort quarantine + degraded coverage. Failures here
                // must not change the error class (still mapping_poison via
                // InvalidResponse) or advance the token.
                let payload = quarantine_replay_payload(&body);
                let now = clock.now_rfc3339();
                let _ = events
                    .upsert_quarantine(
                        &cal.id,
                        "",
                        "replica_page",
                        "mapping_poison",
                        &payload,
                        &now,
                    )
                    .await;
                let _ = calendars
                    .set_event_coverage(&cal.id, "degraded", &now)
                    .await;
                return Err(CalendarError::InvalidResponse(format!(
                    "events.list body: {err}"
                )));
            }
        };

        // Renew before apply so the fence timestamp is inside a fresh TTL.
        // The SQL compares `lease_expires_at` to this captured `now`, not to
        // wall time when D1 runs, so the grant has to outlive the page.
        report.phase = ReplicaWalkPhase::Apply;
        let now = clock.now_rfc3339();
        let renew_expires = lease_expires_at(&now);
        let renewed = match calendars
            .renew_lease(&cal.id, lease_owner, &renew_expires, &now)
            .await
        {
            Ok(renewed) => renewed,
            Err(err) => {
                report.checkpoint = CheckpointResult::Error;
                return Err(err.into());
            }
        };
        if !renewed {
            report.checkpoint = CheckpointResult::LeaseLost;
            return Err(CalendarError::Invalid("lost replica lease".into()));
        }
        if let Err(err) = ensure_lease_held(calendars, &cal.id, lease_owner, &now).await {
            report.checkpoint = CheckpointResult::LeaseLost;
            return Err(err);
        }

        let items = page.items.unwrap_or_default();
        let mut to_upsert = Vec::new();
        for item in &items {
            if inflight.contains(&item.id) {
                // Skip upsert and soft-delete for in-flight google ids.
                continue;
            }
            match classify_replica_item(item, &cal.id, &now, ctx) {
                ReplicaApplyAction::SoftDelete { google_event_id } => {
                    report.deletes = report.deletes.saturating_add(1);
                    let deleted = match events
                        .delete_by_google_event_id_if_owner(
                            &cal.id,
                            &google_event_id,
                            lease_owner,
                            &now,
                        )
                        .await
                    {
                        Ok(d) => d,
                        Err(err) => {
                            report.checkpoint = CheckpointResult::Error;
                            return Err(err.into());
                        }
                    };
                    if !deleted {
                        report.checkpoint = CheckpointResult::LeaseLost;
                        return Err(CalendarError::Invalid("lost replica lease".into()));
                    }
                }
                ReplicaApplyAction::Upsert(row) => {
                    to_upsert.push(row);
                }
            }
        }
        if !to_upsert.is_empty() {
            report.upserts = report
                .upserts
                .saturating_add(to_upsert.len() as u32);
            let applied = match events
                .upsert_batch_if_owner(to_upsert, lease_owner, &now)
                .await
            {
                Ok(a) => a,
                Err(err) => {
                    report.checkpoint = CheckpointResult::Error;
                    return Err(err.into());
                }
            };
            if !applied {
                report.checkpoint = CheckpointResult::LeaseLost;
                return Err(CalendarError::Invalid("lost replica lease".into()));
            }
        }

        // Successful 2xx page applied. On merge-full, record every Google id
        // on the page (upserted, cancelled, cancelled exceptions, in-flight
        // skips) so the terminal sweep will not tombstone them.
        if merge_full {
            let Some(rid) = run_id.as_deref() else {
                report.checkpoint = CheckpointResult::Error;
                return Err(CalendarError::Invalid(
                    "merge-full walk missing run_id".into(),
                ));
            };
            let page_ids: Vec<String> = items
                .iter()
                .map(|item| item.id.clone())
                .filter(|id| !id.is_empty())
                .collect();
            if let Err(err) = events
                .record_replica_seen(&cal.id, rid, page_ids, &now)
                .await
            {
                report.checkpoint = CheckpointResult::Error;
                return Err(err.into());
            }
        }

        // Successful 2xx page applied. The lease was renewed before the
        // writes; the next iteration renews again before the next page.
        report.pages = report.pages.saturating_add(1);

        if let Some(next) = page.next_page_token.filter(|t| !t.is_empty()) {
            page_token = Some(next);
            report.phase = ReplicaWalkPhase::Fetch;
            continue;
        }

        // Terminal page: publication requires a non-empty nextSyncToken.
        report.phase = ReplicaWalkPhase::Checkpoint;
        let Some(next_token) = page.next_sync_token.filter(|t| !t.is_empty()) else {
            report.checkpoint = CheckpointResult::MissingSyncToken;
            return Err(CalendarError::InvalidResponse(
                "missing nextSyncToken on terminal page".into(),
            ));
        };

        // Terminal order: ensure lease → (merge-full) sweep → publish.
        // Sweep failure or lease loss must not move the token.
        let now = clock.now_rfc3339();
        if merge_full {
            if let Err(err) =
                ensure_lease_held(calendars, &cal.id, lease_owner, &now).await
            {
                report.checkpoint = CheckpointResult::LeaseLost;
                return Err(err);
            }
            let Some(rid) = run_id.as_deref() else {
                report.checkpoint = CheckpointResult::Error;
                return Err(CalendarError::Invalid(
                    "merge-full walk missing run_id".into(),
                ));
            };
            let swept = match events
                .sweep_absent_if_owner(&cal.id, rid, lease_owner, &now)
                .await
            {
                Ok(s) => s,
                Err(err) => {
                    report.checkpoint = CheckpointResult::Error;
                    return Err(err.into());
                }
            };
            if !swept {
                report.checkpoint = CheckpointResult::LeaseLost;
                return Err(CalendarError::Invalid("lost replica lease".into()));
            }
        }

        let published = match calendars
            .record_sync_success_if_owner(
                &cal.id,
                &next_token,
                &fingerprint,
                lease_owner,
                &now,
            )
            .await
        {
            Ok(p) => p,
            Err(err) => {
                report.checkpoint = CheckpointResult::Error;
                return Err(err.into());
            }
        };
        if !published {
            report.checkpoint = CheckpointResult::LeaseLost;
            return Err(CalendarError::Invalid("lost replica lease".into()));
        }
        // Best-effort clear; leftovers are OK (next merge-full clears first).
        if merge_full {
            let _ = events.clear_replica_seen_for_calendar(&cal.id).await;
        }
        // Poison page is gone once we published a terminal token.
        let _ = events.clear_quarantine_for_calendar(&cal.id).await;
        report.checkpoint = CheckpointResult::Published;
        return Ok(ReplicaWalkFinish::Published);
    }
}

/// Mint a new `run_id` and abandon any previous incomplete seen snapshot.
async fn start_merge_full_run(
    events: &dyn CalendarEventRepo,
    calendar_id: &str,
    run_id: &mut Option<String>,
) -> Result<(), CalendarError> {
    if let Err(err) = events.clear_replica_seen_for_calendar(calendar_id).await {
        return Err(err.into());
    }
    *run_id = Some(mint_run_id());
    Ok(())
}

async fn ensure_lease_held(
    calendars: &dyn CalendarRepo,
    id: &str,
    lease_owner: &str,
    now_rfc3339: &str,
) -> Result<(), CalendarError> {
    let Some(row) = calendars.get_by_id(id).await? else {
        return Err(CalendarError::Invalid("lost replica lease".into()));
    };
    if row.lease_owner != lease_owner {
        return Err(CalendarError::Invalid("lost replica lease".into()));
    }
    if let Some(exp) = row.lease_expires_at.as_deref() {
        if exp < now_rfc3339 {
            return Err(CalendarError::Invalid("lost replica lease".into()));
        }
    }
    Ok(())
}

/// `now_rfc3339 + REPLICA_LEASE_TTL_SECS` as RFC 3339 UTC.
pub fn lease_expires_at(now_rfc3339: &str) -> String {
    let now_unix = rfc3339_to_unix_secs(now_rfc3339).unwrap_or(0);
    unix_secs_to_rfc3339(now_unix + REPLICA_LEASE_TTL_SECS)
}

/// 16 random bytes as lowercase hex (32 chars) — unique lease owner id.
pub fn mint_lease_owner() -> String {
    let mut bytes = [0u8; 16];
    // Same entropy source as oauth::generate_state / watch mint helpers.
    let _ = getrandom::getrandom(&mut bytes);
    let mut out = String::with_capacity(32);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Builds the replica `events.list` URL: `singleEvents=false&maxResults=250`,
/// optional `syncToken` / `pageToken`. No time bounds.
pub(crate) fn google_events_url(
    google_cal_id: &str,
    sync_token: Option<&str>,
    page_token: Option<&str>,
) -> String {
    let mut url = Url::parse(&format!(
        "{GOOGLE_EVENTS_BASE_URL}/{}/events",
        encode_path_segment(google_cal_id)
    ))
    .expect("static events URL is valid");
    url.query_pairs_mut()
        .append_pair("singleEvents", "false")
        .append_pair("maxResults", "250");
    if let Some(token) = sync_token {
        url.query_pairs_mut().append_pair("syncToken", token);
    }
    if let Some(token) = page_token {
        url.query_pairs_mut().append_pair("pageToken", token);
    }
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replica_url_has_no_time_bounds() {
        let url = google_events_url("primary@example.com", Some("tok"), None);
        assert!(url.contains("singleEvents=false"), "{url}");
        assert!(url.contains("maxResults=250"), "{url}");
        assert!(url.contains("syncToken=tok"), "{url}");
        assert!(!url.contains("timeMin"), "{url}");
        assert!(!url.contains("timeMax"), "{url}");
        assert!(!url.contains("updatedMin"), "{url}");
        assert!(!url.contains("singleEvents=true"), "{url}");
    }

    #[test]
    fn lease_expires_at_adds_ttl() {
        let exp = lease_expires_at("2023-11-14T22:13:20Z");
        assert_eq!(exp, "2023-11-14T22:14:50Z"); // +90s
    }

    #[test]
    fn mint_lease_owner_is_32_hex_chars() {
        let a = mint_lease_owner();
        let b = mint_lease_owner();
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()), "{a}");
        // Extremely unlikely collision; still documents uniqueness intent.
        assert_ne!(a, b);
    }
}
