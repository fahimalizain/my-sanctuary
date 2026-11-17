//! Read-only local stand-in for the Google Calendar endpoints the write
//! journal calls (ADR 0012).
//!
//! A standalone user has no Google project, no OAuth token, and no network
//! calendar. The write journal, the task timer, and the agenda still speak
//! [`HttpClient`], so this type implements that trait over the cached rows:
//!
//! - **State is the cache.** An event's Google payload is the `raw_json` the
//!   journal's own cache upsert stored. This type **never writes**
//!   `calendar_events` or `google_calendars`; rebuilding a response from the
//!   normalized columns would lose occurrence/task links
//!   (`extendedProperties.shared`), so a row with an empty `raw_json` is a loud
//!   500 instead.
//! - **Writes are echoes.** `events.insert` echoes the journal's payload with
//!   the Google-owned envelope stamped on (`etag`, `updated`, `iCalUID`,
//!   `status`, `sequence`, `eventType`, `organizer.self`). `events.patch`
//!   merges the minimal patch into the cached `raw_json`, bumps `sequence` and
//!   mints a fresh `etag`. `events.move` returns the source row unchanged (the
//!   journal reassigns the local `calendar_id`).
//! - **Resolution is always `(user_id, google_calendar_id)`** for calendars and
//!   `(calendar_id, google_event_id)` for events — never a bare Google id, so
//!   two standalone users cannot reach each other's rows.
//! - **Unsupported surfaces are loud.** `events.list`, `events.watch`,
//!   `channels.stop`, `calendarList`, and `post_form` return a non-404
//!   `501`/error. A 404 would make `list_events` disable the calendar
//!   (`set_sync_enabled(false)`) and make any watch attempt look like a
//!   channel that no longer exists.
//! - **`dateTime` is normalized to UTC `…Z`** on insert and patch: the
//!   event-range and running-task SQL compare RFC 3339 strings
//!   lexicographically (`EVENT_LIST_BY_USER_ID_AND_TIME_RANGE_SQL`,
//!   `EVENT_LIST_RUNNING_BY_USER_ID_SQL`), so a local offset would silently
//!   drop rows from the grid.
//! - **Etags are unique per write** (`sequence` + random bytes), never the
//!   one-second request timestamp: two patches inside the same second must
//!   still invalidate each other for the journal's `If-Match` retry path.

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use crate::models::{CalendarEvent, GoogleCalendar};
use crate::oauth::{HttpClient, HttpError};
use crate::repo::{standalone_event_labels_json, CalendarEventRepo, CalendarRepo, RepoError};
use crate::time::{rfc3339_to_unix_secs, unix_secs_to_rfc3339};

/// Google Calendar v3 API base. The journal builds event URLs from
/// [`crate::calendar::GOOGLE_EVENTS_BASE_URL`] (`…/calendar/v3/calendars`);
/// this prefix is the common root so both `…/calendars/…` and
/// `…/channels/stop` parse.
const GOOGLE_API_BASE: &str = "https://www.googleapis.com/calendar/v3";

/// A repo-backed `HttpClient` for a standalone user. Holds owned repo handles
/// so the Worker can build one per request from fresh D1 connections.
pub struct StandaloneCalendarHttp {
    calendars: Box<dyn CalendarRepo>,
    events: Box<dyn CalendarEventRepo>,
    /// Owner of the only calendar this client may touch.
    user_id: String,
    /// Request instant used for `updated` stamps.
    now_unix: i64,
}

impl StandaloneCalendarHttp {
    pub fn new(
        calendars: Box<dyn CalendarRepo>,
        events: Box<dyn CalendarEventRepo>,
        user_id: String,
        now_unix: i64,
    ) -> Self {
        Self {
            calendars,
            events,
            user_id,
            now_unix,
        }
    }

    /// Resolve a calendar by `(user_id, google_calendar_id)`. Never by the
    /// Google id alone: another standalone user's synthetic id must not match.
    async fn resolve_calendar(
        &self,
        google_calendar_id: &str,
    ) -> Result<Option<GoogleCalendar>, HttpError> {
        self.calendars
            .get_by_google_cal_id(&self.user_id, google_calendar_id)
            .await
            .map_err(repo_error)
    }

    /// Resolve a living event by `(calendar_id, google_event_id)`.
    async fn resolve_event(
        &self,
        calendar: &GoogleCalendar,
        google_event_id: &str,
    ) -> Result<Option<CalendarEvent>, HttpError> {
        self.events
            .get_by_calendar_and_google_id(&calendar.id, google_event_id)
            .await
            .map_err(repo_error)
    }
}

fn repo_error(err: RepoError) -> HttpError {
    HttpError::Message(format!("standalone calendar repo error: {err}"))
}

fn now_rfc3339(now_unix: i64) -> String {
    unix_secs_to_rfc3339(now_unix)
}

/// `"<sequence>-<8 random bytes hex>"` — quoted like Google's etags.
///
/// The random component matters: `sequence` alone is not enough when two
/// writes race (both read sequence 3), and the request timestamp is only
/// second-granular. Every write must mint a distinct value.
fn fresh_etag(sequence: i64) -> Result<String, HttpError> {
    let mut bytes = [0u8; 8];
    getrandom::getrandom(&mut bytes)
        .map_err(|_| HttpError::Message("standalone calendar entropy failure".to_string()))?;
    let mut suffix = String::with_capacity(16);
    for byte in bytes {
        suffix.push_str(&format!("{byte:02x}"));
    }
    Ok(format!("\"{sequence}-{suffix}\""))
}

// ──────────────────────────────────────────
// URL parsing
// ──────────────────────────────────────────

/// What a Google Calendar URL addresses. Every variant is fully decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Route {
    /// `GET/PATCH …/calendars/{cal}/events/{id}`
    Event { calendar: String, event: String },
    /// `POST …/calendars/{cal}/events/{id}/move?destination=…`
    EventMove {
        calendar: String,
        event: String,
        destination: String,
    },
    /// `POST …/calendars/{cal}/events` (insert) / `GET …/events` (list).
    Events { calendar: String },
    /// `GET …/calendars/{cal}` (calendars.get).
    Calendar { calendar: String },
    /// `…/calendars/{cal}/events/watch`.
    Watch,
    /// `…/channels/stop`.
    ChannelsStop,
    /// Anything else (calendarList, userinfo, garbage).
    Unsupported(String),
}

fn parse_route(url: &str) -> Route {
    let (path, query) = match url.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (url, None),
    };
    let Some(rest) = path.strip_prefix(GOOGLE_API_BASE) else {
        return Route::Unsupported(url.to_string());
    };
    let segments: Vec<String> = rest
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(percent_decode)
        .collect();

    match segments.as_slice() {
        [calendars, cal] if calendars == "calendars" => Route::Calendar {
            calendar: cal.clone(),
        },
        [calendars, cal, events] if calendars == "calendars" && events == "events" => {
            Route::Events {
                calendar: cal.clone(),
            }
        }
        [calendars, cal, events, watch]
            if calendars == "calendars" && events == "events" && watch == "watch" =>
        {
            Route::Watch
        }
        [calendars, cal, events, event] if calendars == "calendars" && events == "events" => {
            Route::Event {
                calendar: cal.clone(),
                event: event.clone(),
            }
        }
        [calendars, cal, events, event, mv]
            if calendars == "calendars" && events == "events" && mv == "move" =>
        {
            Route::EventMove {
                calendar: cal.clone(),
                event: event.clone(),
                destination: query_param(query, "destination").unwrap_or_default(),
            }
        }
        [channels, stop] if channels == "channels" && stop == "stop" => Route::ChannelsStop,
        _ => Route::Unsupported(url.to_string()),
    }
}

fn query_param(query: Option<&str>, key: &str) -> Option<String> {
    for pair in query?.split('&') {
        let (pair_key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if percent_decode(pair_key) == key {
            return Some(percent_decode(value));
        }
    }
    None
}

/// Decodes `%XX` escapes (the journal percent-encodes calendar ids, so
/// `standalone:<id>` arrives as `standalone%3A<id>`). Anything else is copied
/// verbatim.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[index + 1..index + 3]) {
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    out.push(byte);
                    index += 3;
                    continue;
                }
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ──────────────────────────────────────────
// Payload helpers
// ──────────────────────────────────────────

/// Parses a request body into a JSON object, or a loud error.
fn parse_object(body: &[u8], context: &str) -> Result<Map<String, Value>, HttpError> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|err| HttpError::Message(format!("standalone {context} body: {err}")))?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(HttpError::Message(format!(
            "standalone {context} body is not a JSON object"
        ))),
    }
}

fn to_bytes(value: &Value) -> Result<Vec<u8>, HttpError> {
    serde_json::to_vec(value)
        .map_err(|err| HttpError::Message(format!("standalone calendar response: {err}")))
}

/// Normalizes a `dateTime` string to UTC `…Z` when it parses; unparseable
/// values are kept verbatim (never dropped — losing a time is worse than an
/// unnormalized one).
fn normalize_date_time(date_time: &str) -> String {
    match rfc3339_to_unix_secs(date_time.trim()) {
        Some(unix) => unix_secs_to_rfc3339(unix),
        None => date_time.to_string(),
    }
}

/// Normalizes `start.dateTime`/`end.dateTime` in place, keeping `timeZone`.
fn normalize_times(object: &mut Map<String, Value>) {
    for key in ["start", "end"] {
        let Some(Value::Object(time)) = object.get_mut(key) else {
            continue;
        };
        let Some(date_time) = time.get("dateTime").and_then(Value::as_str) else {
            continue;
        };
        let normalized = normalize_date_time(date_time);
        time.insert("dateTime".to_string(), Value::String(normalized));
    }
}

/// Echoes an `events.insert` payload with Google's response envelope stamped
/// on. `extendedProperties.shared` (task/occurrence links) travels untouched
/// because the payload is echoed, not rebuilt.
fn stamp_insert(body: &[u8], now_unix: i64) -> Result<Value, HttpError> {
    let mut object = parse_object(body, "events.insert")?;
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            HttpError::Message("standalone events.insert body is missing id".to_string())
        })?
        .to_string();

    object.insert("etag".to_string(), Value::String(fresh_etag(0)?));
    object.insert("updated".to_string(), Value::String(now_rfc3339(now_unix)));
    object.insert(
        "iCalUID".to_string(),
        Value::String(format!("sanc-{id}@standalone.invalid")),
    );
    object.insert("status".to_string(), Value::String("confirmed".to_string()));
    object.insert("sequence".to_string(), json!(0));
    object.insert(
        "eventType".to_string(),
        Value::String("default".to_string()),
    );
    object.insert("organizer".to_string(), json!({ "self": true }));
    normalize_times(&mut object);

    Ok(Value::Object(object))
}

/// Merges one `start`/`end` patch object into the stored one.
///
/// - `date` present → replace the object whole (only supplied keys; `dateTime`
///   is dropped, converting the event to all-day).
/// - `dateTime` present → `{dateTime: normalized UTC Z, timeZone: patch.tz ??
///   stored.tz}`; `date` is dropped (converting back to timed).
/// - neither → the supplied keys replace the object (defensive only; the
///   journal always sends one of the two).
fn merge_time(
    patch: &Map<String, Value>,
    stored: Option<&Map<String, Value>>,
) -> Map<String, Value> {
    if patch.contains_key("date") {
        return patch
            .iter()
            .filter(|(key, _)| key.as_str() != "dateTime")
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
    }
    if let Some(date_time) = patch.get("dateTime").and_then(Value::as_str) {
        let mut merged = Map::new();
        merged.insert(
            "dateTime".to_string(),
            Value::String(normalize_date_time(date_time)),
        );
        let time_zone = patch.get("timeZone").and_then(Value::as_str).or_else(|| {
            stored
                .and_then(|time| time.get("timeZone"))
                .and_then(Value::as_str)
        });
        if let Some(time_zone) = time_zone {
            merged.insert("timeZone".to_string(), Value::String(time_zone.to_string()));
        }
        return merged;
    }
    patch.clone()
}

/// Preserves the stored `extendedProperties` and merges a patched `shared`
/// map over it (stored keys survive; task/occurrence links are never dropped
/// by a summary-only patch).
fn merge_extended_properties(object: &mut Map<String, Value>, patch: &Map<String, Value>) {
    let Some(patched_shared) = patch.get("shared").and_then(Value::as_object) else {
        return;
    };

    let entry = object
        .entry("extendedProperties".to_string())
        .or_insert_with(|| json!({}));
    if !entry.is_object() {
        *entry = json!({});
    }
    let extended = entry.as_object_mut().expect("just made an object");
    let shared = extended
        .entry("shared".to_string())
        .or_insert_with(|| json!({}));
    if !shared.is_object() {
        *shared = json!({});
    }
    let shared = shared.as_object_mut().expect("just made an object");
    for (key, value) in patched_shared {
        shared.insert(key.clone(), value.clone());
    }
}

/// Applies a minimal `events.patch` payload to the stored raw event and returns
/// the new body (sequence bumped, fresh etag, `updated` stamped).
fn apply_patch(
    stored: &mut Value,
    patch: &Map<String, Value>,
    now_unix: i64,
) -> Result<Value, HttpError> {
    let object = stored.as_object_mut().ok_or_else(|| {
        HttpError::Message("standalone stored event is not an object".to_string())
    })?;

    for key in ["summary", "description", "status"] {
        if let Some(value) = patch.get(key) {
            object.insert(key.to_string(), value.clone());
        }
    }

    for key in ["start", "end"] {
        if let Some(Value::Object(time)) = patch.get(key) {
            let existing = object.get(key).and_then(Value::as_object).cloned();
            object.insert(
                key.to_string(),
                Value::Object(merge_time(time, existing.as_ref())),
            );
        }
    }

    if let Some(extended) = patch.get("extendedProperties").and_then(Value::as_object) {
        merge_extended_properties(object, extended);
    }

    let sequence = object.get("sequence").and_then(Value::as_i64).unwrap_or(0) + 1;
    object.insert("sequence".to_string(), json!(sequence));
    object.insert("etag".to_string(), Value::String(fresh_etag(sequence)?));
    object.insert("updated".to_string(), Value::String(now_rfc3339(now_unix)));

    Ok(Value::Object(object.clone()))
}

/// The `etag` stored inside a raw event payload, if any.
fn stored_etag(raw: &Value) -> Option<&str> {
    raw.get("etag").and_then(Value::as_str)
}

/// `{"labelProperties":{"eventLabels":[{"id":"1","backgroundColor":"#…"}, …]}}`
/// — the same 24-color palette shape `calendars.get` returns, used only when a
/// standalone calendar's label cache is still empty.
fn calendar_labels_body() -> Result<Vec<u8>, HttpError> {
    let labels: Value = serde_json::from_str(&standalone_event_labels_json())
        .map_err(|err| HttpError::Message(format!("standalone label palette: {err}")))?;
    to_bytes(&json!({ "labelProperties": { "eventLabels": labels } }))
}

const NOT_SUPPORTED_LIST: &[u8] = b"standalone calendar does not implement events.list";
const NOT_SUPPORTED_WATCH: &[u8] = b"standalone calendar does not implement events.watch";
const NOT_SUPPORTED_STOP: &[u8] = b"standalone calendar does not implement channels.stop";
const NOT_FOUND_CALENDAR: &[u8] = b"standalone calendar not found";
const NOT_FOUND_EVENT: &[u8] = b"standalone event not found";
const EMPTY_RAW: &[u8] = b"standalone event has no stored payload";
const PRECONDITION_FAILED: &[u8] = b"precondition failed";

/// Rejects a missing/deleted row with `404`, an empty raw payload with `500`,
/// and otherwise hands back the decoded payload.
fn load_raw(event: &CalendarEvent) -> Result<Value, (u16, &'static [u8])> {
    if event.raw_json.trim().is_empty() {
        return Err((500, EMPTY_RAW));
    }
    serde_json::from_str(&event.raw_json).map_err(|_| (500, EMPTY_RAW))
}

#[async_trait(?Send)]
impl HttpClient for StandaloneCalendarHttp {
    /// Never reachable for standalone: no form-encoded Google endpoint is part
    /// of the write surface. Loud, not a silent `Ok`.
    async fn post_form(&self, _url: &str, _form: &[(&str, &str)]) -> Result<Vec<u8>, HttpError> {
        Err(HttpError::Message(
            "standalone calendar has no form endpoint".to_string(),
        ))
    }

    async fn get_bearer(&self, url: &str, access_token: &str) -> Result<Vec<u8>, HttpError> {
        let (status, body) = self.get_bearer_raw(url, access_token).await?;
        if (200..300).contains(&status) {
            Ok(body)
        } else {
            Err(HttpError::Message(format!(
                "standalone calendar GET returned {status}: {}",
                String::from_utf8_lossy(&body)
            )))
        }
    }

    async fn get_bearer_raw(
        &self,
        url: &str,
        _access_token: &str,
    ) -> Result<(u16, Vec<u8>), HttpError> {
        match parse_route(url) {
            Route::Event { calendar, event } => {
                let Some(calendar) = self.resolve_calendar(&calendar).await? else {
                    return Ok((404, NOT_FOUND_CALENDAR.to_vec()));
                };
                let Some(stored) = self.resolve_event(&calendar, &event).await? else {
                    return Ok((404, NOT_FOUND_EVENT.to_vec()));
                };
                match load_raw(&stored) {
                    Ok(raw) => Ok((200, to_bytes(&raw)?)),
                    Err((status, body)) => Ok((status, body.to_vec())),
                }
            }
            // `calendars.get`: serve the label palette. The seed already caches
            // it, so this is only reached for an older/empty cache.
            Route::Calendar { calendar } => {
                if self.resolve_calendar(&calendar).await?.is_none() {
                    return Ok((404, NOT_FOUND_CALENDAR.to_vec()));
                }
                Ok((200, calendar_labels_body()?))
            }
            Route::Events { .. } => Ok((501, NOT_SUPPORTED_LIST.to_vec())),
            Route::Watch => Ok((501, NOT_SUPPORTED_WATCH.to_vec())),
            Route::ChannelsStop => Ok((501, NOT_SUPPORTED_STOP.to_vec())),
            Route::EventMove { .. } | Route::Unsupported(_) => Err(HttpError::Message(format!(
                "standalone calendar does not implement GET {url}"
            ))),
        }
    }

    async fn post_json(
        &self,
        url: &str,
        _access_token: &str,
        body: &[u8],
    ) -> Result<(u16, Vec<u8>), HttpError> {
        match parse_route(url) {
            Route::Events { calendar } => {
                if self.resolve_calendar(&calendar).await?.is_none() {
                    return Ok((404, NOT_FOUND_CALENDAR.to_vec()));
                }
                Ok((200, to_bytes(&stamp_insert(body, self.now_unix)?)?))
            }
            Route::EventMove {
                calendar,
                event,
                destination,
            } => {
                let Some(source) = self.resolve_calendar(&calendar).await? else {
                    return Ok((404, NOT_FOUND_CALENDAR.to_vec()));
                };
                let Some(stored) = self.resolve_event(&source, &event).await? else {
                    return Ok((404, NOT_FOUND_EVENT.to_vec()));
                };
                // The destination must belong to THIS user. A foreign or
                // unknown destination is a 404, never a cross-user read.
                if destination.is_empty() || self.resolve_calendar(&destination).await?.is_none() {
                    return Ok((404, NOT_FOUND_CALENDAR.to_vec()));
                }
                match load_raw(&stored) {
                    Ok(raw) => Ok((200, to_bytes(&raw)?)),
                    Err((status, body)) => Ok((status, body.to_vec())),
                }
            }
            Route::Watch => Ok((501, NOT_SUPPORTED_WATCH.to_vec())),
            Route::ChannelsStop => Ok((501, NOT_SUPPORTED_STOP.to_vec())),
            Route::Event { .. } | Route::Calendar { .. } | Route::Unsupported(_) => Err(
                HttpError::Message(format!("standalone calendar does not implement POST {url}")),
            ),
        }
    }

    async fn patch_json(
        &self,
        url: &str,
        access_token: &str,
        body: &[u8],
    ) -> Result<(u16, Vec<u8>), HttpError> {
        self.patch_json_with_headers(url, access_token, body, &[])
            .await
    }

    async fn patch_json_with_headers(
        &self,
        url: &str,
        _access_token: &str,
        body: &[u8],
        extra_headers: &[(&str, &str)],
    ) -> Result<(u16, Vec<u8>), HttpError> {
        let Route::Event { calendar, event } = parse_route(url) else {
            return Err(HttpError::Message(format!(
                "standalone calendar does not implement PATCH {url}"
            )));
        };
        let Some(calendar) = self.resolve_calendar(&calendar).await? else {
            return Ok((404, NOT_FOUND_CALENDAR.to_vec()));
        };
        let Some(stored) = self.resolve_event(&calendar, &event).await? else {
            return Ok((404, NOT_FOUND_EVENT.to_vec()));
        };
        let mut raw = match load_raw(&stored) {
            Ok(raw) => raw,
            Err((status, body)) => return Ok((status, body.to_vec())),
        };

        // The journal sends the cached `google_etag` as `If-Match`. A mismatch
        // is a 412 so its retry path re-fetches the etag and re-sends the same
        // minimal payload.
        let if_match = extra_headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("If-Match"))
            .map(|(_, value)| *value)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if let Some(if_match) = if_match {
            if stored_etag(&raw) != Some(if_match) {
                return Ok((412, PRECONDITION_FAILED.to_vec()));
            }
        }

        let patch = parse_object(body, "events.patch")?;
        let patched = apply_patch(&mut raw, &patch, self.now_unix)?;
        Ok((200, to_bytes(&patched)?))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::calendar::{FakeCalendarRepo, FakeEventRepo};
    use crate::google_color::GOOGLE_EVENT_LABEL_COLORS;
    use crate::models::{CalendarEvent, GoogleCalendar};
    use crate::repo::standalone_event_labels_json;

    const NOW: i64 = 1_700_000_000;
    const USER: &str = "u-1";
    const GCAL: &str = "standalone:u-1";

    fn event_url(google_event_id: &str) -> String {
        format!(
            "https://www.googleapis.com/calendar/v3/calendars/{}/events/{}",
            crate::calendar::google::encode_path_segment(GCAL),
            google_event_id
        )
    }

    fn events_url() -> String {
        format!(
            "https://www.googleapis.com/calendar/v3/calendars/{}/events",
            crate::calendar::google::encode_path_segment(GCAL)
        )
    }

    fn calendar_url() -> String {
        format!(
            "https://www.googleapis.com/calendar/v3/calendars/{}",
            crate::calendar::google::encode_path_segment(GCAL)
        )
    }

    fn calendar_row() -> GoogleCalendar {
        GoogleCalendar {
            id: "standalone-cal-u-1".to_string(),
            user_id: USER.to_string(),
            google_calendar_id: GCAL.to_string(),
            summary: "Personal".to_string(),
            time_zone: "Asia/Kolkata".to_string(),
            is_primary: true,
            access_role: "owner".to_string(),
            sync_enabled: true,
            sync_token: String::new(),
            last_synced_at: Some("2023-11-14T22:13:20Z".to_string()),
            event_labels: standalone_event_labels_json(),
            event_labels_updated_at: Some("2023-11-14T22:13:20Z".to_string()),
            sync_query_fingerprint: String::new(),
            sync_status: "disabled".to_string(),
            initial_sync_complete: true,
            last_attempt_at: None,
            last_success_at: Some("2023-11-14T22:13:20Z".to_string()),
            last_error_code: String::new(),
            failure_streak: 0,
            next_retry_at: None,
            dirty_requested_generation: 0,
            dirty_applied_generation: 0,
            full_sync_requested: false,
            lease_owner: String::new(),
            lease_expires_at: None,
            cache_revision: 0,
            projection: "timed_masters_and_exceptions".to_string(),
            watch_coverage: "covered".to_string(),
            event_coverage: "complete".to_string(),
            created_at: "2023-11-14T22:13:20Z".to_string(),
            updated_at: "2023-11-14T22:13:20Z".to_string(),
            deleted_at: None,
        }
    }

    fn event_row(local_id: &str, google_event_id: &str, raw_json: &str) -> CalendarEvent {
        CalendarEvent {
            id: local_id.to_string(),
            calendar_id: "standalone-cal-u-1".to_string(),
            google_event_id: google_event_id.to_string(),
            google_etag: String::new(),
            google_updated_at: String::new(),
            last_synced_at: "2023-11-14T22:13:20Z".to_string(),
            title: "Meeting".to_string(),
            description: String::new(),
            start_time: "2026-08-19T09:00:00Z".to_string(),
            end_time: "2026-08-19T10:00:00Z".to_string(),
            recurrence: String::new(),
            task_id: String::new(),
            ical_uid: String::new(),
            sequence: 0,
            status: "confirmed".to_string(),
            recurring_event_id: String::new(),
            original_start: String::new(),
            start_time_zone: String::new(),
            end_time_zone: String::new(),
            is_all_day: false,
            raw_json: raw_json.to_string(),
            category_id: None,
            is_self_organized: true,
            event_type: "default".to_string(),
            created_at: "2023-11-14T22:13:20Z".to_string(),
            updated_at: "2023-11-14T22:13:20Z".to_string(),
            deleted_at: None,
        }
    }

    fn timed_raw(etag: &str) -> String {
        serde_json::json!({
            "id": "sanc-1",
            "etag": etag,
            "updated": "2023-11-14T22:13:20Z",
            "status": "confirmed",
            "summary": "Focus",
            "sequence": 1,
            "iCalUID": "sanc-1@standalone.invalid",
            "start": { "dateTime": "2026-08-19T09:00:00+05:30", "timeZone": "Asia/Kolkata" },
            "end": { "dateTime": "2026-08-19T10:00:00+05:30", "timeZone": "Asia/Kolkata" },
            "extendedProperties": { "shared": {
                "sanctuary_task_id": "task-1",
                "sanctuary_occurrence_id": "occ-1",
                "sanctuary_routine_id": "routine-1"
            }}
        })
        .to_string()
    }

    fn all_day_raw(etag: &str) -> String {
        serde_json::json!({
            "id": "sanc-1",
            "etag": etag,
            "updated": "2023-11-14T22:13:20Z",
            "status": "confirmed",
            "summary": "Offsite",
            "sequence": 2,
            "start": { "date": "2026-08-20" },
            "end": { "date": "2026-08-21" }
        })
        .to_string()
    }

    /// Builds a stand-in over in-memory repos. `stored_calendars` is the shared
    /// Arc so tests can assert the stand-in wrote nothing.
    fn http_with(
        calendars: Vec<GoogleCalendar>,
        events: Vec<CalendarEvent>,
    ) -> (
        StandaloneCalendarHttp,
        Arc<std::sync::Mutex<Vec<GoogleCalendar>>>,
    ) {
        let calendar_repo = FakeCalendarRepo::with(calendars);
        let stored = Arc::clone(&calendar_repo.stored);
        let event_repo = FakeEventRepo::new();
        event_repo.stored.lock().unwrap().extend(events);
        (
            StandaloneCalendarHttp::new(
                Box::new(calendar_repo),
                Box::new(event_repo),
                USER.to_string(),
                NOW,
            ),
            stored,
        )
    }

    fn insert_body() -> Vec<u8> {
        serde_json::json!({
            "id": "sanc123",
            "summary": "Deep work",
            "start": { "dateTime": "2026-08-19T09:00:00+05:30", "timeZone": "Asia/Kolkata" },
            "end": { "dateTime": "2026-08-19T10:00:00+05:30" },
            "extendedProperties": { "shared": {
                "sanctuary_event_id": "sanc123",
                "sanctuary_task_id": "task-1"
            }}
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn route_parsing_decodes_ids_and_queries() {
        assert_eq!(
            parse_route(&event_url("sanc-1")),
            Route::Event {
                calendar: GCAL.to_string(),
                event: "sanc-1".to_string()
            }
        );
        assert_eq!(
            parse_route(&events_url()),
            Route::Events {
                calendar: GCAL.to_string()
            }
        );
        assert_eq!(
            parse_route(&format!("{}?eventLabelVersion=1", events_url())),
            Route::Events {
                calendar: GCAL.to_string()
            }
        );
        assert_eq!(
            parse_route(&calendar_url()),
            Route::Calendar {
                calendar: GCAL.to_string()
            }
        );
        assert_eq!(
            parse_route(&format!("{}/watch", events_url())),
            Route::Watch
        );
        assert_eq!(
            parse_route("https://www.googleapis.com/calendar/v3/channels/stop"),
            Route::ChannelsStop
        );
        assert_eq!(
            parse_route(&format!(
                "{}/move?destination=standalone%3Au-2",
                event_url("sanc-1")
            )),
            Route::EventMove {
                calendar: GCAL.to_string(),
                event: "sanc-1".to_string(),
                destination: "standalone:u-2".to_string()
            }
        );
        assert!(matches!(
            parse_route("https://www.googleapis.com/calendar/v3/users/me/calendarList"),
            Route::Unsupported(_)
        ));
    }

    #[test]
    fn insert_stamps_the_google_envelope_and_normalizes_utc() {
        let (http, _) = http_with(vec![calendar_row()], Vec::new());

        let (status, body) = pollster::block_on(http.post_json(
            &format!("{}?eventLabelVersion=1", events_url()),
            "standalone",
            &insert_body(),
        ))
        .expect("insert");

        assert_eq!(status, 200);
        let event: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(event["id"], "sanc123");
        assert_eq!(event["summary"], "Deep work");
        assert_eq!(event["updated"], "2023-11-14T22:13:20Z");
        assert_eq!(event["iCalUID"], "sanc-sanc123@standalone.invalid");
        assert_eq!(event["status"], "confirmed");
        assert_eq!(event["sequence"], 0);
        assert_eq!(event["eventType"], "default");
        assert_eq!(event["organizer"]["self"], true);
        let etag = event["etag"].as_str().expect("etag");
        assert!(etag.starts_with("\"0-"), "quoted sequence etag: {etag}");

        // Timed values land in UTC `…Z`; the supplied zone is kept.
        assert_eq!(event["start"]["dateTime"], "2026-08-19T03:30:00Z");
        assert_eq!(event["start"]["timeZone"], "Asia/Kolkata");
        assert_eq!(event["end"]["dateTime"], "2026-08-19T04:30:00Z");
        assert!(event["end"].get("timeZone").is_none());

        // Task links survive the echo.
        assert_eq!(
            event["extendedProperties"]["shared"]["sanctuary_task_id"],
            "task-1"
        );
        assert_eq!(
            event["extendedProperties"]["shared"]["sanctuary_event_id"],
            "sanc123"
        );
    }

    #[test]
    fn insert_keeps_all_day_dates_untouched() {
        let (http, _) = http_with(vec![calendar_row()], Vec::new());
        let body = serde_json::json!({
            "id": "sanc-day",
            "summary": "Offsite",
            "start": { "date": "2026-08-20" },
            "end": { "date": "2026-08-21" }
        })
        .to_string();

        let (status, body) =
            pollster::block_on(http.post_json(&events_url(), "standalone", body.as_bytes()))
                .expect("insert");
        assert_eq!(status, 200);
        let event: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(event["start"], serde_json::json!({ "date": "2026-08-20" }));
        assert_eq!(event["end"], serde_json::json!({ "date": "2026-08-21" }));
    }

    #[test]
    fn insert_for_a_foreign_calendar_is_404_and_writes_nothing() {
        let (http, stored) = http_with(vec![calendar_row()], Vec::new());
        let url = "https://www.googleapis.com/calendar/v3/calendars/standalone%3Aother/events";

        let (status, _) =
            pollster::block_on(http.post_json(url, "standalone", &insert_body())).expect("insert");
        assert_eq!(status, 404);
        assert_eq!(stored.lock().unwrap().len(), 1, "no row written");
    }

    #[test]
    fn get_returns_the_stored_raw_payload() {
        let raw = timed_raw("\"e1\"");
        let (http, _) = http_with(
            vec![calendar_row()],
            vec![event_row("evt-1", "sanc-1", &raw)],
        );

        let body =
            pollster::block_on(http.get_bearer(&event_url("sanc-1"), "standalone")).expect("get");
        assert_eq!(String::from_utf8(body).unwrap(), raw);
    }

    #[test]
    fn get_404s_for_missing_foreign_and_soft_deleted_rows() {
        // Missing event.
        let (http, _) = http_with(vec![calendar_row()], Vec::new());
        let (status, _) =
            pollster::block_on(http.get_bearer_raw(&event_url("sanc-missing"), "standalone"))
                .expect("get");
        assert_eq!(status, 404);

        // A row for the same synthetic Google id but a different owner must not
        // resolve: calendars are always looked up by (user_id, gcal id).
        let mut foreign = calendar_row();
        foreign.user_id = "u-2".to_string();
        let (http, _) = http_with(
            vec![foreign],
            vec![event_row("evt-1", "sanc-1", &timed_raw("\"e1\""))],
        );
        let (status, _) =
            pollster::block_on(http.get_bearer_raw(&event_url("sanc-1"), "standalone"))
                .expect("get");
        assert_eq!(status, 404);

        // Soft-deleted event (the repo read filters `deleted_at IS NULL`).
        let mut deleted = event_row("evt-1", "sanc-1", &timed_raw("\"e1\""));
        deleted.deleted_at = Some("2023-11-14T22:13:20Z".to_string());
        let (http, _) = http_with(vec![calendar_row()], vec![deleted]);
        let (status, _) =
            pollster::block_on(http.get_bearer_raw(&event_url("sanc-1"), "standalone"))
                .expect("get");
        assert_eq!(status, 404);
    }

    #[test]
    fn empty_raw_json_is_a_loud_500() {
        let (http, _) = http_with(vec![calendar_row()], vec![event_row("evt-1", "sanc-1", "")]);

        let (status, body) =
            pollster::block_on(http.get_bearer_raw(&event_url("sanc-1"), "standalone"))
                .expect("get");
        assert_eq!(status, 500);
        assert_eq!(
            String::from_utf8_lossy(&body),
            "standalone event has no stored payload"
        );

        let (status, _) = pollster::block_on(http.patch_json(
            &event_url("sanc-1"),
            "standalone",
            br#"{"summary":"x"}"#,
        ))
        .expect("patch");
        assert_eq!(status, 500);
    }

    #[test]
    fn calendars_get_serves_the_24_label_palette() {
        let (http, _) = http_with(vec![calendar_row()], Vec::new());
        let body = pollster::block_on(http.get_bearer(&calendar_url(), "standalone")).expect("get");
        let value: Value = serde_json::from_slice(&body).expect("json");
        let labels = value["labelProperties"]["eventLabels"]
            .as_array()
            .expect("labels");
        assert_eq!(labels.len(), 24);
        assert_eq!(labels[0]["id"], "1");
        assert_eq!(labels[0]["backgroundColor"], GOOGLE_EVENT_LABEL_COLORS[0]);
        assert_eq!(labels[23]["id"], "24");
    }

    #[test]
    fn patch_merges_a_timed_patch_and_keeps_links() {
        let (http, _) = http_with(
            vec![calendar_row()],
            vec![event_row("evt-1", "sanc-1", &timed_raw("\"e-seq-1\""))],
        );

        let patch = serde_json::json!({
            "summary": "Renamed",
            "description": "Notes",
            "start": { "dateTime": "2026-08-19T12:00:00+05:30" },
            "end": { "dateTime": "2026-08-19T13:00:00+05:30" }
        })
        .to_string();
        let (status, body) = pollster::block_on(http.patch_json(
            &event_url("sanc-1"),
            "standalone",
            patch.as_bytes(),
        ))
        .expect("patch");
        assert_eq!(status, 200);
        let event: Value = serde_json::from_slice(&body).expect("json");

        assert_eq!(event["summary"], "Renamed");
        assert_eq!(event["description"], "Notes");
        assert_eq!(event["start"]["dateTime"], "2026-08-19T06:30:00Z");
        assert_eq!(
            event["start"]["timeZone"], "Asia/Kolkata",
            "stored zone is kept when the patch omits one"
        );
        assert_eq!(event["end"]["dateTime"], "2026-08-19T07:30:00Z");
        assert_eq!(event["sequence"], 2, "stored sequence + 1");
        assert_ne!(event["etag"], "\"e-seq-1\"", "a write must bump the etag");
        assert_eq!(event["updated"], "2023-11-14T22:13:20Z");
        assert_eq!(
            event["extendedProperties"]["shared"]["sanctuary_task_id"],
            "task-1"
        );
        assert_eq!(
            event["extendedProperties"]["shared"]["sanctuary_occurrence_id"],
            "occ-1"
        );
        assert_eq!(
            event["extendedProperties"]["shared"]["sanctuary_routine_id"],
            "routine-1"
        );
    }

    #[test]
    fn patch_converts_timed_to_all_day_and_back() {
        let (http, _) = http_with(
            vec![calendar_row()],
            vec![event_row("evt-1", "sanc-1", &timed_raw("\"e1\""))],
        );

        let to_all_day = serde_json::json!({
            "start": { "date": "2026-08-20" },
            "end": { "date": "2026-08-21" }
        })
        .to_string();
        let (status, body) = pollster::block_on(http.patch_json(
            &event_url("sanc-1"),
            "standalone",
            to_all_day.as_bytes(),
        ))
        .expect("patch");
        assert_eq!(status, 200);
        let event: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(event["start"], serde_json::json!({ "date": "2026-08-20" }));
        assert_eq!(event["end"], serde_json::json!({ "date": "2026-08-21" }));
        assert!(event["start"].get("dateTime").is_none());
        assert!(event["start"].get("timeZone").is_none());

        // Now the other direction, from a stored all-day row.
        let (http, _) = http_with(
            vec![calendar_row()],
            vec![event_row("evt-1", "sanc-1", &all_day_raw("\"e2\""))],
        );
        let to_timed = serde_json::json!({
            "start": { "dateTime": "2026-08-20T09:00:00-04:00", "timeZone": "America/New_York" },
            "end": { "dateTime": "2026-08-20T10:00:00-04:00", "timeZone": "America/New_York" }
        })
        .to_string();
        let (status, body) = pollster::block_on(http.patch_json(
            &event_url("sanc-1"),
            "standalone",
            to_timed.as_bytes(),
        ))
        .expect("patch");
        assert_eq!(status, 200);
        let event: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(event["start"]["dateTime"], "2026-08-20T13:00:00Z");
        assert_eq!(event["start"]["timeZone"], "America/New_York");
        assert!(event["start"].get("date").is_none());
        assert!(event["end"].get("date").is_none());
    }

    #[test]
    fn patch_preserves_unrelated_extended_properties_and_merges_shared() {
        let mut raw: Value = serde_json::from_str(&timed_raw("\"e1\"")).unwrap();
        raw["extendedProperties"]["private"] = serde_json::json!({ "k": "v" });
        let (http, _) = http_with(
            vec![calendar_row()],
            vec![event_row("evt-1", "sanc-1", &raw.to_string())],
        );

        let patch = serde_json::json!({
            "summary": "Renamed",
            "extendedProperties": { "shared": { "sanctuary_focus": "1" } }
        })
        .to_string();
        let (status, body) = pollster::block_on(http.patch_json(
            &event_url("sanc-1"),
            "standalone",
            patch.as_bytes(),
        ))
        .expect("patch");
        assert_eq!(status, 200);
        let event: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(
            event["extendedProperties"]["private"]["k"], "v",
            "unmodelled properties survive"
        );
        assert_eq!(
            event["extendedProperties"]["shared"]["sanctuary_task_id"],
            "task-1"
        );
        assert_eq!(
            event["extendedProperties"]["shared"]["sanctuary_focus"],
            "1"
        );
    }

    #[test]
    fn stale_if_match_is_412_and_the_fresh_etag_succeeds() {
        let stored = timed_raw("\"e-current\"");
        let (http, _) = http_with(
            vec![calendar_row()],
            vec![event_row("evt-1", "sanc-1", &stored)],
        );
        let patch = br#"{"summary":"Renamed"}"#;

        let (status, body) = pollster::block_on(http.patch_json_with_headers(
            &event_url("sanc-1"),
            "standalone",
            patch,
            &[("If-Match", "\"e-stale\"")],
        ))
        .expect("patch");
        assert_eq!(status, 412);
        assert_eq!(String::from_utf8_lossy(&body), "precondition failed");

        let (status, body) = pollster::block_on(http.patch_json_with_headers(
            &event_url("sanc-1"),
            "standalone",
            patch,
            &[("If-Match", "\"e-current\"")],
        ))
        .expect("patch");
        assert_eq!(status, 200);
        let event: Value = serde_json::from_slice(&body).expect("json");
        assert_ne!(event["etag"], "\"e-current\"");
    }

    #[test]
    fn empty_if_match_is_ignored() {
        let (http, _) = http_with(
            vec![calendar_row()],
            vec![event_row("evt-1", "sanc-1", &timed_raw("\"e1\""))],
        );
        let (status, _) = pollster::block_on(http.patch_json_with_headers(
            &event_url("sanc-1"),
            "standalone",
            br#"{"summary":"Renamed"}"#,
            &[("If-Match", "")],
        ))
        .expect("patch");
        assert_eq!(status, 200);
    }

    #[test]
    fn two_patches_in_one_second_get_different_etags() {
        let (http, _) = http_with(
            vec![calendar_row()],
            vec![event_row("evt-1", "sanc-1", &timed_raw("\"e1\""))],
        );

        let first = pollster::block_on(http.patch_json(
            &event_url("sanc-1"),
            "standalone",
            br#"{"summary":"one"}"#,
        ))
        .expect("patch");
        let second = pollster::block_on(http.patch_json(
            &event_url("sanc-1"),
            "standalone",
            br#"{"summary":"two"}"#,
        ))
        .expect("patch");

        let first: Value = serde_json::from_slice(&first.1).expect("json");
        let second: Value = serde_json::from_slice(&second.1).expect("json");
        assert_ne!(first["etag"], second["etag"]);
        assert_ne!(first["etag"], "\"e1\"");
    }

    #[test]
    fn patch_404s_for_a_missing_event() {
        let (http, _) = http_with(vec![calendar_row()], Vec::new());
        let (status, _) = pollster::block_on(http.patch_json(
            &event_url("sanc-missing"),
            "standalone",
            br#"{"summary":"x"}"#,
        ))
        .expect("patch");
        assert_eq!(status, 404);
    }

    #[test]
    fn move_returns_the_row_only_for_an_owned_destination() {
        let raw = timed_raw("\"e1\"");
        let mut destination = calendar_row();
        destination.id = "standalone-cal-u-1-b".to_string();
        destination.google_calendar_id = "standalone:u-1-b".to_string();
        destination.is_primary = false;
        destination.summary = "Second".to_string();

        let (http, stored) = http_with(
            vec![calendar_row(), destination],
            vec![event_row("evt-1", "sanc-1", &raw)],
        );

        let move_url = format!(
            "{}/move?destination=standalone%3Au-1-b",
            event_url("sanc-1")
        );
        let (status, body) =
            pollster::block_on(http.post_json(&move_url, "standalone", b"{}")).expect("move");
        assert_eq!(status, 200);
        assert_eq!(
            String::from_utf8(body).unwrap(),
            raw,
            "row comes back unchanged"
        );

        // Foreign / unknown destination.
        let move_url = format!("{}/move?destination=standalone%3Au-9", event_url("sanc-1"));
        let (status, _) =
            pollster::block_on(http.post_json(&move_url, "standalone", b"{}")).expect("move");
        assert_eq!(status, 404);

        // Missing source row.
        let move_url = format!(
            "{}/move?destination=standalone%3Au-1-b",
            event_url("sanc-missing")
        );
        let (status, _) =
            pollster::block_on(http.post_json(&move_url, "standalone", b"{}")).expect("move");
        assert_eq!(status, 404);

        assert_eq!(stored.lock().unwrap().len(), 2, "move never writes");
    }

    #[test]
    fn watch_and_list_are_501_not_404() {
        let (http, _) = http_with(vec![calendar_row()], Vec::new());

        let (status, _) =
            pollster::block_on(http.get_bearer_raw(&events_url(), "standalone")).expect("list");
        assert_eq!(status, 501);

        let (status, _) = pollster::block_on(http.post_json(
            &format!("{}/watch", events_url()),
            "standalone",
            b"{}",
        ))
        .expect("watch");
        assert_eq!(status, 501);

        let (status, _) = pollster::block_on(http.post_json(
            "https://www.googleapis.com/calendar/v3/channels/stop",
            "standalone",
            b"{}",
        ))
        .expect("stop");
        assert_eq!(status, 501);
    }

    #[test]
    fn post_form_is_a_loud_error() {
        let (http, _) = http_with(vec![calendar_row()], Vec::new());
        let err = pollster::block_on(http.post_form("https://example.com", &[])).unwrap_err();
        assert!(err.to_string().contains("no form endpoint"), "{err}");
    }

    #[test]
    fn unsupported_urls_are_loud_errors() {
        let (http, _) = http_with(vec![calendar_row()], Vec::new());

        for url in [
            "https://www.googleapis.com/calendar/v3/users/me/calendarList",
            "https://accounts.google.com/o/oauth2/token",
        ] {
            assert!(
                pollster::block_on(http.get_bearer_raw(url, "standalone")).is_err(),
                "GET {url}"
            );
            assert!(
                pollster::block_on(http.post_json(url, "standalone", b"{}")).is_err(),
                "POST {url}"
            );
        }
    }
}
