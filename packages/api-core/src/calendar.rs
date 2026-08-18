//! Calendar service: cached event listing (with Google sync) and event
//! creation, mirroring the old Go `handlers/calendar.go`.
//!
//! Pure Rust and unit-testable: Google HTTP calls go through [`HttpClient`],
//! persistence through [`CalendarRepo`]/[`CalendarEventRepo`], and "now" comes
//! from the caller — never `SystemTime`. The Worker layers session checks and
//! token refresh on top (`apps/worker/src/calendar.rs`).
//!
//! Sync rules (ADR 0001):
//! - `list_events` awaits a sync only when `last_synced_at` is missing or
//!   unparseable — i.e. the calendar has never synced (first paint after
//!   calendar import). Once `last_synced_at` is set, `list_events` is
//!   cache-only: no stale pull on the request path, whatever the age. The
//!   fallback cron (a later slice) reintroduces a time-based threshold.
//! - `events.list` uses `singleEvents=false&maxResults=250`, optionally with
//!   the stored `syncToken` (incremental), and follows `nextPageToken`.
//! - HTTP 410 (stale sync token) retries once with an empty token (full
//!   resync). HTTP 404 (e.g. holidays/birthdays calendars that don't support
//!   `events.list`) disables sync for that calendar. Other errors are logged
//!   (returned in `sync_errors`) and do not fail the whole listing.
//! - Incremental cancelled events (`status == "cancelled"`) are soft-deleted
//!   via `delete_by_google_event_id` (missing rows no-op) instead of being
//!   skipped. Events without a `start.dateTime`/`end.dateTime` (all-day
//!   events) are skipped — the old Go parser stored zero times for those,
//!   which is worse than skipping.

use serde::Deserialize;
use thiserror::Error;
use url::Url;

use crate::models::{CalendarEvent, GoogleCalendar, NewCalendar, NewCalendarEvent, NewEventInput};
use crate::oauth::{HttpClient, HttpError};
use crate::repo::{CalendarEventRepo, CalendarRepo, RepoError};
use crate::time::{add_months_unix, rfc3339_to_unix_secs, unix_secs_to_rfc3339};
use crate::token::GoogleAccess;

/// A calendar is stale (needs a sync) when it has not synced in this many
/// seconds (5 minutes, same as Go's `syncStaleThreshold`).
///
/// The request path no longer uses this — `list_events` is cache-only once
/// `last_synced_at` is set (ADR 0001). The fallback cron (a later slice) will
/// use a 15-minute threshold; kept here as the shared constant for that job.
pub const SYNC_STALE_THRESHOLD_SECS: i64 = 5 * 60;

/// Google Calendar API endpoints.
pub const GOOGLE_CALENDAR_LIST_URL: &str =
    "https://www.googleapis.com/calendar/v3/users/me/calendarList";
pub const GOOGLE_EVENTS_BASE_URL: &str = "https://www.googleapis.com/calendar/v3/calendars";

/// Errors produced by the calendar service.
#[derive(Debug, Clone, Error)]
pub enum CalendarError {
    #[error("{0}")]
    InvalidRange(String),
    #[error("calendar not found")]
    NotFound,
    /// Google returned 404 for `events.list` (calendar does not support it).
    #[error("google returned 404 for events.list")]
    GoogleNotFound,
    #[error("google api error: {0}")]
    GoogleApi(String),
    #[error("invalid google response: {0}")]
    InvalidResponse(String),
    #[error("http request failed: {0}")]
    Http(#[from] HttpError),
    #[error("database error: {0}")]
    Repo(#[from] RepoError),
}

/// Result of [`list_events`]: the cached events plus per-calendar sync errors
/// for the caller to log (sync failures never fail the whole listing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarListOutput {
    pub events: Vec<CalendarEvent>,
    /// Human-readable sync failures; empty when every calendar synced fine.
    pub sync_errors: Vec<String>,
}

/// Result of [`create_event`]: the created event plus the response source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateEventOutput {
    pub event: CalendarEvent,
    pub source: String,
    /// Set when the local cache upsert failed (logged, never fatal).
    pub cache_error: Option<String>,
}

/// Response envelope for `GET /api/calendar/events`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CalendarEventsResponse {
    pub events: Vec<CalendarEvent>,
    pub source: String,
}

/// Response envelope for `POST /api/calendar/events`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CreateEventResponse {
    pub event: CalendarEvent,
    pub source: String,
}

/// Resolves the event time window from optional `time_min`/`time_max` query
/// params (RFC 3339, with or without fractional seconds).
///
/// Defaults to `now − 1 month … now + 2 months` (UTC) when a bound is missing,
/// and requires `time_max` to be strictly after `time_min`. Returns normalized
/// RFC 3339 UTC strings.
pub fn parse_event_time_range(
    time_min: Option<&str>,
    time_max: Option<&str>,
    now_unix: i64,
) -> Result<(String, String), CalendarError> {
    let mut start_unix = add_months_unix(now_unix, -1);
    let mut end_unix = add_months_unix(now_unix, 2);

    if let Some(value) = time_min.filter(|value| !value.is_empty()) {
        start_unix = rfc3339_to_unix_secs(value)
            .ok_or_else(|| CalendarError::InvalidRange("invalid time_min: must be RFC 3339".into()))?;
    }
    if let Some(value) = time_max.filter(|value| !value.is_empty()) {
        end_unix = rfc3339_to_unix_secs(value)
            .ok_or_else(|| CalendarError::InvalidRange("invalid time_max: must be RFC 3339".into()))?;
    }
    if end_unix <= start_unix {
        return Err(CalendarError::InvalidRange("time_max must be after time_min".into()));
    }

    Ok((
        unix_secs_to_rfc3339(start_unix),
        unix_secs_to_rfc3339(end_unix),
    ))
}

/// Lists the user's cached events, syncing each stale calendar from Google
/// first (see module docs for the sync rules).
pub async fn list_events(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    access: &GoogleAccess,
    user_id: &str,
    start_rfc3339: &str,
    end_rfc3339: &str,
    now_unix: i64,
) -> Result<CalendarListOutput, CalendarError> {
    let mut cals = calendars.list_by_user_id(user_id).await?;
    if cals.is_empty() {
        // First contact with Google: import the calendar list, then re-read.
        refresh_calendar_list(http, calendars, access, user_id).await?;
        cals = calendars.list_by_user_id(user_id).await?;
    }

    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);
    let mut sync_errors: Vec<String> = Vec::new();
    for cal in &cals {
        if !cal.sync_enabled {
            continue;
        }
        // Cache-only after first paint (ADR 0001): a set, parseable
        // `last_synced_at` means the sync cursor exists, so never pull on the
        // request path — regardless of age. Missing or unparseable means the
        // calendar has never synced: await the first sync.
        let previously_synced = cal
            .last_synced_at
            .as_deref()
            .and_then(rfc3339_to_unix_secs)
            .is_some();
        if previously_synced {
            continue;
        }
        match sync_calendar(http, calendars, events, access, cal, &now_rfc3339).await {
            Ok(()) => {}
            Err(CalendarError::GoogleNotFound) => {
                sync_errors.push(format!(
                    "calendar {} ({}) returned 404 — disabling sync",
                    cal.id, cal.google_calendar_id
                ));
                if let Err(err) = calendars.set_sync_enabled(&cal.id, false, &now_rfc3339).await {
                    sync_errors.push(format!(
                        "failed to disable sync for calendar {}: {err}",
                        cal.id
                    ));
                }
            }
            Err(err) => sync_errors.push(format!(
                "sync failed for calendar {} ({}): {err}",
                cal.id, cal.google_calendar_id
            )),
        }
    }

    let cached = events
        .list_by_user_id_and_time_range(user_id, start_rfc3339, end_rfc3339)
        .await?;
    Ok(CalendarListOutput {
        events: cached,
        sync_errors,
    })
}

/// Creates an event on Google (`events.insert`) and upserts the returned row
/// into the local cache. A cache failure is logged (returned in
/// [`CreateEventOutput::cache_error`]), never fatal.
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
    let payload = serde_json::json!({
        "summary": input.summary,
        "description": input.description,
        "start": { "dateTime": input.start },
        "end": { "dateTime": input.end },
    });
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

/// Imports `/users/me/calendarList` and upserts each entry (all imported
/// calendars default to `sync_enabled = true`).
async fn refresh_calendar_list(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    access: &GoogleAccess,
    user_id: &str,
) -> Result<(), CalendarError> {
    let (status, body) = http
        .get_bearer_raw(GOOGLE_CALENDAR_LIST_URL, &access.access_token)
        .await?;
    if !(200..300).contains(&status) {
        return Err(CalendarError::GoogleApi(format!(
            "calendarList fetch: google returned {status}"
        )));
    }
    let list: CalendarListResponse = serde_json::from_slice(&body)
        .map_err(|err| CalendarError::InvalidResponse(format!("calendarList body: {err}")))?;

    let rows: Vec<NewCalendar> = list
        .items
        .into_iter()
        .map(|item| NewCalendar {
            user_id: user_id.to_string(),
            google_calendar_id: item.id,
            summary: item.summary.unwrap_or_default(),
            time_zone: item.time_zone.unwrap_or_default(),
            is_primary: item.primary,
            access_role: item.access_role.unwrap_or_default(),
            sync_enabled: true,
            sync_token: String::new(),
            last_synced_at: None,
        })
        .collect();
    if !rows.is_empty() {
        calendars.upsert_batch(rows).await?;
    }
    Ok(())
}

/// Full or incremental sync of one calendar: fetch events (following
/// `nextPageToken`, retrying once on 410), upsert them, and store the new
/// sync cursor.
async fn sync_calendar(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    access: &GoogleAccess,
    cal: &GoogleCalendar,
    now_rfc3339: &str,
) -> Result<(), CalendarError> {
    let mut all_items: Vec<GoogleEvent> = Vec::new();
    let mut next_sync_token: Option<String> = None;
    let mut sync_token = if cal.sync_token.is_empty() {
        None
    } else {
        Some(cal.sync_token.as_str())
    };
    let mut page_token: Option<String> = None;
    let mut retried_resync = false;

    loop {
        let url = google_events_url(&cal.google_calendar_id, sync_token, page_token.as_deref());
        let (status, body) = http.get_bearer_raw(&url, &access.access_token).await?;
        if status == 410 && !retried_resync {
            // Stale sync token: drop it and the paging cursor, restart with a
            // full resync (same as Go's recursive fetchGoogleEvents retry).
            retried_resync = true;
            sync_token = None;
            page_token = None;
            all_items.clear();
            continue;
        }
        if status == 404 {
            return Err(CalendarError::GoogleNotFound);
        }
        if !(200..300).contains(&status) {
            return Err(CalendarError::GoogleApi(format!(
                "google events.list returned {status}"
            )));
        }
        let page: EventsPage = serde_json::from_slice(&body)
            .map_err(|err| CalendarError::InvalidResponse(format!("events.list body: {err}")))?;
        if let Some(items) = page.items {
            all_items.extend(items);
        }
        if let Some(token) = page.next_sync_token {
            next_sync_token = Some(token);
        }
        page_token = page.next_page_token;
        if page_token.is_none() {
            break;
        }
    }

    let mut to_upsert: Vec<NewCalendarEvent> = Vec::new();
    for item in &all_items {
        if item.status.as_deref() == Some("cancelled") {
            // Incremental cancelled event: soft-delete the cached row (a no-op
            // when the row was never cached, e.g. an all-day event). A delete
            // failure propagates so the sync token is not advanced after a
            // partial apply.
            events
                .delete_by_google_event_id(&cal.id, &item.id, now_rfc3339)
                .await?;
            continue;
        }
        if is_skipped(item) {
            continue;
        }
        to_upsert.push(map_google_event(item, &cal.id, now_rfc3339));
    }
    if !to_upsert.is_empty() {
        events.upsert_batch(to_upsert, now_rfc3339).await?;
    }

    // Keep the previous sync token when Google omitted nextSyncToken.
    let next_token = next_sync_token.unwrap_or_else(|| cal.sync_token.clone());
    calendars
        .update_sync_state(&cal.id, &next_token, now_rfc3339)
        .await?;
    Ok(())
}

/// Builds the `events.list` URL for a calendar, percent-encoding the Google
/// calendar id (ids like `en.usa#holiday@group.v.calendar.google.com` contain
/// `#`). Query params are appended via `url::Url` so sync tokens (which often
/// contain `=`) are properly encoded.
fn google_events_url(
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

/// RFC 3986 percent-encoding for a URL path segment (calendar ids may contain
/// `#` and other reserved characters).
fn encode_path_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Whether a Google event should be skipped during sync: events without
/// `start.dateTime`/`end.dateTime` (e.g. all-day events, which use
/// `start.date` instead). The old Go parser stored zero times for those —
/// skipping is cleaner. Cancelled events are NOT skipped here; they are
/// soft-deleted first in [`sync_calendar`].
fn is_skipped(event: &GoogleEvent) -> bool {
    let has_date_time =
        |time: &Option<GoogleEventTime>| matches!(time, Some(t) if !t.date_time.as_deref().unwrap_or("").is_empty());
    !(has_date_time(&event.start) && has_date_time(&event.end))
}

/// Converts a Google event API response into the local cache model.
fn map_google_event(event: &GoogleEvent, calendar_id: &str, now_rfc3339: &str) -> NewCalendarEvent {
    NewCalendarEvent {
        calendar_id: calendar_id.to_string(),
        google_event_id: event.id.clone(),
        google_etag: event.etag.clone().unwrap_or_default(),
        google_updated_at: event.updated.clone().unwrap_or_default(),
        last_synced_at: now_rfc3339.to_string(),
        title: event.summary.clone().unwrap_or_default(),
        description: event.description.clone().unwrap_or_default(),
        start_time: event
            .start
            .as_ref()
            .and_then(|time| time.date_time.clone())
            .unwrap_or_default(),
        end_time: event
            .end
            .as_ref()
            .and_then(|time| time.date_time.clone())
            .unwrap_or_default(),
        recurrence: event
            .recurrence
            .as_ref()
            .map(|rules| serde_json::to_string(rules).unwrap_or_default())
            .unwrap_or_default(),
    }
}

/// Builds the full DB-shaped [`CalendarEvent`] (for API responses) from the
/// upsert input plus the generated id.
fn row_from_new_event(event: NewCalendarEvent, id: String, now_rfc3339: &str) -> CalendarEvent {
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
        created_at: now_rfc3339.to_string(),
        updated_at: now_rfc3339.to_string(),
        deleted_at: None,
    }
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
}

#[derive(Debug, Deserialize)]
struct CalendarListResponse {
    #[serde(default)]
    items: Vec<CalendarListEntry>,
}

/// A Google Calendar event as returned by `events.list` / `events.insert`.
#[derive(Debug, Deserialize)]
struct GoogleEvent {
    id: String,
    #[serde(default)]
    etag: Option<String>,
    #[serde(default)]
    updated: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    recurrence: Option<Vec<String>>,
    #[serde(default)]
    start: Option<GoogleEventTime>,
    #[serde(default)]
    end: Option<GoogleEventTime>,
}

/// `start`/`end` of a Google event; all-day events carry `date` instead of
/// `dateTime` and are skipped by the sync.
#[derive(Debug, Deserialize)]
struct GoogleEventTime {
    #[serde(default, rename = "dateTime")]
    date_time: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EventsPage {
    #[serde(default)]
    items: Option<Vec<GoogleEvent>>,
    #[serde(default, rename = "nextSyncToken")]
    next_sync_token: Option<String>,
    #[serde(default, rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::models::{GoogleCalendar, NewCalendar};

    // ──────────────────────────────────────────
    // Fakes
    // ──────────────────────────────────────────

    /// Scripted HTTP fake: `routes` are `(url-substring, status, body)` in
    /// match order; every call is recorded for assertions.
    struct FakeHttp {
        routes: Vec<(String, u16, String)>,
        gets: Mutex<Vec<String>>,
        posts: Mutex<Vec<(String, String)>>,
    }

    impl FakeHttp {
        fn new(routes: Vec<(&str, u16, &str)>) -> Self {
            Self {
                routes: routes
                    .into_iter()
                    .map(|(substr, status, body)| {
                        (substr.to_string(), status, body.to_string())
                    })
                    .collect(),
                gets: Mutex::new(Vec::new()),
                posts: Mutex::new(Vec::new()),
            }
        }

        fn route(&self, url: &str) -> (u16, Vec<u8>) {
            for (substr, status, body) in &self.routes {
                if url.contains(substr) {
                    return (*status, body.clone().into_bytes());
                }
            }
            panic!("no route for {url}");
        }
    }

    #[async_trait::async_trait(?Send)]
    impl HttpClient for FakeHttp {
        async fn post_form(&self, _url: &str, _form: &[(&str, &str)]) -> Result<Vec<u8>, HttpError> {
            Ok(Vec::new())
        }

        async fn get_bearer(&self, _url: &str, _token: &str) -> Result<Vec<u8>, HttpError> {
            Ok(Vec::new())
        }

        async fn get_bearer_raw(
            &self,
            url: &str,
            _token: &str,
        ) -> Result<(u16, Vec<u8>), HttpError> {
            self.gets.lock().unwrap().push(url.to_string());
            Ok(self.route(url))
        }

        async fn post_json(
            &self,
            url: &str,
            _token: &str,
            body: &[u8],
        ) -> Result<(u16, Vec<u8>), HttpError> {
            self.posts
                .lock()
                .unwrap()
                .push((url.to_string(), String::from_utf8_lossy(body).to_string()));
            Ok(self.route(url))
        }
    }

    /// In-memory calendar repo: `upsert_batch` materializes rows (like D1), so
    /// the service's re-read after a calendarList import sees the new rows.
    struct FakeCalendarRepo {
        stored: Mutex<Vec<GoogleCalendar>>,
        upserted: Mutex<Vec<NewCalendar>>,
        sync_states: Mutex<Vec<(String, String, String)>>,
        disabled: Mutex<Vec<(String, bool)>>,
        next_id: Mutex<u64>,
    }

    impl FakeCalendarRepo {
        fn with(calendars: Vec<GoogleCalendar>) -> Self {
            Self {
                stored: Mutex::new(calendars),
                upserted: Mutex::new(Vec::new()),
                sync_states: Mutex::new(Vec::new()),
                disabled: Mutex::new(Vec::new()),
                next_id: Mutex::new(1),
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl CalendarRepo for FakeCalendarRepo {
        async fn list_by_user_id(&self, _user_id: &str) -> Result<Vec<GoogleCalendar>, RepoError> {
            Ok(self.stored.lock().unwrap().clone())
        }

        async fn get_by_id(&self, id: &str) -> Result<Option<GoogleCalendar>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|cal| cal.id == id)
                .cloned())
        }

        async fn get_by_google_cal_id(
            &self,
            _user_id: &str,
            google_cal_id: &str,
        ) -> Result<Option<GoogleCalendar>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|cal| cal.google_calendar_id == google_cal_id)
                .cloned())
        }

        async fn upsert(&self, calendar: NewCalendar) -> Result<(), RepoError> {
            self.upsert_batch(vec![calendar]).await
        }

        async fn upsert_batch(&self, calendars: Vec<NewCalendar>) -> Result<(), RepoError> {
            for cal in calendars {
                let mut next = self.next_id.lock().unwrap();
                let row = GoogleCalendar {
                    id: format!("cal-{next}"),
                    user_id: cal.user_id.clone(),
                    google_calendar_id: cal.google_calendar_id.clone(),
                    summary: cal.summary.clone(),
                    time_zone: cal.time_zone.clone(),
                    is_primary: cal.is_primary,
                    access_role: cal.access_role.clone(),
                    sync_enabled: cal.sync_enabled,
                    sync_token: cal.sync_token.clone(),
                    last_synced_at: cal.last_synced_at.clone(),
                    created_at: "2026-08-17T00:00:00Z".to_string(),
                    updated_at: "2026-08-17T00:00:00Z".to_string(),
                    deleted_at: None,
                };
                *next += 1;
                self.upserted.lock().unwrap().push(cal);
                self.stored.lock().unwrap().push(row);
            }
            Ok(())
        }

        async fn update_sync_state(
            &self,
            id: &str,
            sync_token: &str,
            last_synced_at_rfc3339: &str,
        ) -> Result<(), RepoError> {
            self.sync_states.lock().unwrap().push((
                id.to_string(),
                sync_token.to_string(),
                last_synced_at_rfc3339.to_string(),
            ));
            Ok(())
        }

        async fn set_sync_enabled(
            &self,
            id: &str,
            enabled: bool,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            self.disabled.lock().unwrap().push((id.to_string(), enabled));
            Ok(())
        }

        async fn delete(&self, _id: &str, _now_rfc3339: &str) -> Result<(), RepoError> {
            Ok(())
        }
    }

    /// In-memory event repo: upserts materialize rows so the follow-up
    /// time-range query returns them, and every call is recorded.
    struct FakeEventRepo {
        stored: Mutex<Vec<CalendarEvent>>,
        upserted_batch: Mutex<Vec<NewCalendarEvent>>,
        upserted_single: Mutex<Option<(String, NewCalendarEvent)>>,
        ranged: Mutex<Vec<(String, String, String)>>,
        deleted_by_google_event_id: Mutex<Vec<(String, String)>>,
        fail_upsert: Mutex<bool>,
        fail_delete: Mutex<bool>,
        next_id: Mutex<u64>,
    }

    impl FakeEventRepo {
        fn new() -> Self {
            Self {
                stored: Mutex::new(Vec::new()),
                upserted_batch: Mutex::new(Vec::new()),
                upserted_single: Mutex::new(None),
                ranged: Mutex::new(Vec::new()),
                deleted_by_google_event_id: Mutex::new(Vec::new()),
                fail_upsert: Mutex::new(false),
                fail_delete: Mutex::new(false),
                next_id: Mutex::new(1),
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl CalendarEventRepo for FakeEventRepo {
        async fn upsert(
            &self,
            event: NewCalendarEvent,
            now_rfc3339: &str,
        ) -> Result<String, RepoError> {
            if *self.fail_upsert.lock().unwrap() {
                return Err(RepoError::Backend("cache write failed".into()));
            }
            *self.upserted_single.lock().unwrap() = Some((event.google_event_id.clone(), event.clone()));
            let id = "created-id".to_string();
            self.stored.lock().unwrap().push(row_from_new_event(
                event,
                id.clone(),
                now_rfc3339,
            ));
            Ok(id)
        }

        async fn upsert_batch(
            &self,
            events: Vec<NewCalendarEvent>,
            now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            self.upserted_batch.lock().unwrap().extend(events.clone());
            let mut next = self.next_id.lock().unwrap();
            for event in events {
                self.stored.lock().unwrap().push(row_from_new_event(
                    event,
                    format!("evt-{next}"),
                    now_rfc3339,
                ));
                *next += 1;
            }
            Ok(())
        }

        async fn get_by_id(&self, _id: &str) -> Result<Option<CalendarEvent>, RepoError> {
            Ok(None)
        }

        async fn list_by_user_id_and_time_range(
            &self,
            user_id: &str,
            start_rfc3339: &str,
            end_rfc3339: &str,
        ) -> Result<Vec<CalendarEvent>, RepoError> {
            self.ranged
                .lock()
                .unwrap()
                .push((user_id.to_string(), start_rfc3339.to_string(), end_rfc3339.to_string()));
            Ok(self.stored.lock().unwrap().clone())
        }

        async fn delete(&self, _id: &str, _now_rfc3339: &str) -> Result<(), RepoError> {
            Ok(())
        }

        async fn delete_by_google_event_id(
            &self,
            calendar_id: &str,
            google_event_id: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            self.deleted_by_google_event_id
                .lock()
                .unwrap()
                .push((calendar_id.to_string(), google_event_id.to_string()));
            if *self.fail_delete.lock().unwrap() {
                return Err(RepoError::Backend("cache delete failed".into()));
            }
            Ok(())
        }

        async fn delete_stale(
            &self,
            _calendar_id: &str,
            _older_than_rfc3339: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            Ok(())
        }
    }

    // ──────────────────────────────────────────
    // Fixtures
    // ──────────────────────────────────────────

    const NOW_UNIX: i64 = 1_700_000_000; // 2023-11-14T22:13:20Z

    fn access() -> GoogleAccess {
        GoogleAccess {
            access_token: "at-1".to_string(),
            token_type: "Bearer".to_string(),
        }
    }

    fn calendar(id: &str, google_cal_id: &str, sync_enabled: bool) -> GoogleCalendar {
        GoogleCalendar {
            id: id.to_string(),
            user_id: "u-1".to_string(),
            google_calendar_id: google_cal_id.to_string(),
            summary: "Work".to_string(),
            time_zone: "UTC".to_string(),
            is_primary: true,
            access_role: "owner".to_string(),
            sync_enabled,
            sync_token: String::new(),
            last_synced_at: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            deleted_at: None,
        }
    }

    const CALENDAR_LIST_JSON: &str = r#"{
        "items": [
            {"id": "primary@example.com", "summary": "Work", "timeZone": "UTC", "primary": true, "accessRole": "owner"},
            {"id": "en.usa#holiday@group.v.calendar.google.com", "summary": "Holidays", "primary": false, "accessRole": "reader"}
        ]
    }"#;

    const EVENTS_JSON: &str = r#"{
        "items": [
            {"id": "evt-1", "etag": "e1", "updated": "2026-08-17T10:00:00.000Z",
             "summary": "Standup", "description": "Daily",
             "start": {"dateTime": "2026-08-18T09:00:00Z"},
             "end": {"dateTime": "2026-08-18T09:30:00Z"},
             "recurrence": ["RRULE:FREQ=DAILY"]},
            {"id": "evt-2", "summary": "Lunch",
             "start": {"dateTime": "2026-08-18T12:00:00Z"},
             "end": {"dateTime": "2026-08-18T13:00:00Z"}}
        ],
        "nextSyncToken": "st-9"
    }"#;

    // ──────────────────────────────────────────
    // parse_event_time_range
    // ──────────────────────────────────────────

    #[test]
    fn default_window_is_minus_one_month_to_plus_two_months() {
        let (start, end) = parse_event_time_range(None, None, NOW_UNIX).unwrap();
        // 2023-11-14T22:13:20Z − 1 month / + 2 months.
        assert_eq!(start, "2023-10-14T22:13:20Z");
        assert_eq!(end, "2024-01-14T22:13:20Z");
    }

    #[test]
    fn explicit_bounds_are_normalized() {
        let (start, end) = parse_event_time_range(
            Some("2026-08-01T00:00:00.500Z"),
            Some("2026-09-01T00:00:00+00:00"),
            NOW_UNIX,
        )
        .unwrap();
        assert_eq!(start, "2026-08-01T00:00:00Z", "fraction truncated");
        assert_eq!(end, "2026-09-01T00:00:00Z", "offset normalized to UTC");
    }

    #[test]
    fn empty_bounds_fall_back_to_defaults() {
        let (start, end) = parse_event_time_range(Some(""), Some(""), NOW_UNIX).unwrap();
        assert_eq!(start, "2023-10-14T22:13:20Z");
        assert_eq!(end, "2024-01-14T22:13:20Z");
    }

    #[test]
    fn invalid_bounds_are_rejected() {
        let err = parse_event_time_range(Some("not-a-date"), None, NOW_UNIX).unwrap_err();
        assert!(err.to_string().contains("time_min"), "{err}");

        let err = parse_event_time_range(None, Some("nope"), NOW_UNIX).unwrap_err();
        assert!(err.to_string().contains("time_max"), "{err}");

        let err = parse_event_time_range(
            Some("2026-09-01T00:00:00Z"),
            Some("2026-08-01T00:00:00Z"),
            NOW_UNIX,
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "time_max must be after time_min");

        let err = parse_event_time_range(
            Some("2026-08-01T00:00:00Z"),
            Some("2026-08-01T00:00:00Z"),
            NOW_UNIX,
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "time_max must be after time_min");
    }

    // ──────────────────────────────────────────
    // list_events
    // ──────────────────────────────────────────

    #[test]
    fn empty_calendars_imports_calendar_list_before_serving_cache() {
        let http = FakeHttp::new(vec![
            ("calendarList", 200, CALENDAR_LIST_JSON),
            ("/events", 200, r#"{"items":[],"nextSyncToken":"st-1"}"#),
        ]);
        let calendars = FakeCalendarRepo::with(vec![]);
        let events = FakeEventRepo::new();

        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &access(), "u-1", "2026-08-01T00:00:00Z",
            "2026-09-01T00:00:00Z", NOW_UNIX,
        ))
        .unwrap();

        assert!(output.events.is_empty());
        assert!(output.sync_errors.is_empty());

        // calendarList fetched, rows upserted (sync_enabled defaults true).
        let gets = http.gets.lock().unwrap();
        assert_eq!(gets.len(), 3, "calendarList + one events.list per imported calendar");
        assert!(gets[0].contains("calendarList"), "{gets:?}");
        let upserted = calendars.upserted.lock().unwrap();
        assert_eq!(upserted.len(), 2);
        assert!(upserted.iter().all(|cal| cal.sync_enabled));
        assert!(upserted.iter().any(|cal| cal.is_primary));
        assert_eq!(upserted[1].google_calendar_id, "en.usa#holiday@group.v.calendar.google.com");

        // Freshly imported calendars have never synced (stale), so the same
        // request also syncs them (Go behavior: refreshCalendarList then the
        // staleness loop). The encoded `#`/`@` show in the events.list URLs.
        assert!(gets[1].contains("primary%40example.com/events"), "{gets:?}");
        assert!(gets[2].contains("en.usa%23holiday%40group.v.calendar.google.com/events"), "{gets:?}");
        let states = calendars.sync_states.lock().unwrap();
        assert_eq!(states.len(), 2, "both imported calendars synced");
        assert!(states.iter().all(|(_, token, _)| token == "st-1"));
    }

    #[test]
    fn never_synced_calendar_is_synced_before_the_cache_query() {
        // `calendar()` defaults to `last_synced_at: None` — first paint, so
        // the sync is awaited before the cache query.
        let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &access(), "u-1", "2026-08-01T00:00:00Z",
            "2026-09-01T00:00:00Z", NOW_UNIX,
        ))
        .unwrap();

        // Sync fetched events, then the time-range query returned them.
        let gets = http.gets.lock().unwrap();
        assert_eq!(gets.len(), 1);
        assert!(gets[0].contains("/calendars/primary%40example.com/events"), "{gets:?}");
        assert!(gets[0].contains("singleEvents=false"), "{gets:?}");
        assert!(gets[0].contains("maxResults=250"), "{gets:?}");

        // Both items upserted with the sync timestamp and this calendar.
        let upserted = events.upserted_batch.lock().unwrap();
        assert_eq!(upserted.len(), 2);
        assert!(upserted.iter().all(|event| event.calendar_id == "cal-1"));
        assert!(upserted.iter().all(|event| event.last_synced_at == "2023-11-14T22:13:20Z"));
        assert_eq!(upserted[0].title, "Standup");
        assert_eq!(upserted[0].start_time, "2026-08-18T09:00:00Z");
        assert_eq!(upserted[0].recurrence, r#"["RRULE:FREQ=DAILY"]"#);

        // Sync state advanced with Google's nextSyncToken.
        let states = calendars.sync_states.lock().unwrap();
        assert_eq!(*states, vec![("cal-1".to_string(), "st-9".to_string(), "2023-11-14T22:13:20Z".to_string())]);

        // Overlap query used the parsed window.
        let ranged = events.ranged.lock().unwrap();
        assert_eq!(*ranged, vec![("u-1".to_string(), "2026-08-01T00:00:00Z".to_string(), "2026-09-01T00:00:00Z".to_string())]);

        assert_eq!(output.events.len(), 2);
        assert!(output.sync_errors.is_empty());
    }

    #[test]
    fn previously_synced_calendar_is_not_synced_even_if_old() {
        let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
        let mut cal = calendar("cal-1", "primary@example.com", true);
        // Days old — stale under the old 5-minute rule, but the request path
        // is cache-only once `last_synced_at` is set (ADR 0001).
        cal.last_synced_at = Some("2023-11-10T00:00:00Z".to_string());
        let calendars = FakeCalendarRepo::with(vec![cal]);
        let events = FakeEventRepo::new();

        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &access(), "u-1", "2026-08-01T00:00:00Z",
            "2026-09-01T00:00:00Z", NOW_UNIX,
        ))
        .unwrap();

        assert!(http.gets.lock().unwrap().is_empty(), "no Google calls for a previously synced calendar");
        assert!(events.upserted_batch.lock().unwrap().is_empty());
        assert!(calendars.sync_states.lock().unwrap().is_empty(), "sync state untouched");
        assert!(output.sync_errors.is_empty());
    }

    #[test]
    fn sync_disabled_calendar_is_not_synced() {
        let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", false)]);
        let events = FakeEventRepo::new();

        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &access(), "u-1", "2026-08-01T00:00:00Z",
            "2026-09-01T00:00:00Z", NOW_UNIX,
        ))
        .unwrap();

        assert!(http.gets.lock().unwrap().is_empty());
        assert!(output.events.is_empty());
        assert!(output.sync_errors.is_empty());
    }

    #[test]
    fn events_list_404_disables_sync_but_serves_cache() {
        let http = FakeHttp::new(vec![("/events", 404, r#"{"error":"not found"}"#)]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "holidays", true)]);
        let events = FakeEventRepo::new();

        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &access(), "u-1", "2026-08-01T00:00:00Z",
            "2026-09-01T00:00:00Z", NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(
            *calendars.disabled.lock().unwrap(),
            vec![("cal-1".to_string(), false)]
        );
        assert_eq!(output.sync_errors.len(), 1);
        assert!(output.sync_errors[0].contains("404"), "{}", output.sync_errors[0]);
        assert!(output.events.is_empty(), "cache still served");
    }

    #[test]
    fn events_list_410_retries_without_sync_token() {
        let http = FakeHttp::new(vec![
            ("syncToken=stale-token", 410, ""),
            ("/events", 200, EVENTS_JSON),
        ]);
        let mut cal = calendar("cal-1", "primary@example.com", true);
        cal.sync_token = "stale-token".to_string();
        let calendars = FakeCalendarRepo::with(vec![cal]);
        let events = FakeEventRepo::new();

        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &access(), "u-1", "2026-08-01T00:00:00Z",
            "2026-09-01T00:00:00Z", NOW_UNIX,
        ))
        .unwrap();

        let gets = http.gets.lock().unwrap();
        assert_eq!(gets.len(), 2, "410 then full resync");
        assert!(gets[0].contains("syncToken=stale-token"), "{gets:?}");
        assert!(!gets[1].contains("syncToken"), "{gets:?}");
        assert_eq!(output.events.len(), 2, "resync populated the cache");
        assert!(output.sync_errors.is_empty());

        // New sync token stored after the resync.
        let states = calendars.sync_states.lock().unwrap();
        assert_eq!(states[0].1, "st-9");
    }

    #[test]
    fn events_list_410_without_sync_token_is_an_error_not_an_infinite_loop() {
        let http = FakeHttp::new(vec![("/events", 410, "")]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &access(), "u-1", "2026-08-01T00:00:00Z",
            "2026-09-01T00:00:00Z", NOW_UNIX,
        ))
        .unwrap();

        // One 410 triggers the single retry; the second 410 errors out.
        assert_eq!(http.gets.lock().unwrap().len(), 2);
        assert_eq!(output.sync_errors.len(), 1);
        assert!(output.sync_errors[0].contains("410"), "{}", output.sync_errors[0]);
    }

    #[test]
    fn events_list_follows_next_page_token() {
        let page_one = r#"{"items":[{"id":"p1","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}}],"nextPageToken":"tok-2"}"#;
        let page_two = r#"{"items":[{"id":"p2","start":{"dateTime":"2026-08-18T10:00:00Z"},"end":{"dateTime":"2026-08-18T10:30:00Z"}}]}"#;
        let http = FakeHttp::new(vec![
            ("pageToken=tok-2", 200, page_two),
            ("/events", 200, page_one),
        ]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &access(), "u-1", "2026-08-01T00:00:00Z",
            "2026-09-01T00:00:00Z", NOW_UNIX,
        ))
        .unwrap();

        let gets = http.gets.lock().unwrap();
        assert_eq!(gets.len(), 2);
        assert!(gets[1].contains("pageToken=tok-2"), "{gets:?}");
        assert_eq!(events.upserted_batch.lock().unwrap().len(), 2);
        assert_eq!(output.events.len(), 2);
    }

    #[test]
    fn all_day_and_no_time_events_are_skipped() {
        let body = r#"{"items":[
            {"id": "all-day", "start": {"date": "2026-08-01"}, "end": {"date": "2026-08-02"}},
            {"id": "no-time", "summary": "No times at all"},
            {"id": "real", "summary": "Real",
             "start": {"dateTime": "2026-08-18T09:00:00Z"}, "end": {"dateTime": "2026-08-18T09:30:00Z"}}
        ], "nextSyncToken": "st-9"}"#;
        let http = FakeHttp::new(vec![("/events", 200, body)]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &access(), "u-1", "2026-08-01T00:00:00Z",
            "2026-09-01T00:00:00Z", NOW_UNIX,
        ))
        .unwrap();

        let upserted = events.upserted_batch.lock().unwrap();
        assert_eq!(upserted.len(), 1, "only the timed event");
        assert_eq!(upserted[0].google_event_id, "real");
        assert!(
            events.deleted_by_google_event_id.lock().unwrap().is_empty(),
            "skipped events are not deleted"
        );
        assert_eq!(output.events.len(), 1);
        // Sync state still advances even when everything was skipped.
        assert_eq!(calendars.sync_states.lock().unwrap()[0].1, "st-9");
    }

    #[test]
    fn cancelled_events_are_soft_deleted() {
        let body = r#"{"items":[
            {"id": "cancelled", "status": "cancelled",
             "start": {"dateTime": "2026-08-18T09:00:00Z"}, "end": {"dateTime": "2026-08-18T09:30:00Z"}},
            {"id": "real", "summary": "Real",
             "start": {"dateTime": "2026-08-18T09:00:00Z"}, "end": {"dateTime": "2026-08-18T09:30:00Z"}}
        ], "nextSyncToken": "st-9"}"#;
        let http = FakeHttp::new(vec![("/events", 200, body)]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &access(), "u-1", "2026-08-01T00:00:00Z",
            "2026-09-01T00:00:00Z", NOW_UNIX,
        ))
        .unwrap();

        // Cancelled events are soft-deleted by google id, not skipped.
        assert_eq!(
            *events.deleted_by_google_event_id.lock().unwrap(),
            vec![("cal-1".to_string(), "cancelled".to_string())]
        );
        let upserted = events.upserted_batch.lock().unwrap();
        assert_eq!(upserted.len(), 1, "only the timed, non-cancelled event");
        assert_eq!(upserted[0].google_event_id, "real");
        assert_eq!(output.events.len(), 1);
        // Sync state still advances.
        assert_eq!(calendars.sync_states.lock().unwrap()[0].1, "st-9");
    }

    #[test]
    fn cancelled_event_delete_failure_does_not_advance_sync_token() {
        let body = r#"{"items":[
            {"id": "cancelled", "status": "cancelled",
             "start": {"dateTime": "2026-08-18T09:00:00Z"}, "end": {"dateTime": "2026-08-18T09:30:00Z"}},
            {"id": "real", "summary": "Real",
             "start": {"dateTime": "2026-08-18T09:00:00Z"}, "end": {"dateTime": "2026-08-18T09:30:00Z"}}
        ], "nextSyncToken": "st-9"}"#;
        let http = FakeHttp::new(vec![("/events", 200, body)]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();
        *events.fail_delete.lock().unwrap() = true;

        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &access(), "u-1", "2026-08-01T00:00:00Z",
            "2026-09-01T00:00:00Z", NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(
            *events.deleted_by_google_event_id.lock().unwrap(),
            vec![("cal-1".to_string(), "cancelled".to_string())]
        );
        // A delete failure fails the sync: no partial apply, no upsert, and
        // the sync token must not advance.
        assert!(events.upserted_batch.lock().unwrap().is_empty());
        assert!(
            calendars.sync_states.lock().unwrap().is_empty(),
            "sync token must not advance after a delete failure"
        );
        assert_eq!(output.sync_errors.len(), 1);
        assert!(
            output.sync_errors[0].contains("cache delete failed"),
            "{}",
            output.sync_errors[0]
        );
    }

    #[test]
    fn sync_error_does_not_fail_the_whole_listing() {
        let http = FakeHttp::new(vec![("/events", 500, "")]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &access(), "u-1", "2026-08-01T00:00:00Z",
            "2026-09-01T00:00:00Z", NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(output.sync_errors.len(), 1);
        assert!(output.sync_errors[0].contains("500"), "{}", output.sync_errors[0]);
        assert!(output.events.is_empty());
        assert!(calendars.disabled.lock().unwrap().is_empty(), "500 is not a 404");
    }

    // ──────────────────────────────────────────
    // create_event
    // ──────────────────────────────────────────

    const CREATED_JSON: &str = r#"{
        "id": "google-evt-created", "etag": "e1", "updated": "2026-08-17T12:00:00.000Z",
        "summary": "New meeting", "description": "About things",
        "start": {"dateTime": "2026-08-19T09:00:00Z"},
        "end": {"dateTime": "2026-08-19T10:00:00Z"}
    }"#;

    fn input() -> NewEventInput {
        NewEventInput {
            calendar_id: "cal-1".to_string(),
            summary: "New meeting".to_string(),
            description: Some("About things".to_string()),
            start: "2026-08-19T09:00:00Z".to_string(),
            end: "2026-08-19T10:00:00Z".to_string(),
        }
    }

    #[test]
    fn create_posts_json_to_google_and_upserts_the_cache() {
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events",
            200,
            CREATED_JSON,
        )]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let output = pollster::block_on(create_event(
            &http, &calendars, &events, &access(), &input(), NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(output.source, "google");
        assert!(output.cache_error.is_none());
        assert_eq!(output.event.id, "created-id");
        assert_eq!(output.event.google_event_id, "google-evt-created");
        assert_eq!(output.event.calendar_id, "cal-1");
        assert_eq!(output.event.title, "New meeting");
        assert_eq!(output.event.start_time, "2026-08-19T09:00:00Z");
        assert_eq!(output.event.last_synced_at, "2023-11-14T22:13:20Z");

        // POST body carries the calendar contract.
        let (url, body) = http.posts.lock().unwrap().first().unwrap().clone();
        assert!(url.contains("primary%40example.com"), "{url}");
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["summary"], "New meeting");
        assert_eq!(body["description"], "About things");
        assert_eq!(body["start"]["dateTime"], "2026-08-19T09:00:00Z");
        assert_eq!(body["end"]["dateTime"], "2026-08-19T10:00:00Z");

        // Cache upsert happened with the mapped row.
        let (google_id, upserted) = events.upserted_single.lock().unwrap().clone().unwrap();
        assert_eq!(google_id, "google-evt-created");
        assert_eq!(upserted.calendar_id, "cal-1");
    }

    #[test]
    fn create_missing_calendar_is_not_found() {
        let http = FakeHttp::new(vec![]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-other", "other", true)]);
        let events = FakeEventRepo::new();

        let err = pollster::block_on(create_event(
            &http, &calendars, &events, &access(), &input(), NOW_UNIX,
        ))
        .unwrap_err();
        assert!(matches!(err, CalendarError::NotFound), "got {err:?}");
        assert!(http.posts.lock().unwrap().is_empty(), "no Google call");
    }

    #[test]
    fn create_google_non_2xx_is_an_api_error() {
        let http = FakeHttp::new(vec![("/events", 400, r#"{"error":"invalid"}"#)]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let err = pollster::block_on(create_event(
            &http, &calendars, &events, &access(), &input(), NOW_UNIX,
        ))
        .unwrap_err();
        assert!(matches!(err, CalendarError::GoogleApi(_)), "got {err:?}");
        assert!(events.upserted_single.lock().unwrap().is_none(), "no cache write on failure");
    }

    #[test]
    fn create_cache_failure_is_logged_not_fatal() {
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events",
            200,
            CREATED_JSON,
        )]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();
        *events.fail_upsert.lock().unwrap() = true;

        let output = pollster::block_on(create_event(
            &http, &calendars, &events, &access(), &input(), NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(output.source, "google");
        assert_eq!(output.event.google_event_id, "google-evt-created");
        assert!(
            matches!(output.cache_error.as_deref(), Some(message) if message.contains("cache write failed")),
            "{:?}",
            output.cache_error
        );
    }
}
