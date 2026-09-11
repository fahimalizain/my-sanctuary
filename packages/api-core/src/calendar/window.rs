//! Canonical Google Calendar **window fetch** (Path A).
//!
//! Owns `events.list` with `singleEvents=true&orderBy=startTime&maxResults=250`
//! plus `timeMin` / `timeMax` (and optional `pageToken`). **Never** sends
//! `syncToken` or `updatedMin`. Any `nextSyncToken` Google returns is thrown
//! away — only the replica walk ([`crate::calendar_replica`]) may publish a
//! cursor.
//!
//! Used by [`crate::calendar::list_events`] for never-initialized calendars so
//! first paint is not blocked on the replica token. Write-through reuses
//! [`classify_replica_item`] and applies via the same lease-fenced
//! `*_if_owner` methods as replica (gated on `google_calendars.lease_owner` /
//! expiry). On lease miss at acquire — or mid-window fence loss — the live
//! window is still returned ephemerally when no write-through ran (no D1
//! write); a mid-window steal/expire stops write-through without erroring.

use url::Url;

use super::{CalendarError, GOOGLE_EVENTS_BASE_URL};
use super::apply::{
    classify_replica_item, row_from_new_event, EventsPage, ReplicaApplyAction,
};
use super::google::encode_path_segment;
use super::replica::{lease_expires_at, mint_lease_owner};
use crate::models::{CalendarEvent, GoogleCalendar};
use crate::oauth::HttpClient;
use crate::repo::{CalendarEventRepo, CalendarRepo};
use crate::token::GoogleAccess;

/// Bounded visible-window fetch for one never-initialized calendar.
///
/// - Optionally takes the same replica lease for write-through. Lease miss is
///   not an error: live pages are still fetched and returned as ephemeral
///   rows (caller merges into the HTTP payload); D1 is not touched.
/// - Never sends or persists `syncToken` / `nextSyncToken`.
/// - Never calls `record_sync_success` / `record_sync_attempt` /
///   `record_sync_failure`.
///
/// Returns ephemeral events only when write-through was skipped (lease miss).
/// When the lease was held and pages applied, returns an empty vec (rows are
/// already in D1 for the subsequent cache query).
pub async fn fetch_and_apply_window(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    access: &GoogleAccess,
    cal: &GoogleCalendar,
    time_min_rfc3339: &str,
    time_max_rfc3339: &str,
    now_rfc3339: &str,
) -> Result<Vec<CalendarEvent>, CalendarError> {
    let owner = mint_lease_owner();
    let expires = lease_expires_at(now_rfc3339);
    let acquired = calendars
        .try_acquire_lease(&cal.id, &owner, now_rfc3339, &expires)
        .await
        .unwrap_or(false);

    let result = fetch_pages(
        http,
        calendars,
        events,
        access,
        cal,
        time_min_rfc3339,
        time_max_rfc3339,
        now_rfc3339,
        acquired,
        &owner,
    )
    .await;

    if acquired {
        let _ = calendars.release_lease(&cal.id, &owner, now_rfc3339).await;
    }

    result
}

async fn fetch_pages(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    access: &GoogleAccess,
    cal: &GoogleCalendar,
    time_min_rfc3339: &str,
    time_max_rfc3339: &str,
    now_rfc3339: &str,
    write_through: bool,
    lease_owner: &str,
) -> Result<Vec<CalendarEvent>, CalendarError> {
    let mut page_token: Option<String> = None;
    let mut ephemeral: Vec<CalendarEvent> = Vec::new();
    // Stable synthetic ids for ephemeral rows (not persisted).
    let mut ephemeral_seq: u64 = 0;

    loop {
        let url = google_window_events_url(
            &cal.google_calendar_id,
            time_min_rfc3339,
            time_max_rfc3339,
            page_token.as_deref(),
        );
        let (status, body) = http.get_bearer_raw(&url, &access.access_token).await?;

        if status == 404 {
            return Err(CalendarError::GoogleNotFound);
        }
        if !(200..300).contains(&status) {
            // 410 / 401 / 403 / 429 / 5xx — no merge-full (we never sent a token).
            return Err(CalendarError::GoogleApi(format!(
                "google events.list returned {status}"
            )));
        }

        let page: EventsPage = serde_json::from_slice(&body).map_err(|err| {
            CalendarError::InvalidResponse(format!("events.list body: {err}"))
        })?;

        // Intentionally ignore page.next_sync_token — window must never publish.

        let items = page.items.unwrap_or_default();
        if write_through {
            let mut to_upsert = Vec::new();
            for item in &items {
                match classify_replica_item(item, &cal.id, now_rfc3339) {
                    ReplicaApplyAction::SoftDelete { google_event_id } => {
                        let deleted = events
                            .delete_by_google_event_id_if_owner(
                                &cal.id,
                                &google_event_id,
                                lease_owner,
                                now_rfc3339,
                            )
                            .await?;
                        if !deleted {
                            // Lost lease mid-window: stop write-through without
                            // erroring (not a listing failure). Prefer what is
                            // already in D1 over half-ephemeral pages.
                            return Ok(Vec::new());
                        }
                    }
                    ReplicaApplyAction::Upsert(row) => {
                        to_upsert.push(row);
                    }
                }
            }
            if !to_upsert.is_empty() {
                let applied = events
                    .upsert_batch_if_owner(to_upsert, lease_owner, now_rfc3339)
                    .await?;
                if !applied {
                    return Ok(Vec::new());
                }
            }
            let renew_expires = lease_expires_at(now_rfc3339);
            let renewed = calendars
                .renew_lease(&cal.id, lease_owner, &renew_expires, now_rfc3339)
                .await?;
            if !renewed {
                // Lost lease mid-walk: stop write-through; remaining pages (if
                // any) would need a re-fetch under a new owner. Prefer returning
                // what we already wrote via D1 rather than half-ephemeral.
                return Ok(Vec::new());
            }
        } else {
            for item in &items {
                match classify_replica_item(item, &cal.id, now_rfc3339) {
                    ReplicaApplyAction::SoftDelete { .. } => {
                        // No D1 row to delete; omit from ephemeral payload.
                    }
                    ReplicaApplyAction::Upsert(row) => {
                        ephemeral_seq += 1;
                        let id = format!("window-{}-{ephemeral_seq}", cal.id);
                        ephemeral.push(row_from_new_event(row, id, now_rfc3339));
                    }
                }
            }
        }

        if let Some(next) = page.next_page_token.filter(|t| !t.is_empty()) {
            page_token = Some(next);
            continue;
        }
        break;
    }

    if write_through {
        Ok(Vec::new())
    } else {
        Ok(ephemeral)
    }
}

/// Builds the window `events.list` URL.
///
/// Shape: `singleEvents=true&orderBy=startTime&maxResults=250&timeMin=&timeMax=`
/// plus optional `pageToken`. **No** `syncToken`, **no** `updatedMin`,
/// **no** `singleEvents=false`.
pub fn google_window_events_url(
    google_cal_id: &str,
    time_min_rfc3339: &str,
    time_max_rfc3339: &str,
    page_token: Option<&str>,
) -> String {
    let mut url = Url::parse(&format!(
        "{GOOGLE_EVENTS_BASE_URL}/{}/events",
        encode_path_segment(google_cal_id)
    ))
    .expect("static events URL is valid");
    url.query_pairs_mut()
        .append_pair("singleEvents", "true")
        .append_pair("orderBy", "startTime")
        .append_pair("maxResults", "250")
        .append_pair("timeMin", time_min_rfc3339)
        .append_pair("timeMax", time_max_rfc3339);
    if let Some(token) = page_token {
        url.query_pairs_mut().append_pair("pageToken", token);
    }
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_url_has_single_events_true_and_time_bounds() {
        let url = google_window_events_url(
            "primary@example.com",
            "2026-08-01T00:00:00Z",
            "2026-09-01T00:00:00Z",
            None,
        );
        assert!(url.contains("singleEvents=true"), "{url}");
        assert!(url.contains("orderBy=startTime"), "{url}");
        assert!(url.contains("maxResults=250"), "{url}");
        assert!(url.contains("timeMin=2026-08-01T00%3A00%3A00Z") || url.contains("timeMin=2026-08-01T00:00:00Z"), "{url}");
        assert!(url.contains("timeMax=2026-09-01T00%3A00%3A00Z") || url.contains("timeMax=2026-09-01T00:00:00Z"), "{url}");
        assert!(!url.contains("syncToken"), "{url}");
        assert!(!url.contains("singleEvents=false"), "{url}");
        assert!(!url.contains("updatedMin"), "{url}");
    }

    #[test]
    fn window_url_page_token_only_extra_cursor() {
        let url = google_window_events_url(
            "primary@example.com",
            "2026-08-01T00:00:00Z",
            "2026-09-01T00:00:00Z",
            Some("tok-2"),
        );
        assert!(url.contains("pageToken=tok-2"), "{url}");
        assert!(!url.contains("syncToken"), "{url}");
    }
}
