use super::labels::ensure_event_labels;
use super::sync::SyncErrorCode;
use super::watch::stop_watches_for_calendar;
use super::cron::persist_sync_failure;
use super::{
    CalendarError, CalendarView, CalendarsResponse, GOOGLE_CALENDAR_LIST_URL,
};
use crate::models::NewCalendar;
use crate::oauth::HttpClient;
use crate::repo::{CalendarRepo, WatchChannelRepo};
use crate::time::rfc3339_to_unix_secs;
use crate::token::GoogleAccess;
use serde::Deserialize;
use std::collections::HashSet;
use url::Url;

/// Lists the user's imported calendars for the picker
/// (`GET /api/calendar/calendars`).
///
/// Cache-first, like `list_events`: a non-empty store is served as-is — no
/// Google HTTP, no re-import. Incremental calendarList refresh is owned by
/// the fallback cron ([`run_fallback_cron`]) and preserves a deliberate
/// `sync_enabled = false`. An empty store runs the same first-contact
/// `calendarList` import `list_events` performs, then re-reads.
/// Event sync and watch channels are never touched here (first contact has
/// no channels to stop).
///
/// Rows are mapped to [`CalendarView`] (repo order: `is_primary DESC,
/// summary ASC`). Import failures propagate as [`CalendarError`] — nothing
/// is swallowed.
pub async fn list_calendars(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    access: &GoogleAccess,
    user_id: &str,
    now_rfc3339: &str,
) -> Result<CalendarsResponse, CalendarError> {
    let mut rows = calendars.list_by_user_id(user_id).await?;
    if rows.is_empty() {
        // First contact with Google: import the calendar list, then re-read.
        // No watches yet — pass None so we do not require a WatchChannelRepo.
        refresh_calendar_list(http, calendars, None, access, user_id, now_rfc3339).await?;
        rows = calendars.list_by_user_id(user_id).await?;
    }
    Ok(CalendarsResponse {
        calendars: rows.into_iter().map(CalendarView::from).collect(),
    })
}

/// Walks `/users/me/calendarList` (full or incremental) and merges into the
/// local calendar store.
///
/// - **Full** (no stored cursor, or merge-full after 410): pages without
///   `syncToken`. Living calendars absent from the union of all pages are
///   orphaned (disable + soft-delete + stop watches). Does **not** truncate
///   mid-failure.
/// - **Incremental** (stored non-empty sync token): `syncToken` +
///   `showDeleted=true`. `deleted: true` items disable + soft-delete + stop
///   watches. Absence is a no-op (do not orphan).
/// - Never advances the stored list cursor past uncommitted upserts/deletes.
/// - Never clobbers a living row's deliberate `sync_enabled = false` (SQL +
///   fake upsert contract).
/// - Returns local ids of calendars that were **not** living before this walk
///   (newly inserted or resurrected). Metadata-only updates do not count.
///
/// `watches` is `None` on first-contact import paths that have no channels
/// (`list_calendars`); cron and `list_events` pass `Some`.
pub(crate) async fn refresh_calendar_list(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    watches: Option<&dyn WatchChannelRepo>,
    access: &GoogleAccess,
    user_id: &str,
    now_rfc3339: &str,
) -> Result<Vec<String>, CalendarError> {
    let stored = calendars.get_calendar_list_sync_token(user_id).await?;
    let mut list_token: Option<String> = stored.filter(|t| !t.is_empty());
    let mut is_full = list_token.is_none();
    let mut page_token: Option<String> = None;
    let mut retried_410 = false;
    let mut seen_google_ids: HashSet<String> = HashSet::new();
    let mut new_local_ids: Vec<String> = Vec::new();

    // Snapshot living google ids before the walk so "new" is well-defined
    // even when upsert_batch updates in place.
    let living_before: HashSet<String> = calendars
        .list_by_user_id(user_id)
        .await?
        .into_iter()
        .map(|c| c.google_calendar_id)
        .collect();

    loop {
        let url = calendar_list_url(list_token.as_deref(), page_token.as_deref());
        let (status, body) = http
            .get_bearer_raw(&url, &access.access_token)
            .await?;

        if status == 410 {
            if retried_410 {
                return Err(CalendarError::GoogleApi(
                    "calendarList returned 410 twice".into(),
                ));
            }
            // Merge-full: drop cursor, keep living calendars, restart once.
            // Persist "" so a crash mid-restart retries as full (never leave
            // a fabricated success token).
            retried_410 = true;
            list_token = None;
            page_token = None;
            is_full = true;
            seen_google_ids.clear();
            calendars
                .set_calendar_list_sync_token(user_id, "", now_rfc3339)
                .await?;
            continue;
        }

        if !(200..300).contains(&status) {
            return Err(CalendarError::GoogleApi(format!(
                "calendarList fetch: google returned {status}"
            )));
        }

        let list: CalendarListResponse = serde_json::from_slice(&body).map_err(|err| {
            CalendarError::InvalidResponse(format!("calendarList body: {err}"))
        })?;

        // Apply this page before fetching the next (fence: never advance the
        // list cursor past uncommitted upserts).
        for item in list.items {
            if item.deleted {
                if let Some(cal) = calendars
                    .get_by_google_cal_id(user_id, &item.id)
                    .await?
                {
                    calendars
                        .set_sync_enabled(&cal.id, false, now_rfc3339)
                        .await?;
                    calendars.delete(&cal.id, now_rfc3339).await?;
                    if let Some(w) = watches {
                        // Best-effort stop: calendar is already disabled. A
                        // failed stop leaves channel rows for slice-4 retry.
                        let _ = stop_watches_for_calendar(http, w, access, &cal.id).await;
                    }
                }
                seen_google_ids.insert(item.id);
                continue;
            }

            let google_id = item.id;
            let was_new = !living_before.contains(&google_id);
            let access_role = item.access_role.unwrap_or_default();
            calendars
                .upsert_batch(vec![NewCalendar {
                    user_id: user_id.to_string(),
                    google_calendar_id: google_id.clone(),
                    summary: item.summary.unwrap_or_default(),
                    time_zone: item.time_zone.unwrap_or_default(),
                    is_primary: item.primary,
                    access_role: access_role.clone(),
                    sync_enabled: true,
                    sync_token: String::new(),
                    last_synced_at: None,
                }])
                .await?;
            seen_google_ids.insert(google_id.clone());

            if let Some(cal) = calendars
                .get_by_google_cal_id(user_id, &google_id)
                .await?
            {
                if was_new && !new_local_ids.iter().any(|id| id == &cal.id) {
                    new_local_ids.push(cal.id.clone());
                }
                if access_role == "freeBusyReader" {
                    let now_unix = rfc3339_to_unix_secs(now_rfc3339).unwrap_or(0);
                    // Stamp health only — do not request full_sync or start a
                    // replica walk (`replica_due` skips freeBusyReader).
                    persist_sync_failure(
                        calendars,
                        &cal,
                        SyncErrorCode::InsufficientAccess,
                        now_unix,
                        now_rfc3339,
                    )
                    .await?;
                }
            }
        }

        if let Some(next) = list.next_page_token.filter(|t| !t.is_empty()) {
            page_token = Some(next);
            continue;
        }

        // Terminal page: orphan only after a successful full / merge-full walk.
        if is_full {
            let living = calendars.list_by_user_id(user_id).await?;
            for cal in living {
                if !seen_google_ids.contains(&cal.google_calendar_id) {
                    calendars
                        .set_sync_enabled(&cal.id, false, now_rfc3339)
                        .await?;
                    calendars.delete(&cal.id, now_rfc3339).await?;
                    if let Some(w) = watches {
                        let _ = stop_watches_for_calendar(http, w, access, &cal.id).await;
                    }
                }
            }
        }

        // Commit nextSyncToken only after every page's upserts/deletes/orphans.
        if let Some(next_sync) = list.next_sync_token.filter(|t| !t.is_empty()) {
            calendars
                .set_calendar_list_sync_token(user_id, &next_sync, now_rfc3339)
                .await?;
        }
        // Missing terminal nextSyncToken: leave the stored cursor alone (or
        // "" after a 410 clear) so the next tick retries full/incremental.
        break;
    }

    // Backfill the event-label cache: newly imported rows start with an empty
    // `event_labels` (cache miss), so fetch + persist it right away.
    let imported = calendars.list_by_user_id(user_id).await?;
    for cal in &imported {
        if cal.event_labels.is_empty() {
            ensure_event_labels(http, calendars, access, cal, now_rfc3339).await?;
        }
    }
    Ok(new_local_ids)
}

/// Builds a `calendarList.list` URL.
///
/// Full (no token): bare list URL, optional `pageToken`.
/// Incremental: `syncToken` + `showDeleted=true`, optional `pageToken`.
/// Never pairs a stale syncToken with a full-list pageToken after a 410 —
/// the caller drops `list_token` before restarting.
fn calendar_list_url(sync_token: Option<&str>, page_token: Option<&str>) -> String {
    let mut url = Url::parse(GOOGLE_CALENDAR_LIST_URL).expect("static calendarList URL is valid");
    if let Some(token) = sync_token {
        url.query_pairs_mut()
            .append_pair("syncToken", token)
            .append_pair("showDeleted", "true");
    }
    if let Some(token) = page_token {
        url.query_pairs_mut().append_pair("pageToken", token);
    }
    url.to_string()
}

/// One entry from `/users/me/calendarList`.
#[derive(Debug, Deserialize)]
struct CalendarListEntry {
    id: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default, rename = "timeZone")]
    time_zone: Option<String>,
    #[serde(default)]
    primary: bool,
    #[serde(default, rename = "accessRole")]
    access_role: Option<String>,
    /// Present on incremental (`showDeleted=true`) responses when the user
    /// removed the calendar from their list.
    #[serde(default)]
    deleted: bool,
}

#[derive(Debug, Deserialize)]
struct CalendarListResponse {
    #[serde(default)]
    items: Vec<CalendarListEntry>,
    #[serde(default, rename = "nextPageToken")]
    next_page_token: Option<String>,
    #[serde(default, rename = "nextSyncToken")]
    next_sync_token: Option<String>,
}
