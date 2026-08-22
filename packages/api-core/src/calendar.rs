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
//!   fallback cron ([`run_fallback_cron`]) reintroduces a time-based
//!   threshold (`CRON_SYNC_STALE_SECS`).
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
//! - Watch: every `sync_enabled` calendar is ensure-watched (`events.watch`)
//!   before its first-paint sync, but only when `WATCH_CALLBACK_URL` is set
//!   and is a public HTTPS URL ([`is_public_https_callback`]). Watch 404
//!   disables sync (and stops any prior channels); other watch errors are
//!   logged and leave sync enabled. When sync is disabled for a calendar
//!   (either by a watch 404 or an `events.list` 404), every stored channel
//!   row is stopped (`channels.stop`) and hard-deleted.
//! - Renewal: `ensure_watch` only requires *some* unexpired channel, so the
//!   fallback cron renews via [`renew_watch_if_needed`] — it mints a new
//!   channel unless one expires more than `WATCH_RENEW_HORIZON_SECS` out,
//!   then stops and hard-deletes only the old rows (overlap is allowed).
//! - Webhook: [`decide_webhook`] verifies push notifications
//!   (`X-Goog-Channel-ID`/`-Token`/`-Resource-State`) against the stored
//!   channel and calendar rows — pure and unit-tested; the Worker handler
//!   wires D1 lookups and `ctx.wait_until(sync_calendar)` (ADR 0001 §
//!   Webhook).

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;
use url::Url;

use crate::config::OAuthConfig;
use crate::models::{
    CalendarEvent, GoogleCalendar, NewCalendar, NewCalendarEvent, NewEventInput, NewWatchChannel,
    WatchChannel,
};
use crate::oauth::{HttpClient, HttpError};
use crate::repo::{CalendarEventRepo, CalendarRepo, RepoError, TokenRepo, WatchChannelRepo};
use crate::time::{add_months_unix, rfc3339_to_unix_secs, unix_secs_to_rfc3339};
use crate::token::{refresh_if_needed, GoogleAccess};

/// A calendar is stale (needs a sync) when it has not synced in this many
/// seconds (5 minutes, same as Go's `syncStaleThreshold`).
///
/// The request path no longer uses this — `list_events` is cache-only once
/// `last_synced_at` is set (ADR 0001). The fallback cron uses the 15-minute
/// [`CRON_SYNC_STALE_SECS`] instead.
pub const SYNC_STALE_THRESHOLD_SECS: i64 = 5 * 60;

/// A sync-enabled calendar is stale (needs a cron sync) when it has not
/// synced in this many seconds (15 minutes — the fallback cron's cadence,
/// ADR 0001 § Fallback cron).
pub const CRON_SYNC_STALE_SECS: i64 = 15 * 60;

/// Watch channels must still be valid at least this far in the future
/// (`now_unix + WATCH_RENEW_HORIZON_SECS`) for the cron to consider the
/// coverage healthy; channels expiring sooner are renewed (ADR 0001 §
/// Fallback cron). Google channels live 7 days by default, so a 24-hour
/// horizon renews each channel roughly weekly with plenty of slack.
pub const WATCH_RENEW_HORIZON_SECS: i64 = 24 * 60 * 60;

/// Google Calendar API endpoints.
pub const GOOGLE_CALENDAR_LIST_URL: &str =
    "https://www.googleapis.com/calendar/v3/users/me/calendarList";
pub const GOOGLE_EVENTS_BASE_URL: &str = "https://www.googleapis.com/calendar/v3/calendars";

/// `channels.stop` endpoint: POST `{id, resourceId}` to unsubscribe a watch
/// channel (ADR 0001).
pub const GOOGLE_CHANNELS_STOP_URL: &str = "https://www.googleapis.com/calendar/v3/channels/stop";

/// Default watch channel lifetime in seconds (7 days) used when Google's
/// `events.watch` response omits `expiration`.
pub const WATCH_DEFAULT_TTL_SECS: i64 = 7 * 24 * 60 * 60;

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

/// Public picker row for `GET /api/calendar/calendars`.
/// Omits sync internals (`sync_token`, `last_synced_at`, `deleted_at`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CalendarView {
    pub id: String,                 // local `google_calendars.id`
    pub google_calendar_id: String, // Google's id — this is what categories store
    pub summary: String,
    pub time_zone: String,
    pub is_primary: bool,
    pub access_role: String,
    pub sync_enabled: bool,
}

impl From<GoogleCalendar> for CalendarView {
    fn from(cal: GoogleCalendar) -> Self {
        Self {
            id: cal.id,
            google_calendar_id: cal.google_calendar_id,
            summary: cal.summary,
            time_zone: cal.time_zone,
            is_primary: cal.is_primary,
            access_role: cal.access_role,
            sync_enabled: cal.sync_enabled,
        }
    }
}

/// Response envelope for `GET /api/calendar/calendars`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CalendarsResponse {
    pub calendars: Vec<CalendarView>,
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
///
/// When `watch_callback_url` is a public HTTPS URL ([`is_public_https_callback`]),
/// each `sync_enabled` calendar is ensure-watched before its first-paint sync;
/// a watch 404 disables sync and stops any prior channels (ADR 0001).
/// `None` or a non-public URL skips all watch I/O (local dev).
pub async fn list_events(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    watches: &dyn WatchChannelRepo,
    access: &GoogleAccess,
    user_id: &str,
    start_rfc3339: &str,
    end_rfc3339: &str,
    now_unix: i64,
    watch_callback_url: Option<&str>,
) -> Result<CalendarListOutput, CalendarError> {
    let mut cals = calendars.list_by_user_id(user_id).await?;
    if cals.is_empty() {
        // First contact with Google: import the calendar list, then re-read.
        refresh_calendar_list(http, calendars, access, user_id).await?;
        cals = calendars.list_by_user_id(user_id).await?;
    }

    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);
    let mut sync_errors: Vec<String> = Vec::new();
    let watch_callback_url = watch_callback_url.filter(|url| is_public_https_callback(url));
    for cal in &cals {
        if !cal.sync_enabled {
            continue;
        }
        // Ensure-watch before the first-paint sync (ADR 0001): a sync-enabled
        // calendar with no unexpired channel gets `events.watch`. Skipped
        // entirely when WATCH_CALLBACK_URL is unset or not public HTTPS.
        if let Some(callback_url) = watch_callback_url {
            match ensure_watch(http, watches, access, cal, callback_url, now_unix).await {
                Ok(()) => {}
                Err(CalendarError::GoogleNotFound) => {
                    sync_errors.push(format!(
                        "calendar {} ({}) returned 404 for events.watch — disabling sync",
                        cal.id, cal.google_calendar_id
                    ));
                    if let Err(err) = calendars.set_sync_enabled(&cal.id, false, &now_rfc3339).await {
                        sync_errors.push(format!(
                            "failed to disable sync for calendar {}: {err}",
                            cal.id
                        ));
                    }
                    // A prior channel may exist for this calendar (e.g. the
                    // calendar became unavailable): stop + hard-delete them.
                    if let Err(err) =
                        stop_watches_for_calendar(http, watches, access, &cal.id).await
                    {
                        sync_errors.push(format!(
                            "failed to stop watch channels for calendar {}: {err}",
                            cal.id
                        ));
                    }
                    continue;
                }
                Err(err) => {
                    // Other watch errors leave sync enabled; the next request
                    // (or the fallback cron) retries.
                    sync_errors.push(format!(
                        "watch failed for calendar {} ({}): {err}",
                        cal.id, cal.google_calendar_id
                    ));
                }
            }
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
                // The calendar is gone from Google's side: stop its channels
                // so stale subscriptions do not push at a dead calendar.
                if let Err(err) =
                    stop_watches_for_calendar(http, watches, access, &cal.id).await
                {
                    sync_errors.push(format!(
                        "failed to stop watch channels for calendar {}: {err}",
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

/// Builds `extendedProperties.shared` for an `events.insert`.
///
/// Returns `None` when there is no task carrier — hand-created events
/// send no extendedProperties at all. When `Some`, `sanctuary_task_id`
/// is always set; `sanctuary_focus` is `"1"` only if focused (never
/// `"0"`); `sanctuary_priority` / `sanctuary_difficulty` are present
/// only when the input carried a non-empty snapshot. Never a partial
/// map without the carrier.
fn build_shared_properties(input: &NewEventInput) -> Option<GoogleEventSharedProperties> {
    let task_id = input.task_id.as_deref().map(str::trim).filter(|s| !s.is_empty())?;
    let trim_opt = |value: &Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Some(GoogleEventSharedProperties {
        sanctuary_task_id: Some(task_id.to_string()),
        sanctuary_focus: input.sanctuary_focus.then(|| "1".to_string()),
        sanctuary_priority: trim_opt(&input.priority),
        sanctuary_difficulty: trim_opt(&input.difficulty),
    })
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
    // The stored category color, copied verbatim by `start_task`. Never
    // included for hand-created events (`None`) or when blank after trim.
    if let Some(color_id) = input.color_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload["colorId"] = serde_json::json!(color_id);
    }
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

/// Patches an event's `end` on Google (`events.patch`) and upserts the
/// returned row into the local cache — the task timer's stop/pause path.
/// Google echoes the stored `extendedProperties` back, so the upsert
/// preserves the `sanctuary_task_id` link. Cache failures are logged
/// ([`CreateEventOutput::cache_error`]), never fatal.
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
    let Some(cal) = calendars.get_by_id(calendar_id).await? else {
        return Err(CalendarError::NotFound);
    };

    let url = format!(
        "{GOOGLE_EVENTS_BASE_URL}/{}/events/{}",
        encode_path_segment(&cal.google_calendar_id),
        encode_path_segment(google_event_id)
    );
    let payload = serde_json::json!({
        "end": { "dateTime": end_rfc3339 },
    });
    let body =
        serde_json::to_vec(&payload).map_err(|err| CalendarError::InvalidResponse(err.to_string()))?;
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

/// Lists the user's imported calendars for the picker
/// (`GET /api/calendar/calendars`).
///
/// Cache-first, like `list_events`: a non-empty store is served as-is — no
/// Google HTTP, no re-import. (Re-importing would overwrite `sync_enabled`
/// via `CALENDAR_UPSERT_SQL`'s `sync_enabled = excluded.sync_enabled`.) An
/// empty store runs the same first-contact `calendarList` import
/// `list_events` performs, then re-reads.
/// Event sync and watch channels are never touched here.
///
/// Rows are mapped to [`CalendarView`] (repo order: `is_primary DESC,
/// summary ASC`). Import failures propagate as [`CalendarError`] — nothing
/// is swallowed.
pub async fn list_calendars(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    access: &GoogleAccess,
    user_id: &str,
) -> Result<CalendarsResponse, CalendarError> {
    let mut rows = calendars.list_by_user_id(user_id).await?;
    if rows.is_empty() {
        // First contact with Google: import the calendar list, then re-read.
        refresh_calendar_list(http, calendars, access, user_id).await?;
        rows = calendars.list_by_user_id(user_id).await?;
    }
    Ok(CalendarsResponse {
        calendars: rows.into_iter().map(CalendarView::from).collect(),
    })
}

// ──────────────────────────────────────────
// Watch channels (ADR 0001)
// ──────────────────────────────────────────

/// Whether `url` is a callback Google may push webhooks to: it parses as a
/// URL, has scheme `https`, and its host is not a loopback address
/// (`localhost`, `127.0.0.1`, `::1` — host comparison is case-insensitive).
/// Empty strings, unparseable values, and missing hosts are `false`.
///
/// Google refuses to deliver push notifications to non-public addresses, and
/// watching from local `wrangler dev` would leak a channel we cannot consume —
/// so `list_events` treats a non-public callback as "skip all watch I/O".
pub fn is_public_https_callback(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    if parsed.scheme() != "https" {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    // `url::Url` renders IPv6 hosts with brackets (`[::1]`); strip them so the
    // loopback comparison sees the bare address.
    let host = host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host)
        .to_ascii_lowercase();
    host != "localhost" && host != "127.0.0.1" && host != "::1"
}

/// Mints the watch `channel_id`: 16 random bytes formatted as a UUID string
/// (`8-4-4-4-12` hex, 36 chars — well under Google's 64-char limit). The UUID
/// shape is cosmetic; it is the `X-Goog-Channel-ID` webhook lookup key.
fn mint_channel_id() -> String {
    let mut bytes = [0u8; 16];
    // Same randomness source as oauth::generate_state (OS entropy natively,
    // Web Crypto on wasm); failure is practically impossible.
    let _ = getrandom::getrandom(&mut bytes);
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-\
         {:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8],
        bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

/// Mints the webhook `token`: 32 random bytes hex-encoded (64 hex chars).
/// Compared against `X-Goog-Channel-Token` by the webhook handler. Never
/// contains OAuth tokens or other secrets.
fn mint_token() -> String {
    let mut bytes = [0u8; 32];
    let _ = getrandom::getrandom(&mut bytes);
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Google's `events.watch` success body (subset): the channel `id` we minted
/// (ignored — we store ours), Google's `resourceId`, and the channel
/// `expiration` in Unix **milliseconds**.
#[derive(Debug, Deserialize)]
struct WatchChannelResponse {
    #[serde(rename = "resourceId")]
    resource_id: String,
    #[serde(default, rename = "expiration", deserialize_with = "de_optional_expiration")]
    expiration_millis: Option<i64>,
}

/// Deserializes `Channel.expiration` (Unix ms) into `Option<i64>`. Google's
/// discovery doc types it `string`/`int64`, so `events.watch` sends it as a
/// JSON string of digits (`"1787628641000"`), while some responses send a
/// JSON number. `null`/missing → `None` (callers fall back to the default 7-day
/// TTL); an unparseable string is an error, never a silent `None`.
fn de_optional_expiration<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    struct ExpirationVisitor;

    impl<'de> Visitor<'de> for ExpirationVisitor {
        type Value = Option<i64>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("an integer, a string of digits, or null")
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
            Ok(Some(value))
        }
        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            i64::try_from(value)
                .map(Some)
                .map_err(|_| de::Error::invalid_value(de::Unexpected::Unsigned(value), &self))
        }
        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            value
                .parse::<i64>()
                .map(Some)
                .map_err(|_| de::Error::invalid_value(de::Unexpected::Str(value), &self))
        }
        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            self.visit_str(&value)
        }
    }

    deserializer.deserialize_any(ExpirationVisitor)
}

/// POSTs `events.watch` for `cal` (with a freshly minted `channel_id`/`token`)
/// and inserts the returned [`NewWatchChannel`] from Google's `resourceId` and
/// the converted expiration (Unix ms → RFC 3339 UTC; 7 days from `now_unix`
/// when Google omits it).
///
/// Shared by [`ensure_watch`] and [`renew_watch_if_needed`]. Watch HTTP 404 →
/// [`CalendarError::GoogleNotFound`] so the caller can disable sync (same as
/// an `events.list` 404). Other non-2xx → `GoogleApi`.
async fn create_watch(
    http: &dyn HttpClient,
    watches: &dyn WatchChannelRepo,
    access: &GoogleAccess,
    cal: &GoogleCalendar,
    callback_url: &str,
    now_unix: i64,
) -> Result<(), CalendarError> {
    let channel_id = mint_channel_id();
    let token = mint_token();
    let url = format!(
        "{GOOGLE_EVENTS_BASE_URL}/{}/events/watch",
        encode_path_segment(&cal.google_calendar_id)
    );
    let payload = serde_json::json!({
        "id": channel_id,
        "type": "web_hook",
        "address": callback_url,
        "token": token,
    });
    let body =
        serde_json::to_vec(&payload).map_err(|err| CalendarError::InvalidResponse(err.to_string()))?;
    let (status, response) = http.post_json(&url, &access.access_token, &body).await?;
    if status == 404 {
        return Err(CalendarError::GoogleNotFound);
    }
    if !(200..300).contains(&status) {
        return Err(CalendarError::GoogleApi(format!(
            "google events.watch returned {status}"
        )));
    }
    let channel: WatchChannelResponse = serde_json::from_slice(&response)
        .map_err(|err| CalendarError::InvalidResponse(format!("events.watch body: {err}")))?;
    let expiration_secs = channel
        .expiration_millis
        .map(|millis| millis / 1000)
        .unwrap_or(now_unix + WATCH_DEFAULT_TTL_SECS);
    watches
        .insert(
            NewWatchChannel {
                calendar_id: cal.id.clone(),
                channel_id,
                resource_id: channel.resource_id,
                token,
                expiration: unix_secs_to_rfc3339(expiration_secs),
            },
            &unix_secs_to_rfc3339(now_unix),
        )
        .await?;
    Ok(())
}

/// Ensures a Google `events.watch` channel exists for `cal`.
///
/// Returns `Ok` when an unexpired channel row is already stored for the
/// calendar; otherwise POSTs `events.watch` via [`create_watch`] and inserts
/// the channel row. Note that "unexpired" only means `expiration > now` — a
/// channel with 23 hours left still short-circuits here; renewal is the
/// fallback cron's job ([`renew_watch_if_needed`]).
pub async fn ensure_watch(
    http: &dyn HttpClient,
    watches: &dyn WatchChannelRepo,
    access: &GoogleAccess,
    cal: &GoogleCalendar,
    callback_url: &str,
    now_unix: i64,
) -> Result<(), CalendarError> {
    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);
    if !watches
        .list_unexpired_by_calendar_id(&cal.id, &now_rfc3339)
        .await?
        .is_empty()
    {
        return Ok(());
    }
    create_watch(http, watches, access, cal, callback_url, now_unix).await
}

/// POSTs `channels.stop` for one channel; HTTP 404 counts as success (the
/// channel is already gone). Any other non-2xx is an error.
async fn stop_channel(
    http: &dyn HttpClient,
    access: &GoogleAccess,
    channel: &WatchChannel,
) -> Result<(), CalendarError> {
    let payload = serde_json::json!({
        "id": channel.channel_id,
        "resourceId": channel.resource_id,
    });
    let body =
        serde_json::to_vec(&payload).map_err(|err| CalendarError::InvalidResponse(err.to_string()))?;
    let (status, _response) =
        http.post_json(GOOGLE_CHANNELS_STOP_URL, &access.access_token, &body).await?;
    if status == 404 {
        return Ok(()); // already gone — success
    }
    if !(200..300).contains(&status) {
        return Err(CalendarError::GoogleApi(format!(
            "google channels.stop returned {status}"
        )));
    }
    Ok(())
}

/// Stops every stored watch channel for `calendar_id` via `channels.stop`
/// (HTTP 404 counts as success — the channel is already gone) and then HARD
/// deletes the rows (ADR 0001).
///
/// Channels are stopped sequentially. On the first hard failure this returns
/// the error **before** deleting anything: rows that were not stopped keep
/// their `{id, resourceId}` so a later run can retry them, and earlier rows
/// that stopped successfully may already be dead on Google's side but their
/// rows are only removed once every stop succeeds.
pub async fn stop_watches_for_calendar(
    http: &dyn HttpClient,
    watches: &dyn WatchChannelRepo,
    access: &GoogleAccess,
    calendar_id: &str,
) -> Result<(), CalendarError> {
    let channels = watches.list_by_calendar_id(calendar_id).await?;
    for channel in &channels {
        stop_channel(http, access, channel).await?;
    }
    watches.delete_by_calendar_id(calendar_id).await?;
    Ok(())
}

/// Renews a calendar's watch channel when none covers `WATCH_RENEW_HORIZON_SECS`
/// from `now_unix` (ADR 0001 § Fallback cron).
///
/// `ensure_watch` only checks that some channel is unexpired (`expiration >
/// now`) — a channel with 23 hours left would skip it. Renewal instead mints
/// a new channel whenever no stored channel expires later than `now_unix +
/// WATCH_RENEW_HORIZON_SECS`, then stops and hard-deletes the **old** rows
/// individually — never `delete_by_calendar_id`, which would kill the new row.
///
/// Returns `Ok(true)` when a new channel was created, `Ok(false)` when the
/// existing coverage already spans the horizon. Watch HTTP 404 →
/// [`CalendarError::GoogleNotFound`] (the caller disables sync and stops any
/// prior channels). If stopping an old channel fails, the error is returned
/// and the new channel row remains — overlap of two rows per calendar is
/// expected (ADR 0001).
pub async fn renew_watch_if_needed(
    http: &dyn HttpClient,
    watches: &dyn WatchChannelRepo,
    access: &GoogleAccess,
    cal: &GoogleCalendar,
    callback_url: &str,
    now_unix: i64,
) -> Result<bool, CalendarError> {
    let existing = watches.list_by_calendar_id(&cal.id).await?;
    let horizon = unix_secs_to_rfc3339(now_unix + WATCH_RENEW_HORIZON_SECS);
    // RFC 3339 UTC strings of this shape compare lexicographically.
    if existing.iter().any(|channel| channel.expiration > horizon) {
        return Ok(false);
    }

    create_watch(http, watches, access, cal, callback_url, now_unix).await?;
    for old in &existing {
        stop_channel(http, access, old).await?;
        watches.delete_by_id(&old.id).await?;
    }
    Ok(true)
}

/// Outcome of one fallback cron run: counters plus human-readable failures —
/// a failure for one calendar never fails the whole job.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CronReport {
    /// Calendars synced in this run.
    pub synced: usize,
    /// Watch channels minted (renewals) in this run.
    pub renewed: usize,
    /// Human-readable failures; empty when everything worked.
    pub errors: Vec<String>,
}

/// The fallback cron (ADR 0001 § Fallback cron): for every sync-enabled,
/// non-deleted calendar, sync it when `last_synced_at` is missing/unparseable
/// or older than [`CRON_SYNC_STALE_SECS`], then renew its watch channel when
/// none covers [`WATCH_RENEW_HORIZON_SECS`].
///
/// Orchestration lives here (pure, unit-tested) so the Worker's
/// `#[event(scheduled)]` handler is a thin shell. Per-calendar failures are
/// collected in [`CronReport::errors`] and never abort the rest of the job.
///
/// Per calendar, in order:
/// 1. `refresh_if_needed` for the owner's Google token. On failure the
///    calendar is skipped entirely (no sync, no renew).
/// 2. Stale check (`last_synced_at` missing/unparseable, or
///    `now - last_sync >= CRON_SYNC_STALE_SECS`) → `sync_calendar`.
///    - `events.list` 404 disables sync, stops any prior channels, and skips
///      the renew step (a disabled calendar is not renewed).
///    - Other sync errors are logged but renewal still runs (the calendar is
///      still enabled).
/// 3. When `watch_callback_url` is a public HTTPS URL and the calendar is
///    still enabled: `renew_watch_if_needed`. A watch 404 disables sync and
///    stops any prior channels; other errors are logged.
pub async fn run_fallback_cron(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    watches: &dyn WatchChannelRepo,
    tokens: &dyn TokenRepo,
    oauth: &OAuthConfig,
    watch_callback_url: Option<&str>,
    now_unix: i64,
) -> CronReport {
    let mut report = CronReport::default();
    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);
    // Same gate as list_events: without a public HTTPS callback Google cannot
    // deliver push notifications, so all watch I/O is skipped (local dev).
    let callback = watch_callback_url.filter(|url| is_public_https_callback(url));

    let cals = match calendars.list_sync_enabled().await {
        Ok(cals) => cals,
        Err(err) => {
            report
                .errors
                .push(format!("list_sync_enabled failed: {err}"));
            return report;
        }
    };
    for cal in &cals {
        let access = match refresh_if_needed(http, tokens, oauth, &cal.user_id, now_unix).await {
            Ok(access) => access,
            Err(err) => {
                report.errors.push(format!(
                    "token refresh failed for user {} (calendar {}): {err}",
                    cal.user_id, cal.id
                ));
                continue;
            }
        };

        // Stale: never synced, unparseable timestamp, or last sync older than
        // the cron's 15-minute threshold.
        let stale = cal
            .last_synced_at
            .as_deref()
            .and_then(rfc3339_to_unix_secs)
            .is_none_or(|last_unix| now_unix - last_unix >= CRON_SYNC_STALE_SECS);
        if stale {
            match sync_calendar(http, calendars, events, &access, cal, &now_rfc3339).await {
                Ok(()) => report.synced += 1,
                Err(CalendarError::GoogleNotFound) => {
                    report.errors.push(format!(
                        "calendar {} ({}) returned 404 — disabling sync",
                        cal.id, cal.google_calendar_id
                    ));
                    if let Err(err) =
                        calendars.set_sync_enabled(&cal.id, false, &now_rfc3339).await
                    {
                        report.errors.push(format!(
                            "failed to disable sync for calendar {}: {err}",
                            cal.id
                        ));
                    }
                    // The calendar is gone from Google's side: stop its
                    // channels so stale subscriptions do not push at it.
                    if let Err(err) =
                        stop_watches_for_calendar(http, watches, &access, &cal.id).await
                    {
                        report.errors.push(format!(
                            "failed to stop watch channels for calendar {}: {err}",
                            cal.id
                        ));
                    }
                    // Do not renew a calendar whose sync was just disabled.
                    continue;
                }
                Err(err) => report.errors.push(format!(
                    "sync failed for calendar {} ({}): {err}",
                    cal.id, cal.google_calendar_id
                )),
            }
        }

        if let Some(callback_url) = callback {
            match renew_watch_if_needed(http, watches, &access, cal, callback_url, now_unix).await
            {
                Ok(true) => report.renewed += 1,
                Ok(false) => {}
                Err(CalendarError::GoogleNotFound) => {
                    report.errors.push(format!(
                        "calendar {} ({}) returned 404 for events.watch — disabling sync",
                        cal.id, cal.google_calendar_id
                    ));
                    if let Err(err) =
                        calendars.set_sync_enabled(&cal.id, false, &now_rfc3339).await
                    {
                        report.errors.push(format!(
                            "failed to disable sync for calendar {}: {err}",
                            cal.id
                        ));
                    }
                    if let Err(err) =
                        stop_watches_for_calendar(http, watches, &access, &cal.id).await
                    {
                        report.errors.push(format!(
                            "failed to stop watch channels for calendar {}: {err}",
                            cal.id
                        ));
                    }
                }
                Err(err) => report.errors.push(format!(
                    "watch renew failed for calendar {} ({}): {err}",
                    cal.id, cal.google_calendar_id
                )),
            }
        }
    }
    report
}

// ──────────────────────────────────────────
// Webhook verification (ADR 0001 § Webhook)
// ──────────────────────────────────────────

/// Outcome of verifying a Google push notification (`X-Goog-*` headers)
/// against the stored watch channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookDecision {
    /// HTTP 200, no sync: unknown channel, bad or missing token, missing or
    /// disabled calendar, the `sync` handshake, or any non-`exists` state.
    /// Verification failures never surface as 4xx/5xx (no existence leak,
    /// no Google retry hammer).
    Ignore,
    /// HTTP 200, then `ctx.wait_until(sync_calendar)` for this local
    /// calendar id.
    Sync { calendar_id: String },
}

/// Constant-time token comparison.
///
/// When both strings have the same length, every byte is XOR-accumulated, so
/// a mismatch reveals nothing about *where* the tokens differ. Different
/// lengths return `false` immediately — that is fine because our tokens are
/// fixed 64 hex chars, so length carries no secret information. The contents
/// are never compared with `==`.
pub fn tokens_match(stored: &str, presented: &str) -> bool {
    if stored.len() != presented.len() {
        return false;
    }
    let mut diff = 0u8;
    for (stored_byte, presented_byte) in stored.bytes().zip(presented.bytes()) {
        diff |= stored_byte ^ presented_byte;
    }
    diff == 0
}

/// Decides what a push notification should do, from the request headers and
/// the stored rows. Pure: no Google or D1 I/O — the caller fetches `stored`
/// (via `X-Goog-Channel-ID`) and `calendar` (via `stored.calendar_id`)
/// first.
///
/// Rules (ADR 0001 § Webhook), in order:
/// 1. `stored` is `None` → [`WebhookDecision::Ignore`] (unknown channel).
/// 2. `presented_token` is `None` or `!tokens_match(stored.token, …)` →
///    [`WebhookDecision::Ignore`].
/// 3. `calendar` is `None` (missing or soft-deleted — `get_by_id` already
///    filters `deleted_at IS NULL`) or `!calendar.sync_enabled` →
///    [`WebhookDecision::Ignore`].
/// 4. `resource_state` == `"exists"` → [`WebhookDecision::Sync`] for
///    `stored.calendar_id`.
/// 5. `"sync"` (the channel handshake) or anything else →
///    [`WebhookDecision::Ignore`].
///
/// The state comparison is case-sensitive and exact: Google sends bare
/// values like `exists`/`sync`, so a whitespace-wrapped `exists` is treated
/// as an unknown state and ignored.
pub fn decide_webhook(
    resource_state: &str,
    stored: Option<&WatchChannel>,
    presented_token: Option<&str>,
    calendar: Option<&GoogleCalendar>,
) -> WebhookDecision {
    let Some(stored) = stored else {
        return WebhookDecision::Ignore;
    };
    let Some(presented) = presented_token else {
        return WebhookDecision::Ignore;
    };
    if !tokens_match(&stored.token, presented) {
        return WebhookDecision::Ignore;
    }
    let Some(calendar) = calendar else {
        return WebhookDecision::Ignore;
    };
    if !calendar.sync_enabled {
        return WebhookDecision::Ignore;
    }
    if resource_state == "exists" {
        return WebhookDecision::Sync {
            calendar_id: stored.calendar_id.clone(),
        };
    }
    WebhookDecision::Ignore
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
///
/// `pub` for the webhook handler and the fallback cron (later slices); the
/// request path reaches it via [`list_events`].
pub async fn sync_calendar(
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
///
/// `task_id` is copied from `extendedProperties.shared.sanctuary_task_id`
/// (the task timer's carrier); events without the property map to `""` and
/// the upsert's `COALESCE` leaves any stored value untouched.
// `sanctuary_focus` / `sanctuary_priority` / `sanctuary_difficulty` are
// create-time snapshots on Google and are not cached.
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
        task_id: event
            .extended_properties
            .as_ref()
            .and_then(|props| props.shared.as_ref())
            .and_then(|shared| shared.sanctuary_task_id.clone())
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
        task_id: event.task_id,
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

/// A Google Calendar event as returned by `events.list` / `events.insert` /
/// `events.patch`.
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
    #[serde(default, rename = "extendedProperties")]
    extended_properties: Option<GoogleEventExtendedProperties>,
}

/// `extendedProperties` of a Google event. Only the shared map is modelled —
/// the task timer's `sanctuary_task_id` lives under `shared`.
#[derive(Debug, Deserialize)]
struct GoogleEventExtendedProperties {
    #[serde(default)]
    shared: Option<GoogleEventSharedProperties>,
}

/// `extendedProperties.shared` of a Google event. Every key we write is
/// modelled here: `sanctuary_task_id` (the task timer's carrier),
/// `sanctuary_focus` (focused-segment flag), and the create-time snapshots
/// `sanctuary_priority` / `sanctuary_difficulty`. Absent keys deserialize to
/// `None`; `None` values are skipped on serialize, so the wire shape never
/// carries `"0"`/empty placeholders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
struct GoogleEventSharedProperties {
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "sanctuary_task_id")]
    sanctuary_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "sanctuary_focus")]
    sanctuary_focus: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "sanctuary_priority")]
    sanctuary_priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "sanctuary_difficulty")]
    sanctuary_difficulty: Option<String>,
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
    use crate::models::{GoogleCalendar, GoogleOAuthToken, NewCalendar, NewToken, WatchChannel};

    // ──────────────────────────────────────────
    // Fakes
    // ──────────────────────────────────────────

    /// Scripted HTTP fake: `routes` are `(url-substring, status, body)` in
    /// match order; every call is recorded for assertions.
    struct FakeHttp {
        routes: Vec<(String, u16, String)>,
        gets: Mutex<Vec<String>>,
        posts: Mutex<Vec<(String, String)>>,
        patches: Mutex<Vec<(String, String)>>,
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
                patches: Mutex::new(Vec::new()),
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

        async fn patch_json(
            &self,
            url: &str,
            _token: &str,
            body: &[u8],
        ) -> Result<(u16, Vec<u8>), HttpError> {
            self.patches
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

        async fn list_sync_enabled(&self) -> Result<Vec<GoogleCalendar>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|cal| cal.sync_enabled && cal.deleted_at.is_none())
                .cloned()
                .collect())
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

        async fn get_by_calendar_and_google_id(
            &self,
            calendar_id: &str,
            google_event_id: &str,
        ) -> Result<Option<CalendarEvent>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|event| {
                    event.deleted_at.is_none()
                        && event.calendar_id == calendar_id
                        && event.google_event_id == google_event_id
                })
                .cloned())
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

        async fn list_running_by_user_id(
            &self,
            _user_id: &str,
            now_rfc3339: &str,
        ) -> Result<Vec<CalendarEvent>, RepoError> {
            // Mirrors EVENT_LIST_RUNNING_BY_USER_ID_SQL: task-tagged, living,
            // `start_time <= now < end_time` (lexicographic RFC 3339).
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|event| {
                    event.deleted_at.is_none()
                        && !event.task_id.is_empty()
                        && event.start_time.as_str() <= now_rfc3339
                        && event.end_time.as_str() > now_rfc3339
                })
                .cloned()
                .collect())
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

    /// In-memory watch-channel repo: stores rows and records every
    /// insert/delete/list call so tests can assert watch behavior.
    struct FakeWatchChannelRepo {
        stored: Mutex<Vec<WatchChannel>>,
        inserted: Mutex<Vec<NewWatchChannel>>,
        deleted_by_id: Mutex<Vec<String>>,
        deleted_by_calendar_id: Mutex<Vec<String>>,
    }

    impl FakeWatchChannelRepo {
        fn new() -> Self {
            Self {
                stored: Mutex::new(Vec::new()),
                inserted: Mutex::new(Vec::new()),
                deleted_by_id: Mutex::new(Vec::new()),
                deleted_by_calendar_id: Mutex::new(Vec::new()),
            }
        }

        fn with(channels: Vec<WatchChannel>) -> Self {
            Self {
                stored: Mutex::new(channels),
                inserted: Mutex::new(Vec::new()),
                deleted_by_id: Mutex::new(Vec::new()),
                deleted_by_calendar_id: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl WatchChannelRepo for FakeWatchChannelRepo {
        async fn insert(
            &self,
            channel: NewWatchChannel,
            now_rfc3339: &str,
        ) -> Result<String, RepoError> {
            self.inserted.lock().unwrap().push(channel.clone());
            // Ids must not collide with preloaded fixture rows (the real D1
            // impl mints UUIDv4s).
            let id = format!("wc-{}", self.stored.lock().unwrap().len() + 1);
            self.stored.lock().unwrap().push(WatchChannel {
                id: id.clone(),
                calendar_id: channel.calendar_id.clone(),
                channel_id: channel.channel_id.clone(),
                resource_id: channel.resource_id.clone(),
                token: channel.token.clone(),
                expiration: channel.expiration.clone(),
                created_at: now_rfc3339.to_string(),
                updated_at: now_rfc3339.to_string(),
            });
            Ok(id)
        }

        async fn get_by_channel_id(
            &self,
            _channel_id: &str,
        ) -> Result<Option<WatchChannel>, RepoError> {
            Ok(None)
        }

        async fn list_by_calendar_id(
            &self,
            calendar_id: &str,
        ) -> Result<Vec<WatchChannel>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|channel| channel.calendar_id == calendar_id)
                .cloned()
                .collect())
        }

        async fn list_unexpired_by_calendar_id(
            &self,
            calendar_id: &str,
            now_rfc3339: &str,
        ) -> Result<Vec<WatchChannel>, RepoError> {
            // RFC 3339 UTC strings compare lexicographically (fixed width).
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|channel| channel.calendar_id == calendar_id)
                .filter(|channel| channel.expiration.as_str() > now_rfc3339)
                .cloned()
                .collect())
        }

        async fn delete_by_id(&self, id: &str) -> Result<(), RepoError> {
            self.deleted_by_id.lock().unwrap().push(id.to_string());
            self.stored
                .lock()
                .unwrap()
                .retain(|channel| channel.id != id);
            Ok(())
        }

        async fn delete_by_calendar_id(&self, calendar_id: &str) -> Result<(), RepoError> {
            self.deleted_by_calendar_id
                .lock()
                .unwrap()
                .push(calendar_id.to_string());
            self.stored
                .lock()
                .unwrap()
                .retain(|channel| channel.calendar_id != calendar_id);
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
        calendar_for_user("u-1", id, google_cal_id, sync_enabled)
    }

    fn calendar_for_user(
        user_id: &str,
        id: &str,
        google_cal_id: &str,
        sync_enabled: bool,
    ) -> GoogleCalendar {
        GoogleCalendar {
            id: id.to_string(),
            user_id: user_id.to_string(),
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

    /// Token repo for cron tests: returns a stored token per user (expiring
    /// far in the future, so `refresh_if_needed` never POSTs) — or `None`
    /// for users without one, which fails that user's refresh without
    /// touching anyone else. Records nothing.
    struct FakeTokenRepo {
        stored: std::sync::Mutex<std::collections::HashMap<String, GoogleOAuthToken>>,
    }

    impl FakeTokenRepo {
        fn with(tokens: Vec<GoogleOAuthToken>) -> Self {
            let stored = tokens
                .into_iter()
                .map(|token| (token.user_id.clone(), token))
                .collect();
            Self {
                stored: std::sync::Mutex::new(stored),
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl TokenRepo for FakeTokenRepo {
        async fn get_by_user_id(
            &self,
            user_id: &str,
        ) -> Result<Option<GoogleOAuthToken>, RepoError> {
            Ok(self.stored.lock().unwrap().get(user_id).cloned())
        }

        async fn upsert(&self, _token: NewToken) -> Result<(), RepoError> {
            Ok(())
        }

        async fn delete(&self, _user_id: &str, _now_rfc3339: &str) -> Result<(), RepoError> {
            Ok(())
        }
    }

    /// A stored OAuth token for `user_id` whose expiry is centuries out, so
    /// `refresh_if_needed` returns it as-is (no refresh POST).
    fn fresh_token(user_id: &str, access_token: &str) -> GoogleOAuthToken {
        GoogleOAuthToken {
            id: format!("tok-{user_id}"),
            user_id: user_id.to_string(),
            access_token: access_token.to_string(),
            refresh_token: Some("rt-1".to_string()),
            expiry: "2099-01-01T00:00:00Z".to_string(),
            token_type: "Bearer".to_string(),
            scope: Some("calendar".to_string()),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            deleted_at: None,
        }
    }

    /// OAuth client credentials for `run_fallback_cron` tests; the fresh
    /// tokens above mean `refresh_if_needed` never uses them.
    fn oauth_config() -> OAuthConfig {
        OAuthConfig {
            client_id: "client-id.apps.googleusercontent.com".to_string(),
            client_secret: "client-secret".to_string(),
            redirect_url: "http://localhost:5173/auth/google/callback".to_string(),
        }
    }

    /// A stored watch channel for `calendar_id` with the given RFC 3339
    /// `expiration` (future expirations must be > NOW_UNIX's instant,
    /// 2023-11-14T22:13:20Z, to count as unexpired).
    fn watch_channel(calendar_id: &str, expiration: &str) -> WatchChannel {
        WatchChannel {
            id: "wc-1".to_string(),
            calendar_id: calendar_id.to_string(),
            channel_id: "minted-id".to_string(),
            resource_id: "resource-1".to_string(),
            token: "tok-1".to_string(),
            expiration: expiration.to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    const CALLBACK_URL: &str =
        "https://my-sanctuary.fahimalizain.com/api/calendar/notifications";

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

        let watches = FakeWatchChannelRepo::new();
        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
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

        let watches = FakeWatchChannelRepo::new();
        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
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

        let watches = FakeWatchChannelRepo::new();
        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
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

        let watches = FakeWatchChannelRepo::new();
        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
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

        let watches = FakeWatchChannelRepo::new();
        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
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

        let watches = FakeWatchChannelRepo::new();
        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
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

        let watches = FakeWatchChannelRepo::new();
        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
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

        let watches = FakeWatchChannelRepo::new();
        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
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

        let watches = FakeWatchChannelRepo::new();
        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
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

        let watches = FakeWatchChannelRepo::new();
        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
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

        let watches = FakeWatchChannelRepo::new();
        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
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

        let watches = FakeWatchChannelRepo::new();
        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
        ))
        .unwrap();

        assert_eq!(output.sync_errors.len(), 1);
        assert!(output.sync_errors[0].contains("500"), "{}", output.sync_errors[0]);
        assert!(output.events.is_empty());
        assert!(calendars.disabled.lock().unwrap().is_empty(), "500 is not a 404");
    }

    // ──────────────────────────────────────────
    // list_calendars
    // ──────────────────────────────────────────

    #[test]
    fn list_calendars_empty_store_imports_calendar_list_without_syncing_events() {
        // Only a calendarList route: any events.list or watch call would make
        // the fake panic — this test proves list_calendars does neither.
        let http = FakeHttp::new(vec![("calendarList", 200, CALENDAR_LIST_JSON)]);
        let calendars = FakeCalendarRepo::with(vec![]);

        let output =
            pollster::block_on(list_calendars(&http, &calendars, &access(), "u-1")).unwrap();

        let views = output.calendars;
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].google_calendar_id, "primary@example.com");
        assert_eq!(
            views[1].google_calendar_id,
            "en.usa#holiday@group.v.calendar.google.com"
        );
        assert_eq!(views[0].summary, "Work");
        assert_eq!(views[1].summary, "Holidays");
        assert!(views[0].is_primary, "fixture marks the primary calendar");
        assert_eq!(views[0].access_role, "owner");
        assert!(!views[1].is_primary);
        assert_eq!(views[1].access_role, "reader");

        // Exactly one Google GET — the calendarList import.
        let gets = http.gets.lock().unwrap();
        assert_eq!(gets.len(), 1, "calendarList import only");
        assert!(gets[0].contains("calendarList"), "{gets:?}");
        assert!(http.posts.lock().unwrap().is_empty(), "no watch POSTs");
        assert_eq!(calendars.upserted.lock().unwrap().len(), 2);
        assert!(
            calendars.sync_states.lock().unwrap().is_empty(),
            "no event sync"
        );
    }

    #[test]
    fn list_calendars_non_empty_store_is_cache_only() {
        // No routes: any HTTP call would make the fake panic.
        let http = FakeHttp::new(vec![]);
        let calendars =
            FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);

        let output =
            pollster::block_on(list_calendars(&http, &calendars, &access(), "u-1")).unwrap();

        assert!(
            http.gets.lock().unwrap().is_empty(),
            "no Google calls for a cached store"
        );
        let views = output.calendars;
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].id, "cal-1");
        assert_eq!(views[0].google_calendar_id, "primary@example.com");
        assert_eq!(views[0].summary, "Work");
        assert!(views[0].is_primary);
        assert!(views[0].sync_enabled);
        assert!(
            calendars.upserted.lock().unwrap().is_empty(),
            "no re-import"
        );
    }

    #[test]
    fn list_calendars_import_error_propagates() {
        let http = FakeHttp::new(vec![("calendarList", 500, "nope")]);
        let calendars = FakeCalendarRepo::with(vec![]);

        let err =
            pollster::block_on(list_calendars(&http, &calendars, &access(), "u-1")).unwrap_err();

        assert!(err.to_string().contains("calendarList fetch"), "{err}");
    }

    #[test]
    fn list_calendars_view_json_omits_sync_internals() {
        let json = serde_json::to_value(CalendarsResponse {
            calendars: vec![CalendarView::from(calendar(
                "cal-1",
                "primary@example.com",
                true,
            ))],
        })
        .unwrap();
        let object = json.as_object().unwrap();
        let view = object["calendars"][0].as_object().unwrap();
        for key in [
            "id",
            "google_calendar_id",
            "summary",
            "time_zone",
            "is_primary",
            "access_role",
            "sync_enabled",
        ] {
            assert!(view.contains_key(key), "missing picker field {key}");
        }
        for key in ["sync_token", "last_synced_at", "deleted_at"] {
            assert!(
                !view.contains_key(key),
                "sync internals must stay hidden: {key}"
            );
        }
    }

    // ──────────────────────────────────────────
    // is_public_https_callback
    // ──────────────────────────────────────────

    #[test]
    fn is_public_https_callback_accepts_only_public_https_urls() {
        assert!(is_public_https_callback(CALLBACK_URL));
        assert!(is_public_https_callback("https://sanctuary.example.com/notify"));
        assert!(is_public_https_callback("https://SANCTUARY.EXAMPLE.COM/notify"));

        assert!(!is_public_https_callback(""), "empty is false");
        assert!(!is_public_https_callback("not a url"), "unparseable is false");
        assert!(
            !is_public_https_callback("http://my-sanctuary.fahimalizain.com/api/calendar/notifications"),
            "http scheme is false"
        );
        assert!(!is_public_https_callback("https://localhost/api/calendar/notifications"));
        assert!(!is_public_https_callback("https://LOCALHOST:8443/x"), "host case-insensitive");
        assert!(!is_public_https_callback("https://127.0.0.1:8787/api/calendar/notifications"));
        assert!(!is_public_https_callback("https://[::1]/api/calendar/notifications"));
    }

    // ──────────────────────────────────────────
    // list_events watch wiring
    // ──────────────────────────────────────────

    const WATCH_JSON: &str = r#"{"id":"minted-id","resourceId":"resource-123","expiration":1710000000000}"#;
    // Production shape: `Channel.expiration` is discovery type string/int64, so
    // `events.watch` returns it as a JSON string of milliseconds. Same millis
    // as `WATCH_JSON` — the converted expiration "2024-03-09T16:00:00Z" holds.
    const WATCH_JSON_STRING_EXPIRATION: &str =
        r#"{"id":"minted-id","resourceId":"resource-123","expiration":"1710000000000"}"#;

    // ──────────────────────────────────────────
    // WatchChannelResponse deserialization
    // ──────────────────────────────────────────

    #[test]
    fn watch_channel_response_accepts_numeric_expiration() {
        let channel: WatchChannelResponse =
            serde_json::from_str(r#"{"resourceId":"r","expiration":1710000000000}"#).unwrap();
        assert_eq!(channel.expiration_millis, Some(1710000000000));
    }

    #[test]
    fn watch_channel_response_accepts_string_expiration() {
        let channel: WatchChannelResponse =
            serde_json::from_str(r#"{"resourceId":"r","expiration":"1787628641000"}"#).unwrap();
        assert_eq!(channel.expiration_millis, Some(1787628641000));
    }

    #[test]
    fn watch_channel_response_defaults_missing_expiration_to_none() {
        let channel: WatchChannelResponse =
            serde_json::from_str(r#"{"resourceId":"r"}"#).unwrap();
        assert_eq!(channel.expiration_millis, None);
    }

    #[test]
    fn watch_channel_response_maps_null_expiration_to_none() {
        let channel: WatchChannelResponse =
            serde_json::from_str(r#"{"resourceId":"r","expiration":null}"#).unwrap();
        assert_eq!(channel.expiration_millis, None);
    }

    #[test]
    fn watch_channel_response_rejects_unparseable_expiration_string() {
        let err = serde_json::from_str::<WatchChannelResponse>(
            r#"{"resourceId":"r","expiration":"not-a-number"}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not-a-number"), "{err}");
    }

    #[test]
    fn list_events_skips_watch_when_callback_is_none() {
        // FakeHttp has no `/watch` route: if list_events tried to watch, the
        // fake would panic with "no route for …/events/watch".
        let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let watches = FakeWatchChannelRepo::new();

        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
        ))
        .unwrap();

        assert!(http.posts.lock().unwrap().is_empty(), "no watch POST without a callback");
        assert!(watches.inserted.lock().unwrap().is_empty());
        // The first-paint sync still runs with no callback configured.
        assert_eq!(http.gets.lock().unwrap().len(), 1);
        assert_eq!(output.events.len(), 2);
        assert!(output.sync_errors.is_empty());
    }

    #[test]
    fn list_events_skips_watch_when_callback_is_localhost() {
        let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let watches = FakeWatchChannelRepo::new();

        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX,
            Some("http://localhost:8787/api/calendar/notifications"),
        ))
        .unwrap();

        assert!(http.posts.lock().unwrap().is_empty(), "no watch POST for a localhost callback");
        assert!(watches.inserted.lock().unwrap().is_empty());
        assert_eq!(output.events.len(), 2, "first-paint sync unaffected");
        assert!(output.sync_errors.is_empty());
    }

    #[test]
    fn never_synced_calendar_is_watched_then_synced() {
        // `/events/watch` must precede `/events`: the substring matcher would
        // otherwise swallow the watch POST URL.
        let http = FakeHttp::new(vec![
            ("/events/watch", 200, WATCH_JSON),
            ("/events", 200, EVENTS_JSON),
        ]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let watches = FakeWatchChannelRepo::new();

        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX,
            Some(CALLBACK_URL),
        ))
        .unwrap();

        // Watch POST: web_hook with the configured callback address.
        let posts = http.posts.lock().unwrap();
        assert_eq!(posts.len(), 1, "one watch POST");
        let (url, body) = posts.first().unwrap().clone();
        assert!(url.contains("/calendars/primary%40example.com/events/watch"), "{url}");
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["type"], "web_hook");
        assert_eq!(body["address"], CALLBACK_URL);
        assert_eq!(body["id"].as_str().unwrap().len(), 36, "uuid-shaped channel id");
        assert_eq!(body["token"].as_str().unwrap().len(), 64, "64 hex chars");

        // Channel row inserted with Google's resourceId + converted expiration
        // (1710000000000 ms == 1710000000 s == 2024-03-09T16:00:00Z).
        let inserted = watches.inserted.lock().unwrap();
        assert_eq!(inserted.len(), 1);
        assert_eq!(inserted[0].calendar_id, "cal-1");
        assert_eq!(inserted[0].resource_id, "resource-123");
        assert_eq!(inserted[0].expiration, "2024-03-09T16:00:00Z");

        // First-paint sync still ran and populated the cache.
        assert_eq!(events.upserted_batch.lock().unwrap().len(), 2);
        assert_eq!(output.events.len(), 2);
        assert!(output.sync_errors.is_empty());
    }

    #[test]
    fn never_synced_calendar_is_watched_then_synced_with_string_expiration() {
        // Production shape: Google sends `expiration` as a JSON string
        // (discovery type string/int64). This is the path that used to fail
        // with "invalid type: string ..., expected i64" and orphan every
        // channel; the row must be inserted exactly like the numeric case.
        let http = FakeHttp::new(vec![
            ("/events/watch", 200, WATCH_JSON_STRING_EXPIRATION),
            ("/events", 200, EVENTS_JSON),
        ]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let watches = FakeWatchChannelRepo::new();

        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX,
            Some(CALLBACK_URL),
        ))
        .unwrap();

        // Channel row inserted with Google's resourceId + converted expiration
        // ("1710000000000" ms == 1710000000 s == 2024-03-09T16:00:00Z — same as
        // the numeric fixture).
        let inserted = watches.inserted.lock().unwrap();
        assert_eq!(inserted.len(), 1);
        assert_eq!(inserted[0].calendar_id, "cal-1");
        assert_eq!(inserted[0].resource_id, "resource-123");
        assert_eq!(inserted[0].expiration, "2024-03-09T16:00:00Z");

        // First-paint sync still ran and populated the cache.
        assert_eq!(events.upserted_batch.lock().unwrap().len(), 2);
        assert_eq!(output.events.len(), 2);
        assert!(output.sync_errors.is_empty());
    }

    #[test]
    fn already_unexpired_channel_does_not_rewatch() {
        let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();
        // Future expiration: 2023-11-21T22:13:20Z > NOW_UNIX instant.
        let watches =
            FakeWatchChannelRepo::with(vec![watch_channel("cal-1", "2023-11-21T22:13:20Z")]);

        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX,
            Some(CALLBACK_URL),
        ))
        .unwrap();

        assert!(http.posts.lock().unwrap().is_empty(), "unexpired channel must not be rewatched");
        assert!(watches.inserted.lock().unwrap().is_empty());
        assert_eq!(http.gets.lock().unwrap().len(), 1, "sync still runs");
        assert!(output.sync_errors.is_empty());
    }

    #[test]
    fn watch_404_disables_sync_and_does_not_list_events() {
        let http = FakeHttp::new(vec![("/events/watch", 404, r#"{"error":"not found"}"#)]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let watches = FakeWatchChannelRepo::new();

        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX,
            Some(CALLBACK_URL),
        ))
        .unwrap();

        assert_eq!(
            *calendars.disabled.lock().unwrap(),
            vec![("cal-1".to_string(), false)]
        );
        assert!(http.gets.lock().unwrap().is_empty(), "no events.list after watch 404");
        assert!(events.upserted_batch.lock().unwrap().is_empty());
        assert_eq!(output.sync_errors.len(), 1);
        assert!(output.sync_errors[0].contains("404"), "{}", output.sync_errors[0]);
    }

    #[test]
    fn events_list_404_stops_existing_watches() {
        let http = FakeHttp::new(vec![
            ("/events", 404, r#"{"error":"not found"}"#),
            ("/channels/stop", 200, "{}"),
        ]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();
        // Channel preloaded unexpired: ensure_watch short-circuits, so the
        // only POST is channels.stop from the events.list 404 path.
        let watches =
            FakeWatchChannelRepo::with(vec![watch_channel("cal-1", "2023-11-21T22:13:20Z")]);

        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX,
            Some(CALLBACK_URL),
        ))
        .unwrap();

        // events.list 404 → sync disabled (as before)…
        assert_eq!(
            *calendars.disabled.lock().unwrap(),
            vec![("cal-1".to_string(), false)]
        );
        // …and the stored channel is stopped and hard-deleted.
        let posts = http.posts.lock().unwrap();
        assert_eq!(posts.len(), 1);
        let (url, body) = posts.first().unwrap().clone();
        assert!(url.contains("/channels/stop"), "{url}");
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["id"], "minted-id");
        assert_eq!(body["resourceId"], "resource-1");
        assert_eq!(
            *watches.deleted_by_calendar_id.lock().unwrap(),
            vec!["cal-1".to_string()]
        );
        assert!(watches.stored.lock().unwrap().is_empty(), "rows hard-deleted");
        assert_eq!(output.sync_errors.len(), 1);
        assert!(output.sync_errors[0].contains("404"), "{}", output.sync_errors[0]);
    }

    // ──────────────────────────────────────────
    // renew_watch_if_needed / run_fallback_cron
    // ──────────────────────────────────────────

    #[test]
    fn renew_skips_when_channel_expires_after_horizon() {
        // No /events/watch route: a watch POST would panic "no route".
        let http = FakeHttp::new(vec![]);
        let cal = calendar("cal-1", "primary@example.com", true);
        // 2023-11-16T00:00:00Z > horizon (now + 24h == 2023-11-15T22:13:20Z).
        let watches =
            FakeWatchChannelRepo::with(vec![watch_channel("cal-1", "2023-11-16T00:00:00Z")]);

        let renewed = pollster::block_on(renew_watch_if_needed(
            &http, &watches, &access(), &cal, CALLBACK_URL, NOW_UNIX,
        ))
        .unwrap();

        assert!(!renewed, "existing coverage spans the horizon");
        assert!(http.posts.lock().unwrap().is_empty(), "no watch POST");
        assert!(watches.inserted.lock().unwrap().is_empty());
        assert!(watches.deleted_by_id.lock().unwrap().is_empty());
    }

    #[test]
    fn renew_creates_watch_and_stops_old_when_expiring_within_horizon() {
        let http = FakeHttp::new(vec![
            ("/events/watch", 200, WATCH_JSON),
            ("/channels/stop", 200, "{}"),
        ]);
        let cal = calendar("cal-1", "primary@example.com", true);
        // 1 hour out: unexpired (ensure_watch would short-circuit) but inside
        // the 24-hour horizon — the cron must renew.
        let watches =
            FakeWatchChannelRepo::with(vec![watch_channel("cal-1", "2023-11-14T23:13:20Z")]);

        let renewed = pollster::block_on(renew_watch_if_needed(
            &http, &watches, &access(), &cal, CALLBACK_URL, NOW_UNIX,
        ))
        .unwrap();

        assert!(renewed, "new channel minted");
        // New channel inserted with the same body/minting contract as
        // ensure_watch: Google's resourceId + converted expiration.
        let inserted = watches.inserted.lock().unwrap();
        assert_eq!(inserted.len(), 1);
        assert_eq!(inserted[0].calendar_id, "cal-1");
        assert_eq!(inserted[0].resource_id, "resource-123");
        assert_eq!(inserted[0].expiration, "2024-03-09T16:00:00Z");

        // watch POST, then a channels.stop POST with the OLD channel's id.
        let posts = http.posts.lock().unwrap();
        assert_eq!(posts.len(), 2);
        assert!(posts[0].0.contains("/events/watch"), "{}", posts[0].0);
        assert!(posts[1].0.contains("/channels/stop"), "{}", posts[1].0);
        let stop_body: serde_json::Value = serde_json::from_str(&posts[1].1).unwrap();
        assert_eq!(stop_body["id"], "minted-id");
        assert_eq!(stop_body["resourceId"], "resource-1");

        // Old row hard-deleted by id only — never delete_by_calendar_id
        // (that would kill the new row). The new row remains stored.
        assert_eq!(
            *watches.deleted_by_id.lock().unwrap(),
            vec!["wc-1".to_string()]
        );
        assert!(watches.deleted_by_calendar_id.lock().unwrap().is_empty());
        let stored = watches.stored.lock().unwrap();
        assert_eq!(stored.len(), 1, "new row only");
        assert_eq!(stored[0].channel_id, inserted[0].channel_id);
    }

    #[test]
    fn cron_syncs_stale_and_skips_fresh() {
        let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
        let mut stale = calendar("cal-a", "primary@example.com", true);
        stale.last_synced_at = Some("2023-11-14T21:53:20Z".to_string()); // 20 min ago
        let mut fresh = calendar("cal-b", "secondary@example.com", true);
        fresh.last_synced_at = Some("2023-11-14T22:12:20Z".to_string()); // 1 min ago
        let calendars = FakeCalendarRepo::with(vec![stale, fresh]);
        let events = FakeEventRepo::new();
        let watches = FakeWatchChannelRepo::new();
        let tokens = FakeTokenRepo::with(vec![fresh_token("u-1", "at-1")]);
        let oauth = oauth_config();

        let report = pollster::block_on(run_fallback_cron(
            &http, &calendars, &events, &watches, &tokens, &oauth, None, NOW_UNIX,
        ));

        assert_eq!(report.synced, 1, "only the stale calendar synced");
        assert_eq!(report.renewed, 0);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let gets = http.gets.lock().unwrap();
        assert_eq!(gets.len(), 1, "fresh calendar must not hit events.list");
        assert!(gets[0].contains("primary%40example.com/events"), "{:?}", gets);
        let states = calendars.sync_states.lock().unwrap();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].0, "cal-a");
    }

    #[test]
    fn cron_syncs_never_synced() {
        let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
        // `calendar()` defaults last_synced_at: None → stale by definition.
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let watches = FakeWatchChannelRepo::new();
        let tokens = FakeTokenRepo::with(vec![fresh_token("u-1", "at-1")]);
        let oauth = oauth_config();

        let report = pollster::block_on(run_fallback_cron(
            &http, &calendars, &events, &watches, &tokens, &oauth, None, NOW_UNIX,
        ));

        assert_eq!(report.synced, 1);
        assert_eq!(report.renewed, 0);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(http.gets.lock().unwrap().len(), 1);
        assert_eq!(calendars.sync_states.lock().unwrap().len(), 1);
    }

    #[test]
    fn cron_watch_404_disables() {
        let http = FakeHttp::new(vec![("/events/watch", 404, r#"{"error":"not found"}"#)]);
        let mut cal = calendar("cal-1", "primary@example.com", true);
        cal.last_synced_at = Some("2023-11-14T22:12:20Z".to_string()); // fresh → no sync
        let calendars = FakeCalendarRepo::with(vec![cal]);
        let events = FakeEventRepo::new();
        let watches = FakeWatchChannelRepo::new();
        let tokens = FakeTokenRepo::with(vec![fresh_token("u-1", "at-1")]);
        let oauth = oauth_config();

        let report = pollster::block_on(run_fallback_cron(
            &http, &calendars, &events, &watches, &tokens, &oauth, Some(CALLBACK_URL), NOW_UNIX,
        ));

        assert_eq!(report.synced, 0);
        assert_eq!(report.renewed, 0);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("404"), "{}", report.errors[0]);
        assert_eq!(
            *calendars.disabled.lock().unwrap(),
            vec![("cal-1".to_string(), false)]
        );
        assert!(
            http.gets.lock().unwrap().is_empty(),
            "fresh calendar was not synced"
        );
    }

    #[test]
    fn cron_skips_watch_when_callback_not_public() {
        // No /events/watch route: a renew attempt would panic "no route".
        let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let watches = FakeWatchChannelRepo::new();
        let tokens = FakeTokenRepo::with(vec![fresh_token("u-1", "at-1")]);
        let oauth = oauth_config();

        let report = pollster::block_on(run_fallback_cron(
            &http, &calendars, &events, &watches, &tokens, &oauth,
            Some("http://localhost:8787/api/calendar/notifications"),
            NOW_UNIX,
        ));

        assert_eq!(report.synced, 1, "sync unaffected by the callback gate");
        assert_eq!(report.renewed, 0);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(
            http.posts.lock().unwrap().is_empty(),
            "no watch POST for a non-public callback"
        );
    }

    #[test]
    fn cron_token_refresh_failure_for_one_user_does_not_abort_the_rest() {
        let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
        // User u-a has NO stored token → refresh fails; u-b has one → proceeds.
        let mut cal_a = calendar_for_user("u-a", "cal-a", "primary@example.com", true);
        cal_a.last_synced_at = Some("2023-11-14T21:53:20Z".to_string()); // stale
        let mut cal_b = calendar_for_user("u-b", "cal-b", "secondary@example.com", true);
        cal_b.last_synced_at = Some("2023-11-14T21:53:20Z".to_string()); // stale
        let calendars = FakeCalendarRepo::with(vec![cal_a, cal_b]);
        let events = FakeEventRepo::new();
        let watches = FakeWatchChannelRepo::new();
        let tokens = FakeTokenRepo::with(vec![fresh_token("u-b", "at-b")]);
        let oauth = oauth_config();

        let report = pollster::block_on(run_fallback_cron(
            &http, &calendars, &events, &watches, &tokens, &oauth, None, NOW_UNIX,
        ));

        assert_eq!(report.synced, 1, "u-b's calendar synced despite u-a's failure");
        assert_eq!(report.renewed, 0);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("u-a"), "{}", report.errors[0]);
        let gets = http.gets.lock().unwrap();
        assert_eq!(gets.len(), 1);
        assert!(gets[0].contains("secondary%40example.com"), "{:?}", gets);
    }

    // ──────────────────────────────────────────
    // decide_webhook / tokens_match
    // ──────────────────────────────────────────

    /// A stored channel whose token is `tok-1` (the `watch_channel` fixture
    /// token), so the fixture calendar and channel pair verify cleanly.
    fn webhook_channel(calendar_id: &str) -> WatchChannel {
        watch_channel(calendar_id, "2023-11-21T22:13:20Z")
    }

    #[test]
    fn unknown_channel_is_ignored() {
        assert_eq!(
            decide_webhook(
                "exists",
                None,
                Some("tok-1"),
                Some(&calendar("cal-1", "primary@example.com", true)),
            ),
            WebhookDecision::Ignore
        );
    }

    #[test]
    fn token_mismatch_is_ignored() {
        let channel = webhook_channel("cal-1");
        assert_eq!(
            decide_webhook(
                "exists",
                Some(&channel),
                Some("tok-2"),
                Some(&calendar("cal-1", "primary@example.com", true)),
            ),
            WebhookDecision::Ignore
        );
    }

    #[test]
    fn token_length_mismatch_is_ignored() {
        let channel = webhook_channel("cal-1");
        assert_eq!(
            decide_webhook(
                "exists",
                Some(&channel),
                Some("short"),
                Some(&calendar("cal-1", "primary@example.com", true)),
            ),
            WebhookDecision::Ignore
        );
    }

    #[test]
    fn missing_token_header_is_ignored() {
        let channel = webhook_channel("cal-1");
        assert_eq!(
            decide_webhook(
                "exists",
                Some(&channel),
                None,
                Some(&calendar("cal-1", "primary@example.com", true)),
            ),
            WebhookDecision::Ignore
        );
    }

    #[test]
    fn missing_calendar_is_ignored() {
        let channel = webhook_channel("cal-1");
        assert_eq!(
            decide_webhook("exists", Some(&channel), Some("tok-1"), None),
            WebhookDecision::Ignore
        );
    }

    #[test]
    fn sync_disabled_calendar_is_ignored() {
        let channel = webhook_channel("cal-1");
        assert_eq!(
            decide_webhook(
                "exists",
                Some(&channel),
                Some("tok-1"),
                Some(&calendar("cal-1", "primary@example.com", false)),
            ),
            WebhookDecision::Ignore
        );
    }

    #[test]
    fn sync_handshake_state_is_ignored() {
        // Channel + calendar verify, but the `sync` handshake must not sync.
        let channel = webhook_channel("cal-1");
        assert_eq!(
            decide_webhook(
                "sync",
                Some(&channel),
                Some("tok-1"),
                Some(&calendar("cal-1", "primary@example.com", true)),
            ),
            WebhookDecision::Ignore
        );
    }

    #[test]
    fn unknown_state_is_ignored() {
        let channel = webhook_channel("cal-1");
        let calendar = calendar("cal-1", "primary@example.com", true);
        for state in ["", "deleted", "EXISTS", "exists2"] {
            assert_eq!(
                decide_webhook(state, Some(&channel), Some("tok-1"), Some(&calendar)),
                WebhookDecision::Ignore,
                "state {state:?} must not sync"
            );
        }
    }

    #[test]
    fn exists_with_extra_whitespace_is_ignored() {
        // Google sends bare values; a padded `exists` is not one of them.
        let channel = webhook_channel("cal-1");
        let calendar = calendar("cal-1", "primary@example.com", true);
        for state in [" exists", "exists ", " exists ", "\texists"] {
            assert_eq!(
                decide_webhook(state, Some(&channel), Some("tok-1"), Some(&calendar)),
                WebhookDecision::Ignore,
                "state {state:?} must not sync"
            );
        }
    }

    #[test]
    fn exists_state_syncs_the_channel_calendar() {
        let channel = webhook_channel("cal-1");
        assert_eq!(
            decide_webhook(
                "exists",
                Some(&channel),
                Some("tok-1"),
                Some(&calendar("cal-1", "primary@example.com", true)),
            ),
            WebhookDecision::Sync {
                calendar_id: "cal-1".to_string(),
            }
        );
    }

    #[test]
    fn tokens_match_compares_whole_strings() {
        let stored = "0123456789abcdef0123456789abcdef";
        assert!(tokens_match(stored, "0123456789abcdef0123456789abcdef"));
        assert!(
            !tokens_match(stored, "1123456789abcdef0123456789abcdef"),
            "first byte differs"
        );
        assert!(
            !tokens_match(stored, "0123456789abcdef0123456789abcde0"),
            "last byte differs"
        );
    }

    #[test]
    fn tokens_match_rejects_different_lengths() {
        assert!(!tokens_match("abcdef", "abc"));
        assert!(!tokens_match("", "x"));
        assert!(tokens_match("", ""));
    }

    #[test]
    fn tokens_match_handles_real_64_hex_tokens() {
        let stored = "a".repeat(64);
        let same = "a".repeat(64);
        let different = format!("{}b", "a".repeat(63));
        assert!(tokens_match(&stored, &same));
        assert!(!tokens_match(&stored, &different));
        assert!(!tokens_match(&stored, &"a".repeat(63)));
    }

    // ──────────────────────────────────────────
    // build_shared_properties
    // ──────────────────────────────────────────

    #[test]
    fn shared_properties_without_task_id_are_none() {
        let mut input = input();
        input.priority = Some("high".to_string());
        input.difficulty = Some("hard".to_string());
        assert!(
            build_shared_properties(&input).is_none(),
            "no carrier → no extendedProperties at all"
        );
    }

    #[test]
    fn shared_properties_with_whitespace_only_task_id_are_none() {
        for blank in ["", "   ", "\t"] {
            let mut input = input();
            input.task_id = Some(blank.to_string());
            assert!(
                build_shared_properties(&input).is_none(),
                "whitespace-only task id {blank:?} is no carrier"
            );
        }
    }

    #[test]
    fn shared_properties_task_id_only_serializes_the_carrier_alone() {
        let mut input = input();
        input.task_id = Some("task-1".to_string());
        let shared = build_shared_properties(&input).expect("carrier present");
        assert_eq!(shared.sanctuary_task_id.as_deref(), Some("task-1"));
        let value = serde_json::to_value(&shared).unwrap();
        let map = value.as_object().unwrap();
        assert_eq!(map.len(), 1, "only the carrier key: {value}");
        assert_eq!(map["sanctuary_task_id"], "task-1");
        assert!(
            !matches!(map.get("sanctuary_focus"), Some(v) if v == "0"),
            "focus never serializes as \"0\""
        );
        assert_eq!(shared.sanctuary_focus, None);
        assert_eq!(shared.sanctuary_priority, None);
        assert_eq!(shared.sanctuary_difficulty, None);
    }

    #[test]
    fn shared_properties_focused_sets_sanctuary_focus_to_one() {
        let mut input = input();
        input.task_id = Some("task-1".to_string());
        input.sanctuary_focus = true;
        let shared = build_shared_properties(&input).unwrap();
        assert_eq!(shared.sanctuary_focus.as_deref(), Some("1"));
    }

    #[test]
    fn shared_properties_unfocused_omits_the_focus_key_on_serialize() {
        let mut input = input();
        input.task_id = Some("task-1".to_string());
        input.sanctuary_focus = false;
        let shared = build_shared_properties(&input).unwrap();
        assert_eq!(shared.sanctuary_focus, None);
        let value = serde_json::to_value(&shared).unwrap();
        assert!(
            value.get("sanctuary_focus").is_none(),
            "unfocused never sends the key (never \"0\"): {value}"
        );
    }

    #[test]
    fn shared_properties_carry_priority_and_difficulty_snapshots() {
        let mut input = input();
        input.task_id = Some("task-1".to_string());
        input.priority = Some("high".to_string());
        input.difficulty = Some("hard".to_string());
        let shared = build_shared_properties(&input).unwrap();
        assert_eq!(shared.sanctuary_priority.as_deref(), Some("high"));
        assert_eq!(shared.sanctuary_difficulty.as_deref(), Some("hard"));
        let value = serde_json::to_value(&shared).unwrap();
        assert_eq!(value["sanctuary_priority"], "high");
        assert_eq!(value["sanctuary_difficulty"], "hard");
    }

    #[test]
    fn shared_properties_blank_snapshots_are_dropped() {
        let mut input = input();
        input.task_id = Some("task-1".to_string());
        input.priority = Some("  ".to_string());
        input.difficulty = Some("\t".to_string());
        let shared = build_shared_properties(&input).unwrap();
        assert_eq!(shared.sanctuary_priority, None);
        assert_eq!(shared.sanctuary_difficulty, None);
        let value = serde_json::to_value(&shared).unwrap();
        assert!(value.get("sanctuary_priority").is_none(), "{value}");
        assert!(value.get("sanctuary_difficulty").is_none(), "{value}");
    }

    #[test]
    fn shared_properties_focus_without_a_task_id_is_none() {
        let mut input = input();
        input.sanctuary_focus = true;
        input.priority = Some("high".to_string());
        assert!(
            build_shared_properties(&input).is_none(),
            "focus and snapshots never travel without the carrier"
        );
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
            task_id: None,
            color_id: None,
            sanctuary_focus: false,
            priority: None,
            difficulty: None,
        }
    }

    /// A created event that carries the task carrier, exactly as Google
    /// echoes it back after `events.insert` with the property.
    fn created_with_task_json(task_id: &str) -> String {
        format!(
            r#"{{
                "id": "google-evt-created", "etag": "e1", "updated": "2026-08-17T12:00:00.000Z",
                "summary": "New meeting", "description": "About things",
                "start": {{"dateTime": "2026-08-19T09:00:00Z"}},
                "end": {{"dateTime": "2026-08-19T10:00:00Z"}},
                "extendedProperties": {{"shared": {{"sanctuary_task_id": "{task_id}"}}}}
            }}"#
        )
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
        assert!(body.get("colorId").is_none(), "hand-created events carry no colorId");

        // Cache upsert happened with the mapped row.
        let (google_id, upserted) = events.upserted_single.lock().unwrap().clone().unwrap();
        assert_eq!(google_id, "google-evt-created");
        assert_eq!(upserted.calendar_id, "cal-1");
    }

    #[test]
    fn create_with_color_id_sends_color_id() {
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events",
            200,
            CREATED_JSON,
        )]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let mut input = input();
        input.color_id = Some("7".to_string());

        pollster::block_on(create_event(&http, &calendars, &events, &access(), &input, NOW_UNIX))
            .unwrap();

        let (_, body) = http.posts.lock().unwrap().first().unwrap().clone();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["colorId"], "7");
    }

    #[test]
    fn create_with_blank_color_id_omits_the_key() {
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();
        for blank in ["", "   ", "\t"] {
            let http = FakeHttp::new(vec![(
                "/calendars/primary%40example.com/events",
                200,
                CREATED_JSON,
            )]);
            let mut input = input();
            input.color_id = Some(blank.to_string());

            pollster::block_on(create_event(
                &http, &calendars, &events, &access(), &input, NOW_UNIX,
            ))
            .unwrap();

            let (_, body) = http.posts.lock().unwrap().first().unwrap().clone();
            let body: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert!(
                body.get("colorId").is_none(),
                "blank color_id {blank:?} must omit the key"
            );
        }
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

    #[test]
    fn create_with_task_id_sends_extended_properties_and_maps_it_back() {
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events",
            200,
            &created_with_task_json("task-1"),
        )]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let mut input = input();
        input.task_id = Some("task-1".to_string());
        let output = pollster::block_on(create_event(
            &http, &calendars, &events, &access(), &input, NOW_UNIX,
        ))
        .unwrap();

        // The insert body carries the shared carrier — never `private`, never
        // a description footer.
        let (url, body) = http.posts.lock().unwrap().first().unwrap().clone();
        assert!(url.contains("primary%40example.com"), "{url}");
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            body["extendedProperties"]["shared"]["sanctuary_task_id"],
            "task-1"
        );
        let shared = body["extendedProperties"]["shared"].as_object().unwrap();
        for key in ["sanctuary_focus", "sanctuary_priority", "sanctuary_difficulty"] {
            assert!(!shared.contains_key(key), "{key} absent without a value: {body}");
        }
        assert!(body.get("private").is_none(), "no private properties");

        // The echoed event maps the property onto the cached row.
        assert_eq!(output.event.task_id, "task-1");
        let upserted = events.upserted_single.lock().unwrap().clone().unwrap().1;
        assert_eq!(upserted.task_id, "task-1");
    }

    #[test]
    fn create_without_task_id_sends_no_extended_properties() {
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

        let (_, body) = http.posts.lock().unwrap().first().unwrap().clone();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(body.get("extendedProperties").is_none(), "{body}");
        assert_eq!(output.event.task_id, "", "no property → no task link");
    }

    /// A created event that carries both the task carrier and the focus flag,
    /// exactly as Google echoes a focused segment back after `events.insert`.
    fn created_with_focus_json(task_id: &str) -> String {
        format!(
            r#"{{
                "id": "google-evt-created", "etag": "e1", "updated": "2026-08-17T12:00:00.000Z",
                "summary": "New meeting", "description": "About things",
                "start": {{"dateTime": "2026-08-19T09:00:00Z"}},
                "end": {{"dateTime": "2026-08-19T10:00:00Z"}},
                "extendedProperties": {{"shared": {{"sanctuary_task_id": "{task_id}", "sanctuary_focus": "1"}}}}
            }}"#
        )
    }

    #[test]
    fn create_with_task_and_focus_sends_both_shared_keys() {
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events",
            200,
            &created_with_focus_json("task-1"),
        )]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let mut input = input();
        input.task_id = Some("task-1".to_string());
        input.sanctuary_focus = true;
        let output = pollster::block_on(create_event(
            &http, &calendars, &events, &access(), &input, NOW_UNIX,
        ))
        .unwrap();

        // Both keys travel together — never a partial shared map, never
        // `sanctuary_focus` inside a `private` map.
        let (_, body) = http.posts.lock().unwrap().first().unwrap().clone();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            body["extendedProperties"]["shared"]["sanctuary_task_id"],
            "task-1"
        );
        assert_eq!(body["extendedProperties"]["shared"]["sanctuary_focus"], "1");
        // No snapshots were set on the input, so P/D stay absent.
        let shared = body["extendedProperties"]["shared"].as_object().unwrap();
        for key in ["sanctuary_priority", "sanctuary_difficulty"] {
            assert!(!shared.contains_key(key), "{key} absent unless set: {body}");
        }
        assert!(body.get("private").is_none(), "no private properties");

        // The echoed event maps the task carrier onto the cached row.
        assert_eq!(output.event.task_id, "task-1");
        let upserted = events.upserted_single.lock().unwrap().clone().unwrap().1;
        assert_eq!(upserted.task_id, "task-1");
    }

    #[test]
    fn create_with_task_but_no_focus_omits_the_focus_key() {
        // Unfocused creates (e.g. `start_task`) send the carrier alone — the
        // `sanctuary_focus` key must not appear, and never as `"0"`.
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events",
            200,
            &created_with_task_json("task-1"),
        )]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let mut input = input();
        input.task_id = Some("task-1".to_string());
        input.sanctuary_focus = false;
        pollster::block_on(create_event(&http, &calendars, &events, &access(), &input, NOW_UNIX))
            .unwrap();

        let (_, body) = http.posts.lock().unwrap().first().unwrap().clone();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            body["extendedProperties"]["shared"]["sanctuary_task_id"],
            "task-1"
        );
        assert!(
            body["extendedProperties"]["shared"].get("sanctuary_focus").is_none(),
            "unfocused creates never send sanctuary_focus: {body}"
        );
    }

    #[test]
    fn create_with_task_priority_and_difficulty_snapshots_both_keys() {
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events",
            200,
            &created_with_task_json("task-1"),
        )]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let mut input = input();
        input.task_id = Some("task-1".to_string());
        input.priority = Some("high".to_string());
        input.difficulty = Some("hard".to_string());
        input.sanctuary_focus = false;
        let output = pollster::block_on(create_event(
            &http, &calendars, &events, &access(), &input, NOW_UNIX,
        ))
        .unwrap();

        // The snapshots ride along with the carrier; unfocused sends no
        // `sanctuary_focus`.
        let (_, body) = http.posts.lock().unwrap().first().unwrap().clone();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            body["extendedProperties"]["shared"]["sanctuary_task_id"],
            "task-1"
        );
        assert_eq!(
            body["extendedProperties"]["shared"]["sanctuary_priority"],
            "high"
        );
        assert_eq!(
            body["extendedProperties"]["shared"]["sanctuary_difficulty"],
            "hard"
        );
        assert!(
            body["extendedProperties"]["shared"]
                .get("sanctuary_focus")
                .is_none(),
            "snapshots never imply focus: {body}"
        );

        // The echo maps the carrier onto the cached row (snapshots are not
        // cached — no D1 columns for them).
        assert_eq!(output.event.task_id, "task-1");
    }

    #[test]
    fn sync_maps_sanctuary_task_id_onto_cached_events() {
        let body = r#"{"items":[
            {"id": "timed", "summary": "Deep Work",
             "start": {"dateTime": "2026-08-18T09:00:00Z"},
             "end": {"dateTime": "2026-08-18T10:00:00Z"},
             "extendedProperties": {"shared": {"sanctuary_task_id": "task-1"}}}
        ], "nextSyncToken": "st-9"}"#;
        let http = FakeHttp::new(vec![("/events", 200, body)]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let watches = FakeWatchChannelRepo::new();
        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
        ))
        .unwrap();

        let upserted = events.upserted_batch.lock().unwrap();
        assert_eq!(upserted.len(), 1);
        assert_eq!(upserted[0].task_id, "task-1", "carrier copied from shared props");
        assert_eq!(output.events[0].task_id, "task-1");
    }

    // ──────────────────────────────────────────
    // patch_event
    // ──────────────────────────────────────────

    const PATCHED_JSON: &str = r#"{
        "id": "google-evt-created", "etag": "e2", "updated": "2026-08-17T12:30:00.000Z",
        "summary": "New meeting",
        "start": {"dateTime": "2026-08-19T09:00:00Z"},
        "end": {"dateTime": "2026-08-19T11:00:00Z"}
    }"#;

    #[test]
    fn patch_posts_end_and_upserts_the_echoed_event() {
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events/google-evt-created",
            200,
            PATCHED_JSON,
        )]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let output = pollster::block_on(patch_event(
            &http, &calendars, &events, &access(), "cal-1", "google-evt-created",
            "2026-08-19T11:00:00Z", NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(output.event.google_event_id, "google-evt-created");
        assert_eq!(output.event.end_time, "2026-08-19T11:00:00Z");
        assert_eq!(output.event.calendar_id, "cal-1");

        // PATCH body: `end.dateTime` only.
        let patches = http.patches.lock().unwrap();
        assert_eq!(patches.len(), 1);
        let (url, body) = patches.first().unwrap().clone();
        assert!(url.contains("primary%40example.com/events/google-evt-created"), "{url}");
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["end"]["dateTime"], "2026-08-19T11:00:00Z");

        // The echoed (patched) event replaced the cached row.
        let (google_id, upserted) = events.upserted_single.lock().unwrap().clone().unwrap();
        assert_eq!(google_id, "google-evt-created");
        assert_eq!(upserted.end_time, "2026-08-19T11:00:00Z");
    }

    #[test]
    fn patch_preserves_task_link_when_google_echoes_it() {
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events/google-evt-created",
            200,
            &created_with_task_json("task-1"),
        )]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let output = pollster::block_on(patch_event(
            &http, &calendars, &events, &access(), "cal-1", "google-evt-created",
            "2026-08-19T11:00:00Z", NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(output.event.task_id, "task-1");
        let (_, upserted) = events.upserted_single.lock().unwrap().clone().unwrap();
        assert_eq!(upserted.task_id, "task-1");
    }

    #[test]
    fn patch_missing_calendar_is_not_found() {
        let http = FakeHttp::new(vec![]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-other", "other", true)]);
        let events = FakeEventRepo::new();

        let err = pollster::block_on(patch_event(
            &http, &calendars, &events, &access(), "cal-1", "g-1",
            "2026-08-19T11:00:00Z", NOW_UNIX,
        ))
        .unwrap_err();
        assert!(matches!(err, CalendarError::NotFound), "got {err:?}");
        assert!(http.patches.lock().unwrap().is_empty(), "no Google call");
    }

    #[test]
    fn patch_google_non_2xx_is_an_api_error() {
        let http = FakeHttp::new(vec![("/events/google-evt-created", 400, r#"{"error":"invalid"}"#)]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let err = pollster::block_on(patch_event(
            &http, &calendars, &events, &access(), "cal-1", "google-evt-created",
            "2026-08-19T11:00:00Z", NOW_UNIX,
        ))
        .unwrap_err();
        assert!(matches!(err, CalendarError::GoogleApi(_)), "got {err:?}");
        assert!(events.upserted_single.lock().unwrap().is_none(), "no cache write on failure");
    }

    #[test]
    fn patch_cache_failure_is_logged_not_fatal() {
        let http = FakeHttp::new(vec![(
            "/events/google-evt-created",
            200,
            PATCHED_JSON,
        )]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();
        *events.fail_upsert.lock().unwrap() = true;

        let output = pollster::block_on(patch_event(
            &http, &calendars, &events, &access(), "cal-1", "google-evt-created",
            "2026-08-19T11:00:00Z", NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(output.event.end_time, "2026-08-19T11:00:00Z");
        assert!(
            matches!(output.cache_error.as_deref(), Some(message) if message.contains("cache write failed")),
            "{:?}",
            output.cache_error
        );
    }
}
