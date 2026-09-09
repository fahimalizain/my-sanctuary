//! Calendar service: cached event listing (with Google sync) and event
//! creation, mirroring the old Go `handlers/calendar.go`.
//!
//! Pure Rust and unit-testable: Google HTTP calls go through [`HttpClient`],
//! persistence through [`CalendarRepo`]/[`CalendarEventRepo`], and "now" comes
//! from the caller — never `SystemTime`. The Worker layers session checks and
//! token refresh on top (`apps/worker/src/calendar.rs`).
//!
//! Sync rules (ADR 0001 + ADR 0005 two-path + health):
//! - `list_events` is **cache-only** once `initial_sync_complete` is true **or**
//!   `last_synced_at` is parseable. Never-initialized calendars take Path A:
//!   a bounded **window** fetch (`singleEvents=true` + `timeMin`/`timeMax`),
//!   write-through display rows, throw away any `nextSyncToken`, bump dirty,
//!   and do **not** call [`sync_calendar`] / [`crate::calendar_replica`].
//!   The fallback cron ([`run_fallback_cron`]) and webhooks still own Path B
//!   (replica) and reintroduce a time-based threshold (`CRON_SYNC_STALE_SECS`).
//! - Parseable `last_synced_at` remains the request-path gate (ADR 0001). It
//!   is **not** the health signal: health is the sanitized `sync` envelope
//!   built from persisted replica columns (`last_success_at`, `sync_status`,
//!   …) via [`crate::calendar_sync`] (ADR 0005).
//! - [`sync_calendar`] records `record_sync_attempt` before any Google fetch,
//!   acquires a lease, applies the replica walk page-by-page, and publishes
//!   via fenced `record_sync_success_if_owner` only when apply finished **and**
//!   a terminal `nextSyncToken` is present, then marks `dirty_applied_generation`
//!   to the generation snapshotted at start. `record_sync_failure` on every
//!   other path (including missing terminal token). Attempt ≠ success. A busy
//!   lease is [`SyncCalendarOutcome::LeaseBusy`] (not a failure, not a publish).
//! - Replica `events.list` uses `singleEvents=false&maxResults=250`, optionally
//!   with the stored `syncToken` (incremental), and follows `nextPageToken`
//!   (see [`crate::calendar_replica`]). **Only the replica walk owns
//!   `sync_token`.**
//! - Window `events.list` uses `singleEvents=true&orderBy=startTime` plus the
//!   request time bounds (see [`crate::calendar_window`]). Never sends
//!   `syncToken`; never calls `record_sync_success`.
//! - HTTP 410 on the **replica** is merge-full once in-invocation: drop the
//!   cursor and re-list without truncating applied rows. Window 410 is a plain
//!   error (no merge-full — no token was sent). HTTP 404 (e.g. holidays/
//!   birthdays calendars that don't support `events.list`) disables sync for
//!   that calendar. Other errors are logged (returned in `sync_errors`) and
//!   do not fail the whole listing.
//! - Apply is classified in [`crate::calendar_apply`]: ordinary cancelled
//!   events (`status == "cancelled"`, no `recurringEventId`) are soft-deleted;
//!   cancelled exceptions are upserted as sparse living rows; all-day and
//!   no-time events are upserted (stored out of the GET projection
//!   `timed_masters_and_exceptions`). Window write-through reuses the same
//!   classifier.
//! - Watch: every `sync_enabled` calendar is ensure-watched (`events.watch`)
//!   before its first-paint window, but only when `WATCH_CALLBACK_URL` is set
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
//!   channel and calendar rows — pure and unit-tested. The Worker persists
//!   the decision via [`persist_webhook_decision`] (dirty bump or disable)
//!   **before** HTTP 200; an optional `ctx.wait_until` replica attempt is
//!   only an optimization after durable dirty is written.

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;
use thiserror::Error;
use url::Url;

use crate::calendar_apply::{
    map_google_event, row_from_new_event, GoogleEvent, GoogleEventSharedProperties,
};
use crate::calendar_replica::{lease_expires_at, mint_lease_owner, sync_replica};
use crate::calendar_sync::{
    classify_sync_error, events_sync_envelope, next_retry_rfc3339, replica_state_for_error,
    EventsSyncEnvelope, SyncErrorCode,
};
use crate::calendar_window::fetch_and_apply_window;
use crate::config::OAuthConfig;
use crate::google_color::{canonicalize_hex, snap_to_event_label_hex};
use crate::models::{
    CalendarEvent, GoogleCalendar, NewCalendar, NewEventInput, NewWatchChannel, PatchEventFields,
    WatchChannel,
};
use crate::oauth::{HttpClient, HttpError};
use crate::repo::{CalendarEventRepo, CalendarRepo, RepoError, TokenRepo, WatchChannelRepo};
use crate::time::{add_months_unix, rfc3339_to_unix_secs, unix_secs_to_rfc3339};
use crate::token::{is_refresh_auth_revoked, refresh_if_needed, GoogleAccess, TokenError};

/// A calendar is stale (needs a sync) when it has not synced in this many
/// seconds (5 minutes, same as Go's `syncStaleThreshold`).
///
/// The request path no longer uses this — `list_events` is cache-only once
/// `last_synced_at` is set (ADR 0001). The fallback cron uses the 15-minute
/// [`CRON_SYNC_STALE_SECS`] instead.
pub const SYNC_STALE_THRESHOLD_SECS: i64 = 5 * 60;

/// A sync-enabled calendar is stale (needs a cron sync) when it has not
/// had a successful replica publish in this many seconds (15 minutes — the
/// fallback cron's cadence, ADR 0001 § Fallback cron). Freshness is
/// [`GoogleCalendar::last_success_at`], not `last_synced_at` (ADR 0005).
pub const CRON_SYNC_STALE_SECS: i64 = 15 * 60;

/// Cap on how many calendars may start a replica walk (`sync_calendar`) in
/// one fallback-cron tick. Remaining due calendars stay dirty for the next
/// tick. Counts Published, LeaseBusy, and Err attempts alike.
pub const CRON_MAX_REPLICA_CALENDARS: usize = 25;

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
    #[error("{0}")]
    Invalid(String),
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

/// Result of [`list_events`]: the cached (and/or window) events, per-calendar
/// sync errors for the caller to log (failures never fail the whole listing),
/// the sanitized replica-health envelope, and the response `source`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarListOutput {
    pub events: Vec<CalendarEvent>,
    /// Human-readable sync failures; empty when every calendar synced fine.
    /// Worker console logs only — never placed on the HTTP envelope.
    pub sync_errors: Vec<String>,
    /// Sanitized per-calendar replica health (no tokens / credentials).
    pub sync: EventsSyncEnvelope,
    /// `"cache"` | `"window"` | `"mixed"` — how the event set was produced.
    /// See [`list_events`] for the rules.
    pub source: String,
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
///
/// Events are painted with the matched category color via
/// [`crate::calendar_color::paint_events_for_user`] before serialization.
/// `sync` is the sanitized replica-health envelope (never contains tokens).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CalendarEventsResponse {
    pub events: Vec<crate::calendar_color::CalendarEventView>,
    pub source: String,
    pub sync: EventsSyncEnvelope,
}

/// Response envelope for `POST /api/calendar/events` and
/// `PATCH /api/calendar/events/:id`.
///
/// The event is painted with the matched category color via
/// [`crate::calendar_color::paint_events_for_user`] before serialization.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CreateEventResponse {
    pub event: crate::calendar_color::CalendarEventView,
    pub source: String,
}

/// Response envelope for `DELETE /api/calendar/events/:id`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DeleteEventResponse {
    pub success: bool,
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

/// Lists the user's cached events. Never-initialized calendars take a
/// bounded window fetch first (see module docs); initialized calendars are
/// cache-only. Does **not** call [`sync_calendar`] / the replica walk.
///
/// When `watch_callback_url` is a public HTTPS URL ([`is_public_https_callback`]),
/// each `sync_enabled` calendar is ensure-watched before its first-paint window;
/// a watch 404 disables sync and stops any prior channels (ADR 0001).
/// `None` or a non-public URL skips all watch I/O (local dev).
///
/// `source` rules:
/// - `"cache"` — no window fetch ran
/// - `"window"` — at least one window fetch ran and no initialized calendar
///   contributed cache-only rows
/// - `"mixed"` — at least one window fetch and at least one initialized
///   cache-only calendar
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
    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);
    let mut cals = calendars.list_by_user_id(user_id).await?;
    if cals.is_empty() {
        // First contact with Google: import the calendar list, then re-read.
        refresh_calendar_list(http, calendars, Some(watches), access, user_id, &now_rfc3339)
            .await?;
        cals = calendars.list_by_user_id(user_id).await?;
    }

    let mut sync_errors: Vec<String> = Vec::new();
    let mut window_fetched = false;
    let mut cache_only_initialized = false;
    let mut ephemeral: Vec<CalendarEvent> = Vec::new();
    let watch_callback_url = watch_callback_url.filter(|url| is_public_https_callback(url));
    for cal in &cals {
        if !cal.sync_enabled {
            continue;
        }
        // Ensure-watch before the first-paint window (ADR 0001): a sync-enabled
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
        // Cache-only after first publication (ADR 0001 / 0005): either gate
        // means the replica cursor exists — never pull on the request path.
        // Missing both → never-initialized: Path A window fetch (not Path B).
        let previously_synced = cal.initial_sync_complete
            || cal
                .last_synced_at
                .as_deref()
                .and_then(rfc3339_to_unix_secs)
                .is_some();
        if previously_synced {
            cache_only_initialized = true;
            continue;
        }

        // Hint cron/webhook Path B; do not await the replica on first paint.
        if let Err(err) = calendars.bump_dirty_requested(&cal.id, &now_rfc3339).await {
            sync_errors.push(format!(
                "failed to bump dirty for calendar {} ({}): {err}",
                cal.id, cal.google_calendar_id
            ));
        }

        window_fetched = true;
        match fetch_and_apply_window(
            http,
            calendars,
            events,
            access,
            cal,
            start_rfc3339,
            end_rfc3339,
            &now_rfc3339,
        )
        .await
        {
            Ok(rows) => {
                ephemeral.extend(rows);
            }
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
                "window fetch failed for calendar {} ({}): {err}",
                cal.id, cal.google_calendar_id
            )),
        }
    }

    let mut cached = events
        .list_by_user_id_and_time_range(user_id, start_rfc3339, end_rfc3339)
        .await?;

    // Lease-miss path: merge ephemeral window rows the D1 write-through skipped.
    merge_ephemeral_window_events(&mut cached, ephemeral, start_rfc3339, end_rfc3339);

    // Re-read calendars so the health envelope reflects dirty bumps / disables
    // from this request. On re-read failure, fall back to the in-memory
    // snapshot from the start of the request. Window path must not flip
    // ready / initial_sync_complete / sync_token.
    let fresh = match calendars.list_by_user_id(user_id).await {
        Ok(rows) => rows,
        Err(_) => cals,
    };
    let sync = events_sync_envelope(&fresh, now_unix);

    let source = match (window_fetched, cache_only_initialized) {
        (false, _) => "cache",
        (true, false) => "window",
        (true, true) => "mixed",
    }
    .to_string();

    Ok(CalendarListOutput {
        events: cached,
        sync_errors,
        sync,
        source,
    })
}

/// Merge lease-miss ephemeral window rows into the D1 cache query result.
///
/// Applies the same GET projection filters as the list SQL
/// (`timed_masters_and_exceptions` + overlap). Prefers existing D1 rows on
/// natural-key collision.
fn merge_ephemeral_window_events(
    cached: &mut Vec<CalendarEvent>,
    ephemeral: Vec<CalendarEvent>,
    start_rfc3339: &str,
    end_rfc3339: &str,
) {
    for event in ephemeral {
        if event.is_all_day {
            continue;
        }
        if event.status == "cancelled" {
            continue;
        }
        if event.deleted_at.is_some() {
            continue;
        }
        if !(event.start_time.as_str() < end_rfc3339 && event.end_time.as_str() > start_rfc3339)
        {
            continue;
        }
        let already = cached.iter().any(|row| {
            row.calendar_id == event.calendar_id && row.google_event_id == event.google_event_id
        });
        if !already {
            cached.push(event);
        }
    }
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
fn build_shared_properties(input: &NewEventInput) -> Option<GoogleEventSharedProperties> {
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
/// Horizon is 24h (`WATCH_RENEW_HORIZON_SECS`); cron already renews before
/// coverage goes thin — no separate health-column alert for under-coverage.
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
    /// Calendars that successfully published a replica in this run.
    pub synced: usize,
    /// Watch channels minted (renewals) in this run.
    pub renewed: usize,
    /// Human-readable failures; empty when everything worked.
    /// Never contains tokens or event bodies.
    pub errors: Vec<String>,
    /// `(user_id, calendar_id)` that actually published this run.
    /// Never includes failures or [`SyncCalendarOutcome::LeaseBusy`]. Worker
    /// notifies from this list **after** D1 is updated.
    pub published: Vec<(String, String)>,
}

/// Outcome of a successful [`sync_calendar`] call (errors use [`CalendarError`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncCalendarOutcome {
    /// Fenced replica publish succeeded (terminal nextSyncToken committed).
    Published,
    /// Foreign unexpired lease; quiet skip, not a failure, not a publish.
    LeaseBusy,
}

/// Whether a sync-enabled calendar should start a replica walk this tick.
///
/// Due when **all** of:
/// - `access_role != "freeBusyReader"` (replica `singleEvents=false` strips
///   details on freeBusyReader calendars — keep events, never walk)
/// - `sync_status != "authorization_required"` (do not hot-loop Google; keep events)
/// - `next_retry_at` is missing/unparseable **or** `<= now_unix` (honor V1 backoff)
///
/// AND **any** of:
/// - `dirty_requested_generation > dirty_applied_generation`
/// - `last_success_at` missing/unparseable
/// - `last_success_at` older than [`CRON_SYNC_STALE_SECS`] (15m backstop; watches
///   do not disable the poll)
/// - `full_sync_requested`
/// - `next_retry_at` is due (`Some` and `<= now`) — a failed run with a still-fresh
///   `last_success_at` still retries when backoff expires
///
/// Freshness uses `last_success_at`, not `last_synced_at` (ADR 0005).
pub fn replica_due(cal: &GoogleCalendar, now_unix: i64) -> bool {
    if cal.access_role == "freeBusyReader" {
        return false;
    }
    if cal.sync_status == "authorization_required" {
        return false;
    }

    let retry_unix = cal
        .next_retry_at
        .as_deref()
        .and_then(rfc3339_to_unix_secs);
    // Missing/unparseable next_retry_at → no backoff gate.
    // Future next_retry_at → not due (backoff wins even if otherwise stale).
    if let Some(retry) = retry_unix {
        if retry > now_unix {
            return false;
        }
    }

    let dirty = cal.dirty_requested_generation > cal.dirty_applied_generation;
    let success_unix = cal
        .last_success_at
        .as_deref()
        .and_then(rfc3339_to_unix_secs);
    // Missing/unparseable/future last_success → treat as stale (due).
    let success_stale = match success_unix {
        None => true,
        Some(last) => now_unix - last >= CRON_SYNC_STALE_SECS || last > now_unix,
    };
    let retry_due = retry_unix.is_some_and(|retry| retry <= now_unix);

    dirty || success_stale || cal.full_sync_requested || retry_due
}

/// The fallback cron (ADR 0001 § Fallback cron + ADR 0005 V3): per user,
/// incrementally refresh `calendarList`, then for every sync-enabled
/// non-deleted calendar publish a replica when [`replica_due`] (dirty
/// generation, 15-minute `last_success_at` backstop, full-sync flag, or
/// expired backoff), then renew its watch channel when none covers
/// [`WATCH_RENEW_HORIZON_SECS`]. After the per-user loop, a **leftover-stop
/// pass** retries `channels.stop` for watch rows whose calendar is disabled
/// or soft-deleted (best-effort stop failures leave rows for the next tick).
/// Successful publishes and **newly imported** calendar ids land in
/// [`CronReport::published`] so the Worker can notify open browsers after D1
/// is updated.
///
/// Orchestration lives here (pure, unit-tested) so the Worker's
/// `#[event(scheduled)]` handler is a thin shell. Per-calendar / per-user
/// failures are collected in [`CronReport::errors`] and never abort the rest
/// of the job. OAuth refresh is cached per `user_id` so two calendars of the
/// same user share one access token in a tick.
///
/// Per user, in order:
/// 1. `refresh_if_needed` for the owner's Google token (cached per user).
///    - Revoked refresh (`invalid_grant` / token-endpoint 400/401): stamp
///      `auth_revoked` / `authorization_required` on the user's living
///      sync-enabled calendars, keep events, skip Google for that user.
///    - `NoToken` / `NoRefreshToken`: skip + error string only (do not flip
///      healthy calendars to `authorization_required`).
/// 2. [`refresh_calendar_list`] (incremental when a list cursor exists).
///    List errors are logged; existing sync-enabled calendars still get
///    replica/renew work. New local calendar ids are pushed to `published`.
/// 3. For each living sync-enabled calendar of the user:
///    - When [`replica_due`] and under [`CRON_MAX_REPLICA_CALENDARS`]:
///      `sync_calendar`.
///      - [`SyncCalendarOutcome::Published`] → `synced` + `published`.
///      - [`SyncCalendarOutcome::LeaseBusy`] → quiet skip (not synced).
///      - `events.list` 404 disables sync, stops channels, skips renew.
///      - Other sync errors are logged; dirty stays requested > applied; renew
///        still runs when the calendar is still enabled.
///    - When `watch_callback_url` is a public HTTPS URL, the calendar is still
///      enabled, and `sync_status != "authorization_required"`:
///      `renew_watch_if_needed`. A watch 404 disables sync and stops channels.
/// 4. Leftover-stop tail: [`WatchChannelRepo::list_all`], resolve each calendar
///    via [`CalendarRepo::get_by_id_unfiltered`], stop channels when the
///    calendar is missing-from-living-path (soft-deleted) or `!sync_enabled`.
///    Does **not** require a public HTTPS callback (stop needs no webhook URL).
///    Does **not** abort prior replica work. Failed stops leave rows + error.
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

    // Prefer every owner of a living calendar (including sync-disabled) so a
    // user who disabled everything still gets list refresh and can pick up
    // newly added Google calendars. Fall back to unique owners of the
    // sync-enabled work list when that query is empty.
    let user_ids = match calendars.list_user_ids_with_calendars().await {
        Ok(ids) if !ids.is_empty() => ids,
        Ok(_) => match calendars.list_sync_enabled().await {
            Ok(cals) => {
                let mut ids: Vec<String> = cals.into_iter().map(|c| c.user_id).collect();
                ids.sort();
                ids.dedup();
                ids
            }
            Err(err) => {
                report
                    .errors
                    .push(format!("list_sync_enabled failed: {err}"));
                return report;
            }
        },
        Err(err) => {
            report
                .errors
                .push(format!("list_user_ids_with_calendars failed: {err}"));
            match calendars.list_sync_enabled().await {
                Ok(cals) => {
                    let mut ids: Vec<String> = cals.into_iter().map(|c| c.user_id).collect();
                    ids.sort();
                    ids.dedup();
                    ids
                }
                Err(err2) => {
                    report
                        .errors
                        .push(format!("list_sync_enabled failed: {err2}"));
                    return report;
                }
            }
        }
    };

    // One refresh per user per tick — two concurrent refreshes of the same
    // grant can invalidate each other.
    let mut access_by_user: HashMap<String, Result<GoogleAccess, TokenError>> = HashMap::new();
    let mut replica_attempts: usize = 0;

    for user_id in &user_ids {
        let access_result = match access_by_user.get(user_id) {
            Some(cached) => cached.clone(),
            None => {
                let result = refresh_if_needed(http, tokens, oauth, user_id, now_unix).await;
                access_by_user.insert(user_id.clone(), result.clone());
                result
            }
        };

        let access = match access_result {
            Ok(access) => access,
            Err(err) => {
                report.errors.push(format!(
                    "token refresh failed for user {user_id}: {err}"
                ));
                if is_refresh_auth_revoked(&err) {
                    // Keep events; stop Google for this user's calendars this tick.
                    match calendars.list_by_user_id(user_id).await {
                        Ok(user_cals) => {
                            for cal in user_cals.iter().filter(|c| c.sync_enabled) {
                                if let Err(persist_err) = persist_sync_failure(
                                    calendars,
                                    cal,
                                    SyncErrorCode::AuthRevoked,
                                    now_unix,
                                    &now_rfc3339,
                                )
                                .await
                                {
                                    report.errors.push(format!(
                                        "failed to stamp auth_revoked for calendar {}: {persist_err}",
                                        cal.id
                                    ));
                                }
                            }
                        }
                        Err(list_err) => report.errors.push(format!(
                            "failed to list calendars for auth_revoked stamp (user {user_id}): {list_err}"
                        )),
                    }
                }
                continue;
            }
        };

        // Incremental (or full) calendarList refresh before replica work so
        // newly added calendars can be published this tick and removed ones
        // stop being watched.
        match refresh_calendar_list(
            http,
            calendars,
            Some(watches),
            &access,
            user_id,
            &now_rfc3339,
        )
        .await
        {
            Ok(new_ids) => {
                for id in new_ids {
                    report.published.push((user_id.clone(), id));
                }
            }
            Err(err) => {
                // Prefer: log list error, still process existing sync-enabled calendars.
                report.errors.push(format!(
                    "calendarList refresh failed for user {user_id}: {err}"
                ));
            }
        }

        let user_cals = match calendars.list_by_user_id(user_id).await {
            Ok(cals) => cals
                .into_iter()
                .filter(|c| c.sync_enabled)
                .collect::<Vec<_>>(),
            Err(err) => {
                report.errors.push(format!(
                    "list_by_user_id failed for user {user_id}: {err}"
                ));
                continue;
            }
        };

        for cal in &user_cals {
            let due = replica_due(cal, now_unix);
            if due && replica_attempts < CRON_MAX_REPLICA_CALENDARS {
                replica_attempts += 1;
                match sync_calendar(http, calendars, events, &access, cal, &now_rfc3339).await {
                    Ok(SyncCalendarOutcome::Published) => {
                        report.synced += 1;
                        report
                            .published
                            .push((cal.user_id.clone(), cal.id.clone()));
                    }
                    Ok(SyncCalendarOutcome::LeaseBusy) => {
                        // Quiet skip — not a failure, not a publish.
                    }
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

            // Do not hammer Google watch endpoints for revoked calendars.
            if cal.sync_status == "authorization_required" {
                continue;
            }

            if let Some(callback_url) = callback {
                match renew_watch_if_needed(
                    http,
                    watches,
                    &access,
                    cal,
                    callback_url,
                    now_unix,
                )
                .await
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
    }

    // Leftover-stop pass: channel rows that survived a failed best-effort stop
    // after disable/soft-delete. Soft-deleted calendars are invisible to
    // get_by_id / list_user_ids_with_calendars, so this pass uses unfiltered
    // reads. Stop does not need the webhook callback URL.
    stop_leftover_watch_channels(
        http,
        calendars,
        watches,
        tokens,
        oauth,
        &mut access_by_user,
        &mut report,
        now_unix,
    )
    .await;

    report
}

/// Retry `channels.stop` for watch rows whose calendar is disabled or
/// soft-deleted. Living + `sync_enabled` channels are the real subscription
/// (including renewal overlap) and are left alone.
///
/// On stop failure the channel rows remain for the next tick. Missing calendar
/// rows (hard-deleted or orphaned FK) cannot recover `user_id` — those are
/// skipped once per channel calendar id with an error string (no tokens).
async fn stop_leftover_watch_channels(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    watches: &dyn WatchChannelRepo,
    tokens: &dyn TokenRepo,
    oauth: &OAuthConfig,
    access_by_user: &mut HashMap<String, Result<GoogleAccess, TokenError>>,
    report: &mut CronReport,
    now_unix: i64,
) {
    let all_channels = match watches.list_all().await {
        Ok(rows) => rows,
        Err(err) => {
            report
                .errors
                .push(format!("list_all watch channels failed: {err}"));
            return;
        }
    };
    if all_channels.is_empty() {
        return;
    }

    // calendar_id → user_id for leftover calendars only.
    let mut leftover_by_user: HashMap<String, Vec<String>> = HashMap::new();
    let mut seen_calendar: HashSet<String> = HashSet::new();
    let mut missing_logged: HashSet<String> = HashSet::new();

    for channel in &all_channels {
        if !seen_calendar.insert(channel.calendar_id.clone()) {
            continue;
        }
        let cal = match calendars.get_by_id_unfiltered(&channel.calendar_id).await {
            Ok(row) => row,
            Err(err) => {
                report.errors.push(format!(
                    "get_by_id_unfiltered failed for calendar {}: {err}",
                    channel.calendar_id
                ));
                continue;
            }
        };
        let Some(cal) = cal else {
            // No user_id → cannot stop. Log once per calendar id.
            if missing_logged.insert(channel.calendar_id.clone()) {
                report.errors.push(format!(
                    "leftover watch channel for missing calendar {} — cannot stop (no user_id)",
                    channel.calendar_id
                ));
            }
            continue;
        };
        let is_leftover = cal.deleted_at.is_some() || !cal.sync_enabled;
        if !is_leftover {
            // Living + sync_enabled: real subscription (renewal overlap ok).
            continue;
        }
        leftover_by_user
            .entry(cal.user_id.clone())
            .or_default()
            .push(cal.id.clone());
    }

    for (user_id, calendar_ids) in leftover_by_user {
        let access_result = match access_by_user.get(&user_id) {
            Some(cached) => cached.clone(),
            None => {
                let result = refresh_if_needed(http, tokens, oauth, &user_id, now_unix).await;
                access_by_user.insert(user_id.clone(), result.clone());
                result
            }
        };
        let access = match access_result {
            Ok(access) => access,
            Err(err) => {
                // Do not hammer revoked grants; skip this user's leftover stops.
                report.errors.push(format!(
                    "token refresh failed for leftover-stop user {user_id}: {err}"
                ));
                continue;
            }
        };
        for calendar_id in calendar_ids {
            if let Err(err) =
                stop_watches_for_calendar(http, watches, &access, &calendar_id).await
            {
                report.errors.push(format!(
                    "failed to stop leftover watch channels for calendar {calendar_id}: {err}"
                ));
            }
        }
    }
}

// ──────────────────────────────────────────
// Webhook verification (ADR 0001 § Webhook)
// ──────────────────────────────────────────

/// Outcome of verifying a Google push notification (`X-Goog-*` headers)
/// against the stored watch channel.
///
/// Watches are **hints**. The caller MUST persist durable D1 work (via
/// [`persist_webhook_decision`]) before treating a non-[`Ignore`] decision
/// as accepted — HTTP 200 is only valid after that write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookDecision {
    /// 200, no durable work: unknown channel, bad/missing token, missing or
    /// disabled calendar, `sync` handshake, or any other non-actionable state.
    /// Verification failures never surface as 4xx/5xx (no existence leak,
    /// no Google retry hammer).
    Ignore,
    /// Verified `exists` on a living sync_enabled calendar. Caller MUST persist
    /// dirty before treating this as accepted work.
    EnqueueDirty { calendar_id: String },
    /// Verified `not_exists`: the calendar resource is gone. Caller MUST persist
    /// disable/tombstone before 200. Do not full-sync.
    CalendarGone { calendar_id: String },
}

/// Result of applying a [`WebhookDecision`] to D1 (dirty bump or disable).
///
/// Only [`DirtyAccepted`] and [`GoneDisabled`] mean durable work landed.
/// Persist failures must **not** be described as accepted work — cron recovers
/// lost `wait_until`, not a failed dirty write that never happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookPersistResult {
    /// Decision was [`WebhookDecision::Ignore`]; no repo writes.
    Ignored,
    /// Dirty generation was written. This is the only "accepted work" outcome
    /// for `exists`.
    DirtyAccepted { calendar_id: String },
    /// Tried to persist dirty and failed. MUST NOT be described as accepted work.
    DirtyPersistFailed { calendar_id: String },
    /// Disable was written for a gone calendar.
    GoneDisabled { calendar_id: String },
    /// Tried to disable a gone calendar and failed.
    GonePersistFailed { calendar_id: String },
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
/// Rules, in order:
/// 1. `stored` is `None` → [`WebhookDecision::Ignore`] (unknown channel).
/// 2. `presented_token` is `None` or `!tokens_match(stored.token, …)` →
///    [`WebhookDecision::Ignore`].
/// 3. `calendar` is `None` (missing or soft-deleted — `get_by_id` already
///    filters `deleted_at IS NULL`) or `!calendar.sync_enabled` →
///    [`WebhookDecision::Ignore`].
/// 4. `resource_state` == `"exists"` → [`WebhookDecision::EnqueueDirty`] for
///    `stored.calendar_id` (never `X-Goog-Resource-Id`).
/// 5. `resource_state` == `"not_exists"` → [`WebhookDecision::CalendarGone`]
///    for `stored.calendar_id` (verified channel + living enabled calendar).
/// 6. `"sync"` (the channel handshake) or anything else →
///    [`WebhookDecision::Ignore`].
///
/// The state comparison is case-sensitive and exact: Google sends bare
/// values like `exists`/`sync`/`not_exists`, so a whitespace-wrapped `exists`
/// is treated as an unknown state and ignored.
///
/// The caller must run [`persist_webhook_decision`] before HTTP 200 for any
/// non-[`Ignore`] outcome.
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
    match resource_state {
        "exists" => WebhookDecision::EnqueueDirty {
            calendar_id: stored.calendar_id.clone(),
        },
        "not_exists" => WebhookDecision::CalendarGone {
            calendar_id: stored.calendar_id.clone(),
        },
        _ => WebhookDecision::Ignore,
    }
}

/// Persists a verified webhook decision to D1 before the Worker returns 200.
///
/// - [`WebhookDecision::Ignore`] → no writes, [`WebhookPersistResult::Ignored`].
/// - [`WebhookDecision::EnqueueDirty`] → [`CalendarRepo::bump_dirty_requested`];
///   does **not** call Google or `sync_calendar`.
/// - [`WebhookDecision::CalendarGone`] → [`CalendarRepo::set_sync_enabled`]
///   `(false)`; does **not** bump dirty, delete events, or call Google.
///
/// `now_rfc3339` is supplied by the caller (never `SystemTime` in api-core).
pub async fn persist_webhook_decision(
    calendars: &dyn CalendarRepo,
    decision: &WebhookDecision,
    now_rfc3339: &str,
) -> WebhookPersistResult {
    match decision {
        WebhookDecision::Ignore => WebhookPersistResult::Ignored,
        WebhookDecision::EnqueueDirty { calendar_id } => {
            match calendars.bump_dirty_requested(calendar_id, now_rfc3339).await {
                Ok(()) => WebhookPersistResult::DirtyAccepted {
                    calendar_id: calendar_id.clone(),
                },
                Err(_) => WebhookPersistResult::DirtyPersistFailed {
                    calendar_id: calendar_id.clone(),
                },
            }
        }
        WebhookDecision::CalendarGone { calendar_id } => {
            match calendars
                .set_sync_enabled(calendar_id, false, now_rfc3339)
                .await
            {
                Ok(()) => WebhookPersistResult::GoneDisabled {
                    calendar_id: calendar_id.clone(),
                },
                Err(_) => WebhookPersistResult::GonePersistFailed {
                    calendar_id: calendar_id.clone(),
                },
            }
        }
    }
}

/// Subset of the `calendars.get` response: the label properties carrying the
/// calendar's event labels (`labelProperties.eventLabels[]`, each
/// `{id, backgroundColor}` — `name` is often null and is ignored).
#[derive(Debug, Deserialize)]
struct CalendarGetResponse {
    #[serde(default, rename = "labelProperties")]
    label_properties: Option<LabelProperties>,
}

#[derive(Debug, Deserialize)]
struct LabelProperties {
    #[serde(default, rename = "eventLabels")]
    event_labels: Option<Vec<GoogleEventLabel>>,
}

/// One `labelProperties.eventLabels` entry exactly as Google sends it.
#[derive(Debug, Deserialize)]
struct GoogleEventLabel {
    #[serde(default)]
    id: String,
    #[serde(default, rename = "backgroundColor")]
    background_color: String,
}

/// The cached form persisted onto `google_calendars.event_labels` — a JSON
/// array of `{"id","backgroundColor"}` (background colors canonicalized).
/// Deserializable again on the create path, where the caller's snapped hex
/// is resolved against the cache to pick the label id to send.
#[derive(Debug, Serialize, Deserialize)]
struct CachedEventLabel {
    id: String,
    #[serde(rename = "backgroundColor")]
    background_color: String,
}

/// Fetches a calendar's `labelProperties.eventLabels` via `calendars.get` and
/// persists them as a JSON array on the `google_calendars` row.
///
/// URL: `{GOOGLE_EVENTS_BASE_URL}/{url-encoded-id}` (NOT `.../events`).
/// Absent `labelProperties.eventLabels` → `[]` (holiday/reader calendars).
/// Each `backgroundColor` is canonicalized via [`canonicalize_hex`] when it
/// parses (lowercased `#rrggbb`); the original is kept when it does not.
/// Skipped entirely when `cal.event_labels` is non-empty (already fetched);
/// Google 4xx/5xx is a hard [`CalendarError::GoogleApi`] — an import must not
/// silently leave the cache empty.
async fn ensure_event_labels(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    access: &GoogleAccess,
    cal: &GoogleCalendar,
    now_rfc3339: &str,
) -> Result<(), CalendarError> {
    if !cal.event_labels.is_empty() {
        return Ok(());
    }
    let url = format!(
        "{GOOGLE_EVENTS_BASE_URL}/{}",
        encode_path_segment(&cal.google_calendar_id)
    );
    let (status, body) = http.get_bearer_raw(&url, &access.access_token).await?;
    if !(200..300).contains(&status) {
        return Err(CalendarError::GoogleApi(format!(
            "calendars.get returned {status} for calendar {}",
            cal.google_calendar_id
        )));
    }
    let get: CalendarGetResponse = serde_json::from_slice(&body)
        .map_err(|err| CalendarError::InvalidResponse(format!("calendars.get body: {err}")))?;
    let labels = get
        .label_properties
        .and_then(|props| props.event_labels)
        .unwrap_or_default();
    let cached: Vec<CachedEventLabel> = labels
        .into_iter()
        .filter_map(|label| {
            if label.id.is_empty() && label.background_color.is_empty() {
                return None;
            }
            Some(CachedEventLabel {
                id: label.id,
                background_color: canonicalize_hex(&label.background_color)
                    .unwrap_or(label.background_color),
            })
        })
        .collect();
    let json = serde_json::to_string(&cached)
        .map_err(|err| CalendarError::InvalidResponse(format!("serialize event labels: {err}")))?;
    calendars
        .set_event_labels(&cal.id, &json, now_rfc3339)
        .await?;
    Ok(())
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
async fn refresh_calendar_list(
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

/// Full or incremental sync of one calendar via the fenced replica walk
/// ([`crate::calendar_replica::sync_replica`], ADR 0005).
///
/// - Attempt is stamped **before** lease acquire / any Google fetch.
/// - A busy (unexpired foreign) lease is a quiet
///   [`SyncCalendarOutcome::LeaseBusy`] — not a failure, does not mark dirty
///   applied.
/// - After acquire, re-reads the calendar and **snapshots**
///   `dirty_requested_generation`. On successful publish, marks applied to
///   that snapshot (not the live requested value, so dirty bumps mid-run
///   remain dirty).
/// - Success requires apply finished **and** a non-empty terminal
///   `nextSyncToken` published under the still-held lease (empty `items` +
///   token still counts) → [`SyncCalendarOutcome::Published`].
/// - Every other path records failure without advancing the token or applied
///   generation.
/// - The lease is always released on the way out (success, skip-after-acquire,
///   or error).
///
/// `pub` for the webhook handler and the fallback cron. The request path
/// ([`list_events`]) never calls this — never-initialized calendars use the
/// window path instead.
pub async fn sync_calendar(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    access: &GoogleAccess,
    cal: &GoogleCalendar,
    now_rfc3339: &str,
) -> Result<SyncCalendarOutcome, CalendarError> {
    calendars
        .record_sync_attempt(&cal.id, now_rfc3339)
        .await?;
    let now_unix = rfc3339_to_unix_secs(now_rfc3339).unwrap_or(0);

    let owner = mint_lease_owner();
    let expires = lease_expires_at(now_rfc3339);
    let acquired = calendars
        .try_acquire_lease(&cal.id, &owner, now_rfc3339, &expires)
        .await?;
    if !acquired {
        // Another owner is working this calendar — not a failure.
        return Ok(SyncCalendarOutcome::LeaseBusy);
    }

    // Re-read cursor/fingerprint under the lease (not the stale snapshot).
    // Snapshot dirty generation at start so mid-run bumps stay dirty.
    let body_result = async {
        let fresh = calendars
            .get_by_id(&cal.id)
            .await?
            .ok_or_else(|| CalendarError::Invalid("calendar missing after lease acquire".into()))?;
        let dirty_snapshot = fresh.dirty_requested_generation;
        // Deploy backfill: empty event-label cache before the first fetch.
        ensure_event_labels(http, calendars, access, &fresh, now_rfc3339).await?;
        sync_replica(
            http,
            calendars,
            events,
            access,
            &fresh,
            &owner,
            now_rfc3339,
        )
        .await?;
        Ok::<i64, CalendarError>(dirty_snapshot)
    }
    .await;

    // Always release, even when the body failed.
    let _ = calendars
        .release_lease(&cal.id, &owner, now_rfc3339)
        .await;

    match body_result {
        Ok(dirty_snapshot) => {
            // After a successful publish: advance applied to the generation-at-start.
            // If this write fails, surface the error (replica is idempotent; cron
            // retries). Do not pretend applied advanced.
            calendars
                .mark_dirty_applied(&cal.id, dirty_snapshot, now_rfc3339)
                .await?;
            Ok(SyncCalendarOutcome::Published)
        }
        Err(err) => {
            let code = match &err {
                CalendarError::InvalidResponse(msg)
                    if msg.contains("missing nextSyncToken") =>
                {
                    SyncErrorCode::MissingSyncToken
                }
                _ => classify_sync_error(&err),
            };
            match persist_sync_failure(calendars, cal, code, now_unix, now_rfc3339).await {
                Ok(()) => Err(err),
                Err(persist_err) => Err(persist_err),
            }
        }
    }
}

/// Persist a classified failure without advancing the sync cursor.
///
/// Uses `cal.failure_streak + 1` for backoff (the in-memory snapshot at
/// invocation — attempt does not bump streak). Returns a repo error if the
/// health write itself fails so callers never drop health silently.
async fn persist_sync_failure(
    calendars: &dyn CalendarRepo,
    cal: &GoogleCalendar,
    code: SyncErrorCode,
    now_unix: i64,
    now_rfc3339: &str,
) -> Result<(), CalendarError> {
    let state = replica_state_for_error(code);
    let streak_for_backoff = cal.failure_streak.saturating_add(1);
    let retry = next_retry_rfc3339(now_unix, streak_for_backoff);
    calendars
        .record_sync_failure(
            &cal.id,
            code.as_str(),
            state.as_str(),
            &retry,
            now_rfc3339,
        )
        .await?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::models::{
        GoogleCalendar, GoogleOAuthToken, NewCalendar, NewCalendarEvent, NewToken, WatchChannel,
    };

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
            // Default for the `calendars.get` event-label backfill: any URL
            // that is a bare calendar resource (not `.../events`, not the
            // calendarList) returns "no labels", so first-import and sync tests
            // do not need to script a route for it.
            if url.contains("/calendar/v3/calendars/")
                && !url.contains("/events")
                && !url.contains("calendarList")
            {
                return (200, br#"{"labelProperties":{"eventLabels":[]}}"#.to_vec());
            }
            // Default for unscripted incremental calendarList ticks (cron):
            // empty delta, no nextSyncToken — no-op merge. Full-list URLs
            // (no syncToken) still panic so first-import tests keep their
            // explicit route.
            if url.contains("calendarList") && url.contains("syncToken=") {
                return (200, br#"{"items":[]}"#.to_vec());
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

    /// In-memory calendar repo: `upsert_batch` upserts by natural key
    /// `(user_id, google_calendar_id)` (like D1), so incremental calendarList
    /// tests do not invent duplicate rows. Lease methods mirror the V1 SQL
    /// semantics so replica tests exercise real fencing.
    struct FakeCalendarRepo {
        stored: Mutex<Vec<GoogleCalendar>>,
        upserted: Mutex<Vec<NewCalendar>>,
        sync_states: Mutex<Vec<(String, String, String)>>,
        disabled: Mutex<Vec<(String, bool)>>,
        label_updates: Mutex<Vec<(String, String)>>,
        next_id: Mutex<u64>,
        /// Per-user calendarList.list sync tokens.
        calendar_list_tokens: Mutex<HashMap<String, String>>,
        /// Count of `get_by_id` calls (for mid-walk lease-steal hooks).
        get_by_id_count: Mutex<usize>,
        /// After this many `get_by_id` calls, force `lease_owner` to `"thief"`
        /// on the matched row (None = disabled).
        steal_lease_after_get_by_id: Mutex<Option<usize>>,
        /// When true, `bump_dirty_requested` returns `RepoError::Backend`.
        fail_bump_dirty: Mutex<bool>,
        /// When true, `set_sync_enabled` returns `RepoError::Backend`.
        fail_set_sync_enabled: Mutex<bool>,
        /// After this many `get_by_id` calls, increment
        /// `dirty_requested_generation` on the matched row (None = disabled).
        /// Snapshot is taken before the bump so the caller still sees the
        /// pre-bump generation (mirrors mid-run dirty enqueue).
        bump_dirty_after_get_by_id: Mutex<Option<usize>>,
    }

    impl FakeCalendarRepo {
        fn with(calendars: Vec<GoogleCalendar>) -> Self {
            // Seed a non-empty list token for every distinct user so existing
            // cron tests take the incremental-empty path (FakeHttp default)
            // and do not orphan calendars on an unscripted full list.
            // `with(vec![])` seeds nothing → first-import tests stay full-list.
            let mut tokens = HashMap::new();
            let mut max_numeric_id = 0u64;
            for cal in &calendars {
                tokens
                    .entry(cal.user_id.clone())
                    .or_insert_with(|| "test-list-token".to_string());
                if let Some(n) = cal.id.strip_prefix("cal-").and_then(|s| s.parse().ok()) {
                    max_numeric_id = max_numeric_id.max(n);
                }
            }
            Self {
                stored: Mutex::new(calendars),
                upserted: Mutex::new(Vec::new()),
                sync_states: Mutex::new(Vec::new()),
                disabled: Mutex::new(Vec::new()),
                label_updates: Mutex::new(Vec::new()),
                // Avoid colliding with fixture ids like `cal-1`.
                next_id: Mutex::new(max_numeric_id.saturating_add(1).max(1)),
                calendar_list_tokens: Mutex::new(tokens),
                get_by_id_count: Mutex::new(0),
                steal_lease_after_get_by_id: Mutex::new(None),
                fail_bump_dirty: Mutex::new(false),
                fail_set_sync_enabled: Mutex::new(false),
                bump_dirty_after_get_by_id: Mutex::new(None),
            }
        }

        /// Test helper: plant a foreign or same-owner lease on a stored row.
        fn force_lease(&self, id: &str, owner: &str, expires_rfc3339: Option<&str>) {
            let mut stored = self.stored.lock().unwrap();
            if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
                cal.lease_owner = owner.to_string();
                cal.lease_expires_at = expires_rfc3339.map(str::to_string);
            }
        }

        fn apply_success_fields(
            cal: &mut GoogleCalendar,
            sync_token: &str,
            query_fingerprint: &str,
            now_rfc3339: &str,
        ) {
            cal.sync_token = sync_token.to_string();
            cal.last_synced_at = Some(now_rfc3339.to_string());
            cal.last_success_at = Some(now_rfc3339.to_string());
            cal.last_attempt_at = Some(now_rfc3339.to_string());
            cal.last_error_code = String::new();
            cal.failure_streak = 0;
            cal.next_retry_at = None;
            cal.initial_sync_complete = true;
            cal.sync_status = "ready".to_string();
            cal.sync_query_fingerprint = query_fingerprint.to_string();
            cal.cache_revision += 1;
            cal.full_sync_requested = false;
            cal.updated_at = now_rfc3339.to_string();
        }

        fn lease_held(cal: &GoogleCalendar, owner: &str, now_rfc3339: &str) -> bool {
            if cal.lease_owner != owner {
                return false;
            }
            match cal.lease_expires_at.as_deref() {
                None => true,
                Some(exp) => exp >= now_rfc3339,
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl CalendarRepo for FakeCalendarRepo {
        async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<GoogleCalendar>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|cal| cal.user_id == user_id && cal.deleted_at.is_none())
                .cloned()
                .collect())
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
            let mut count = self.get_by_id_count.lock().unwrap();
            *count += 1;
            let n = *count;
            drop(count);

            let mut stored = self.stored.lock().unwrap();
            // Snapshot first so the Nth call still sees our lease / dirty gen;
            // subsequent reads observe the post-hook mutation.
            // Match D1: only living rows.
            let result = stored
                .iter()
                .find(|cal| cal.id == id && cal.deleted_at.is_none())
                .cloned();
            if let Some(threshold) = *self.steal_lease_after_get_by_id.lock().unwrap() {
                if n == threshold {
                    if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
                        cal.lease_owner = "thief".to_string();
                        cal.lease_expires_at = Some("2099-01-01T00:00:00Z".to_string());
                    }
                }
            }
            if let Some(threshold) = *self.bump_dirty_after_get_by_id.lock().unwrap() {
                if n == threshold {
                    if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
                        cal.dirty_requested_generation =
                            cal.dirty_requested_generation.saturating_add(1);
                    }
                }
            }
            Ok(result)
        }

        async fn get_by_id_unfiltered(
            &self,
            id: &str,
        ) -> Result<Option<GoogleCalendar>, RepoError> {
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
            user_id: &str,
            google_cal_id: &str,
        ) -> Result<Option<GoogleCalendar>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|cal| {
                    cal.user_id == user_id
                        && cal.google_calendar_id == google_cal_id
                        && cal.deleted_at.is_none()
                })
                .cloned())
        }

        async fn upsert(&self, calendar: NewCalendar) -> Result<(), RepoError> {
            self.upsert_batch(vec![calendar]).await
        }

        async fn upsert_batch(&self, calendars: Vec<NewCalendar>) -> Result<(), RepoError> {
            for cal in calendars {
                self.upserted.lock().unwrap().push(cal.clone());
                let mut stored = self.stored.lock().unwrap();
                if let Some(existing) = stored.iter_mut().find(|row| {
                    row.user_id == cal.user_id && row.google_calendar_id == cal.google_calendar_id
                }) {
                    let resurrecting = existing.deleted_at.is_some();
                    existing.summary = cal.summary.clone();
                    existing.time_zone = cal.time_zone.clone();
                    existing.is_primary = cal.is_primary;
                    existing.access_role = cal.access_role.clone();
                    existing.updated_at = "2026-08-17T00:00:00Z".to_string();
                    existing.deleted_at = None;
                    if resurrecting {
                        // Returned calendar is a new appearance.
                        existing.sync_enabled = cal.sync_enabled;
                    }
                    // Living: keep sync_enabled, health, dirty gens, event_labels, id.
                    // Empty incoming sync_token / last_synced_at preserve stored
                    // (COALESCE), matching CALENDAR_UPSERT_SQL.
                    if !cal.sync_token.is_empty() {
                        existing.sync_token = cal.sync_token.clone();
                    }
                    if cal.last_synced_at.is_some() {
                        existing.last_synced_at = cal.last_synced_at.clone();
                    }
                } else {
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
                        // Freshly imported rows start with an empty label cache
                        // (cache miss) — `refresh_calendar_list` backfills it.
                        event_labels: String::new(),
                        // Health defaults: calendarList upsert must never write these.
                        sync_query_fingerprint: String::new(),
                        sync_status: String::new(),
                        initial_sync_complete: false,
                        last_attempt_at: None,
                        last_success_at: None,
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
                        created_at: "2026-08-17T00:00:00Z".to_string(),
                        updated_at: "2026-08-17T00:00:00Z".to_string(),
                        deleted_at: None,
                    };
                    *next += 1;
                    stored.push(row);
                }
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
            // Match D1 `CALENDAR_UPDATE_SYNC_STATE_SQL`: persist token +
            // last_synced_at onto the stored row so a re-read after
            // `sync_calendar` sees compat success.
            let mut stored = self.stored.lock().unwrap();
            if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
                cal.sync_token = sync_token.to_string();
                cal.last_synced_at = Some(last_synced_at_rfc3339.to_string());
            }
            Ok(())
        }

        async fn record_sync_attempt(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
            let mut stored = self.stored.lock().unwrap();
            if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
                cal.last_attempt_at = Some(now_rfc3339.to_string());
                cal.updated_at = now_rfc3339.to_string();
            }
            Ok(())
        }

        async fn record_sync_success(
            &self,
            id: &str,
            sync_token: &str,
            query_fingerprint: &str,
            now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            // Keep `sync_states` meaningful for tests that assert cursor
            // advancement (mirrors legacy `update_sync_state` recording).
            self.sync_states.lock().unwrap().push((
                id.to_string(),
                sync_token.to_string(),
                now_rfc3339.to_string(),
            ));
            let mut stored = self.stored.lock().unwrap();
            if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
                Self::apply_success_fields(cal, sync_token, query_fingerprint, now_rfc3339);
            }
            Ok(())
        }

        async fn record_sync_success_if_owner(
            &self,
            id: &str,
            sync_token: &str,
            query_fingerprint: &str,
            lease_owner: &str,
            now_rfc3339: &str,
        ) -> Result<bool, RepoError> {
            let mut stored = self.stored.lock().unwrap();
            let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) else {
                return Ok(false);
            };
            if !Self::lease_held(cal, lease_owner, now_rfc3339) {
                // Token must remain unchanged.
                return Ok(false);
            }
            Self::apply_success_fields(cal, sync_token, query_fingerprint, now_rfc3339);
            drop(stored);
            self.sync_states.lock().unwrap().push((
                id.to_string(),
                sync_token.to_string(),
                now_rfc3339.to_string(),
            ));
            Ok(true)
        }

        async fn record_sync_failure(
            &self,
            id: &str,
            error_code: &str,
            sync_status: &str,
            next_retry_rfc3339: &str,
            now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            let mut stored = self.stored.lock().unwrap();
            if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
                // Do not touch sync_token / last_success_at / last_synced_at.
                cal.last_error_code = error_code.to_string();
                cal.failure_streak += 1;
                cal.sync_status = sync_status.to_string();
                cal.next_retry_at = Some(next_retry_rfc3339.to_string());
                cal.updated_at = now_rfc3339.to_string();
            }
            Ok(())
        }

        async fn try_acquire_lease(
            &self,
            id: &str,
            owner: &str,
            now_rfc3339: &str,
            expires_rfc3339: &str,
        ) -> Result<bool, RepoError> {
            let mut stored = self.stored.lock().unwrap();
            let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) else {
                return Ok(false);
            };
            let can_take = cal.lease_owner.is_empty()
                || cal.lease_owner == owner
                || cal.lease_expires_at.is_none()
                || cal
                    .lease_expires_at
                    .as_deref()
                    .is_some_and(|exp| exp < now_rfc3339);
            if !can_take {
                return Ok(false);
            }
            cal.lease_owner = owner.to_string();
            cal.lease_expires_at = Some(expires_rfc3339.to_string());
            cal.updated_at = now_rfc3339.to_string();
            Ok(true)
        }

        async fn release_lease(
            &self,
            id: &str,
            owner: &str,
            now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            let mut stored = self.stored.lock().unwrap();
            if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
                if cal.lease_owner == owner {
                    cal.lease_owner = String::new();
                    cal.lease_expires_at = None;
                    cal.updated_at = now_rfc3339.to_string();
                }
            }
            Ok(())
        }

        async fn renew_lease(
            &self,
            id: &str,
            owner: &str,
            expires_rfc3339: &str,
            now_rfc3339: &str,
        ) -> Result<bool, RepoError> {
            let mut stored = self.stored.lock().unwrap();
            let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) else {
                return Ok(false);
            };
            if cal.lease_owner != owner {
                return Ok(false);
            }
            cal.lease_expires_at = Some(expires_rfc3339.to_string());
            cal.updated_at = now_rfc3339.to_string();
            Ok(true)
        }

        async fn bump_dirty_requested(
            &self,
            id: &str,
            now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            if *self.fail_bump_dirty.lock().unwrap() {
                return Err(RepoError::Backend("bump dirty failed".into()));
            }
            let mut stored = self.stored.lock().unwrap();
            if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
                if cal.deleted_at.is_none() {
                    cal.dirty_requested_generation =
                        cal.dirty_requested_generation.saturating_add(1);
                    cal.updated_at = now_rfc3339.to_string();
                }
            }
            Ok(())
        }

        async fn mark_dirty_applied(
            &self,
            id: &str,
            generation: i64,
            now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            let mut stored = self.stored.lock().unwrap();
            if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
                if cal.deleted_at.is_none() && cal.dirty_applied_generation < generation {
                    cal.dirty_applied_generation = generation;
                    cal.updated_at = now_rfc3339.to_string();
                }
            }
            Ok(())
        }

        async fn set_sync_enabled(
            &self,
            id: &str,
            enabled: bool,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            if *self.fail_set_sync_enabled.lock().unwrap() {
                return Err(RepoError::Backend("set_sync_enabled failed".into()));
            }
            self.disabled.lock().unwrap().push((id.to_string(), enabled));
            // Mutate stored so a re-read after 404-disable shows `disabled`
            // in the health envelope.
            let mut stored = self.stored.lock().unwrap();
            if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
                cal.sync_enabled = enabled;
            }
            Ok(())
        }

        async fn set_event_labels(
            &self,
            id: &str,
            event_labels_json: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            self.label_updates
                .lock()
                .unwrap()
                .push((id.to_string(), event_labels_json.to_string()));
            let mut stored = self.stored.lock().unwrap();
            if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
                cal.event_labels = event_labels_json.to_string();
            }
            Ok(())
        }

        async fn delete(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
            let mut stored = self.stored.lock().unwrap();
            if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
                cal.deleted_at = Some(now_rfc3339.to_string());
                cal.updated_at = now_rfc3339.to_string();
            }
            Ok(())
        }

        async fn get_calendar_list_sync_token(
            &self,
            user_id: &str,
        ) -> Result<Option<String>, RepoError> {
            Ok(self
                .calendar_list_tokens
                .lock()
                .unwrap()
                .get(user_id)
                .cloned())
        }

        async fn set_calendar_list_sync_token(
            &self,
            user_id: &str,
            token: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            self.calendar_list_tokens
                .lock()
                .unwrap()
                .insert(user_id.to_string(), token.to_string());
            Ok(())
        }

        async fn list_user_ids_with_calendars(&self) -> Result<Vec<String>, RepoError> {
            let mut ids: Vec<String> = self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|cal| cal.deleted_at.is_none())
                .map(|cal| cal.user_id.clone())
                .collect();
            ids.sort();
            ids.dedup();
            Ok(ids)
        }
    }

    /// In-memory event repo: upserts materialize rows so the follow-up
    /// time-range query returns them, and every call is recorded.
    struct FakeEventRepo {
        stored: Mutex<Vec<CalendarEvent>>,
        upserted_batch: Mutex<Vec<NewCalendarEvent>>,
        upserted_single: Mutex<Option<(String, NewCalendarEvent)>>,
        ranged: Mutex<Vec<(String, String, String)>>,
        deleted: Mutex<Vec<(String, String)>>,
        deleted_by_google_event_id: Mutex<Vec<(String, String)>>,
        /// `(calendar_id, older_than, now)` — replica walk must never push.
        deleted_stale: Mutex<Vec<(String, String, String)>>,
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
                deleted: Mutex::new(Vec::new()),
                deleted_by_google_event_id: Mutex::new(Vec::new()),
                deleted_stale: Mutex::new(Vec::new()),
                fail_upsert: Mutex::new(false),
                fail_delete: Mutex::new(false),
                next_id: Mutex::new(1),
            }
        }

        /// Natural-key upsert including soft-deleted rows: on hit, update
        /// fields, clear `deleted_at`, return the existing id; on miss, insert
        /// and return a new id. Mirrors D1 + `EVENT_UPSERT_ON_CONFLICT`.
        fn apply_upsert(&self, event: NewCalendarEvent, now_rfc3339: &str) -> String {
            let mut stored = self.stored.lock().unwrap();
            if let Some(existing) = stored.iter_mut().find(|row| {
                row.calendar_id == event.calendar_id && row.google_event_id == event.google_event_id
            }) {
                let id = existing.id.clone();
                // Preserve task_id when incoming is empty (SQL COALESCE).
                let task_id = if event.task_id.is_empty() {
                    existing.task_id.clone()
                } else {
                    event.task_id.clone()
                };
                existing.google_etag = event.google_etag;
                existing.google_updated_at = event.google_updated_at;
                existing.last_synced_at = event.last_synced_at;
                existing.title = event.title;
                existing.description = event.description;
                existing.start_time = event.start_time;
                existing.end_time = event.end_time;
                existing.recurrence = event.recurrence;
                existing.task_id = task_id;
                existing.ical_uid = event.ical_uid;
                existing.sequence = event.sequence;
                existing.status = event.status;
                existing.recurring_event_id = event.recurring_event_id;
                existing.original_start = event.original_start;
                existing.start_time_zone = event.start_time_zone;
                existing.end_time_zone = event.end_time_zone;
                existing.is_all_day = event.is_all_day;
                existing.raw_json = event.raw_json;
                existing.updated_at = now_rfc3339.to_string();
                existing.deleted_at = None;
                return id;
            }
            let mut next = self.next_id.lock().unwrap();
            let id = format!("evt-{next}");
            *next += 1;
            stored.push(row_from_new_event(event, id.clone(), now_rfc3339));
            id
        }

        /// Mirrors GET projection filters (`timed_masters_and_exceptions`).
        fn in_projection(event: &CalendarEvent) -> bool {
            event.deleted_at.is_none()
                && !event.is_all_day
                && (event.status.is_empty() || event.status != "cancelled")
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
            *self.upserted_single.lock().unwrap() =
                Some((event.google_event_id.clone(), event.clone()));
            Ok(self.apply_upsert(event, now_rfc3339))
        }

        async fn upsert_batch(
            &self,
            events: Vec<NewCalendarEvent>,
            now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            if *self.fail_upsert.lock().unwrap() {
                return Err(RepoError::Backend("cache write failed".into()));
            }
            self.upserted_batch.lock().unwrap().extend(events.clone());
            for event in events {
                self.apply_upsert(event, now_rfc3339);
            }
            Ok(())
        }

        async fn get_by_id(&self, id: &str) -> Result<Option<CalendarEvent>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|event| event.deleted_at.is_none() && event.id == id)
                .cloned())
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
            self.ranged.lock().unwrap().push((
                user_id.to_string(),
                start_rfc3339.to_string(),
                end_rfc3339.to_string(),
            ));
            // Mirrors EVENT_LIST_BY_USER_ID_AND_TIME_RANGE_SQL projection +
            // overlap (start < window_end AND end > window_start).
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|event| {
                    Self::in_projection(event)
                        && event.start_time.as_str() < end_rfc3339
                        && event.end_time.as_str() > start_rfc3339
                })
                .cloned()
                .collect())
        }

        async fn list_running_by_user_id(
            &self,
            _user_id: &str,
            now_rfc3339: &str,
        ) -> Result<Vec<CalendarEvent>, RepoError> {
            // Mirrors EVENT_LIST_RUNNING_BY_USER_ID_SQL: projection +
            // task-tagged + `start_time <= now < end_time`.
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|event| {
                    Self::in_projection(event)
                        && !event.task_id.is_empty()
                        && event.start_time.as_str() <= now_rfc3339
                        && event.end_time.as_str() > now_rfc3339
                })
                .cloned()
                .collect())
        }

        async fn delete(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
            self.deleted
                .lock()
                .unwrap()
                .push((id.to_string(), now_rfc3339.to_string()));
            if *self.fail_delete.lock().unwrap() {
                return Err(RepoError::Backend("cache delete failed".into()));
            }
            let mut stored = self.stored.lock().unwrap();
            if let Some(event) = stored.iter_mut().find(|event| event.id == id) {
                event.deleted_at = Some(now_rfc3339.to_string());
            }
            Ok(())
        }

        async fn delete_by_google_event_id(
            &self,
            calendar_id: &str,
            google_event_id: &str,
            now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            self.deleted_by_google_event_id
                .lock()
                .unwrap()
                .push((calendar_id.to_string(), google_event_id.to_string()));
            if *self.fail_delete.lock().unwrap() {
                return Err(RepoError::Backend("cache delete failed".into()));
            }
            let mut stored = self.stored.lock().unwrap();
            if let Some(event) = stored.iter_mut().find(|event| {
                event.calendar_id == calendar_id && event.google_event_id == google_event_id
            }) {
                event.deleted_at = Some(now_rfc3339.to_string());
            }
            Ok(())
        }

        async fn delete_stale(
            &self,
            calendar_id: &str,
            older_than_rfc3339: &str,
            now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            self.deleted_stale.lock().unwrap().push((
                calendar_id.to_string(),
                older_than_rfc3339.to_string(),
                now_rfc3339.to_string(),
            ));
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

        async fn list_all(&self) -> Result<Vec<WatchChannel>, RepoError> {
            Ok(self.stored.lock().unwrap().clone())
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
            // `"[]"` = label cache already fetched (no labels) — existing sync
            // tests skip the `calendars.get` backfill. Tests exercising the
            // cache-miss path construct rows with an empty string explicitly.
            event_labels: "[]".to_string(),
            sync_query_fingerprint: String::new(),
            // Empty string matches serde default for missing columns; production
            // backfill uses `never_initialized` / `ready` / `disabled`.
            sync_status: String::new(),
            initial_sync_complete: false,
            last_attempt_at: None,
            last_success_at: None,
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

        // calendarList fetched, rows upserted (sync_enabled defaults true),
        // then each imported row's event-label cache backfilled via
        // `calendars.get` (empty `event_labels` = cache miss).
        let gets = http.gets.lock().unwrap();
        assert_eq!(
            gets.len(),
            5,
            "calendarList + 2 calendars.get backfill + 2 events.list"
        );
        assert!(gets[0].contains("calendarList"), "{gets:?}");
        assert!(
            gets[1].contains("/calendars/primary%40example.com") && !gets[1].contains("/events"),
            "{gets:?}"
        );
        assert!(
            gets[2].contains("/calendars/en.usa%23holiday%40group.v.calendar.google.com")
                && !gets[2].contains("/events"),
            "{gets:?}"
        );
        let upserted = calendars.upserted.lock().unwrap();
        assert_eq!(upserted.len(), 2);
        assert!(upserted.iter().all(|cal| cal.sync_enabled));
        assert!(upserted.iter().any(|cal| cal.is_primary));
        assert_eq!(upserted[1].google_calendar_id, "en.usa#holiday@group.v.calendar.google.com");

        // Freshly imported calendars are never-initialized: Path A window
        // fetch (not replica). Encoded `#`/`@` show in the events.list URLs.
        assert!(gets[3].contains("primary%40example.com/events"), "{gets:?}");
        assert!(gets[3].contains("singleEvents=true"), "{gets:?}");
        assert!(!gets[3].contains("syncToken"), "{gets:?}");
        assert!(gets[4].contains("en.usa%23holiday%40group.v.calendar.google.com/events"), "{gets:?}");
        assert!(gets[4].contains("singleEvents=true"), "{gets:?}");
        // Window discards nextSyncToken — no record_sync_success.
        assert!(calendars.sync_states.lock().unwrap().is_empty());
        let stored = calendars.stored.lock().unwrap();
        assert_eq!(stored.len(), 2);
        assert!(stored.iter().all(|c| c.sync_token.is_empty()));
        assert!(stored.iter().all(|c| !c.initial_sync_complete));
        assert!(stored.iter().all(|c| c.dirty_requested_generation == 1));
        assert_eq!(output.source, "window");
    }

    #[test]
    fn first_import_backfills_event_label_cache() {
        // Empty store: first contact imports calendarList, then every imported
        // row (empty `event_labels` = cache miss) is fetched via
        // `calendars.get` and the event labels are persisted.
        let http = FakeHttp::new(vec![
            ("calendarList", 200, CALENDAR_LIST_JSON),
            ("/events", 200, r#"{"items":[],"nextSyncToken":"st-1"}"#),
            (
                "/calendars/",
                200,
                r##"{"id":"ignored","labelProperties":{"eventLabels":[
                    {"id":"1","backgroundColor":"#AC725E","name":null},
                    {"id":"2","backgroundColor":"#d06b64"},
                    {"id":"3","backgroundColor":"not-a-hex"}
                ]}}"##,
            ),
        ]);
        let calendars = FakeCalendarRepo::with(vec![]);
        let events = FakeEventRepo::new();

        let watches = FakeWatchChannelRepo::new();
        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
        ))
        .unwrap();
        assert!(output.sync_errors.is_empty(), "{:?}", output.sync_errors);

        // Both imported rows carry the cached labels: background colors are
        // canonicalized to lowercase #rrggbb when they parse, the original is
        // kept otherwise, and null `name` is ignored.
        let stored = calendars.stored.lock().unwrap();
        assert_eq!(stored.len(), 2);
        let expected = r##"[{"id":"1","backgroundColor":"#ac725e"},{"id":"2","backgroundColor":"#d06b64"},{"id":"3","backgroundColor":"not-a-hex"}]"##;
        assert!(
            stored.iter().all(|cal| cal.event_labels == expected),
            "label cache filled on every imported row: {stored:?}"
        );
        // The writes went through the dedicated set_event_labels path.
        let updates = calendars.label_updates.lock().unwrap();
        assert_eq!(updates.len(), 2);
        assert!(updates.iter().all(|(_, json)| json == &expected));
    }

    #[test]
    fn calendar_get_without_label_properties_stores_empty_array() {
        // Holiday-style `calendars.get` body: no `labelProperties` at all.
        // The cache must read `"[]"` (fetched, no labels) — never stay empty.
        // `/events` must be listed before `/calendars/` so events.list URLs
        // (which contain both substrings) do not match the calendars.get body.
        let http = FakeHttp::new(vec![
            ("calendarList", 200, CALENDAR_LIST_JSON),
            ("/events", 200, r#"{"items":[],"nextSyncToken":"st-1"}"#),
            (
                "/calendars/",
                200,
                r#"{"id":"en.usa#holiday@group.v.calendar.google.com","summary":"Holidays","timeZone":"UTC"}"#,
            ),
        ]);
        let calendars = FakeCalendarRepo::with(vec![]);
        let events = FakeEventRepo::new();

        let watches = FakeWatchChannelRepo::new();
        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
        ))
        .unwrap();
        assert!(output.sync_errors.is_empty(), "{:?}", output.sync_errors);

        let stored = calendars.stored.lock().unwrap();
        assert_eq!(stored.len(), 2);
        assert!(
            stored.iter().all(|cal| cal.event_labels == "[]"),
            "absent labelProperties must cache as an empty array: {stored:?}"
        );
    }

    #[test]
    fn sync_skips_calendars_get_when_label_cache_is_filled() {
        // Both variants of a filled cache — `"[]"` (fetched, no labels) and a
        // non-empty JSON array — must skip the `calendars.get` backfill.
        let mut empty_labels = calendar("cal-1", "primary@example.com", true);
        empty_labels.event_labels = "[]".to_string();
        let mut filled_labels = calendar("cal-2", "work@example.com", true);
        filled_labels.event_labels = r##"[{"id":"1","backgroundColor":"#ac725e"}]"##.to_string();

        let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
        let calendars = FakeCalendarRepo::with(vec![empty_labels, filled_labels]);
        let events = FakeEventRepo::new();

        let rows = calendars.stored.lock().unwrap().clone();
        for cal in &rows {
            pollster::block_on(sync_calendar(
                &http, &calendars, &events, &access(), cal, "2023-11-14T22:13:20Z",
            ))
            .unwrap();
        }

        // events.list still ran for both calendars; no URL is the bare
        // calendar resource (which would be the backfill GET).
        let gets = http.gets.lock().unwrap();
        assert_eq!(gets.len(), 2, "one events.list per calendar: {gets:?}");
        assert!(
            gets.iter().all(|url| url.contains("/events")),
            "no bare calendars.get when the cache is filled: {gets:?}"
        );
        assert!(
            calendars.label_updates.lock().unwrap().is_empty(),
            "no label writes"
        );
    }

    #[test]
    fn never_synced_calendar_window_fetch_before_cache_query() {
        // `calendar()` defaults to `last_synced_at: None` — first paint uses
        // Path A (window), not Path B (replica).
        let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let watches = FakeWatchChannelRepo::new();
        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
        ))
        .unwrap();

        let gets = http.gets.lock().unwrap();
        assert_eq!(gets.len(), 1);
        assert!(gets[0].contains("/calendars/primary%40example.com/events"), "{gets:?}");
        assert!(gets[0].contains("singleEvents=true"), "{gets:?}");
        assert!(gets[0].contains("orderBy=startTime"), "{gets:?}");
        assert!(gets[0].contains("maxResults=250"), "{gets:?}");
        assert!(gets[0].contains("timeMin="), "{gets:?}");
        assert!(gets[0].contains("timeMax="), "{gets:?}");
        assert!(!gets[0].contains("syncToken"), "{gets:?}");
        assert!(!gets[0].contains("singleEvents=false"), "{gets:?}");

        // Write-through under lease: both items upserted.
        let upserted = events.upserted_batch.lock().unwrap();
        assert_eq!(upserted.len(), 2);
        assert!(upserted.iter().all(|event| event.calendar_id == "cal-1"));
        assert!(upserted.iter().all(|event| event.last_synced_at == "2023-11-14T22:13:20Z"));
        assert_eq!(upserted[0].title, "Standup");
        assert_eq!(upserted[0].start_time, "2026-08-18T09:00:00Z");
        assert_eq!(upserted[0].recurrence, r#"["RRULE:FREQ=DAILY"]"#);

        // Window must not publish nextSyncToken / record_sync_success.
        assert!(calendars.sync_states.lock().unwrap().is_empty());

        let ranged = events.ranged.lock().unwrap();
        assert_eq!(*ranged, vec![("u-1".to_string(), "2026-08-01T00:00:00Z".to_string(), "2026-09-01T00:00:00Z".to_string())]);

        assert_eq!(output.events.len(), 2);
        assert!(output.sync_errors.is_empty());
        assert_eq!(output.source, "window");

        // Health still never_initialized — window does not flip ready.
        assert_eq!(output.sync.calendars.len(), 1);
        let health = &output.sync.calendars[0];
        assert_eq!(health.calendar_id, "cal-1");
        assert_eq!(
            health.state,
            crate::calendar_sync::CalendarReplicaState::NeverInitialized
        );
        assert!(!health.initial_sync_complete);
        assert!(health.error_code.is_none());
        assert_eq!(
            output.sync.status,
            crate::calendar_sync::SyncAggregateStatus::Degraded
        );

        let stored = calendars.stored.lock().unwrap();
        assert!(stored[0].sync_token.is_empty(), "window discards nextSyncToken");
        assert!(!stored[0].initial_sync_complete);
        assert_eq!(stored[0].cache_revision, 0);
        assert_eq!(stored[0].dirty_requested_generation, 1);
        assert!(stored[0].last_error_code.is_empty());
        assert!(stored[0].last_success_at.is_none());
        assert!(stored[0].last_attempt_at.is_none());
    }

    #[test]
    fn empty_items_with_next_sync_token_is_replica_success() {
        // Completed empty incremental: items=[] + nextSyncToken is publication
        // on the replica path only.
        let http = FakeHttp::new(vec![("/events", 200, r#"{"items":[],"nextSyncToken":"st-empty"}"#)]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let cal = calendars.stored.lock().unwrap()[0].clone();

        pollster::block_on(sync_calendar(
            &http, &calendars, &events, &access(), &cal, "2023-11-14T22:13:20Z",
        ))
        .unwrap();

        let stored = calendars.stored.lock().unwrap();
        assert_eq!(stored[0].sync_token, "st-empty");
        assert_eq!(
            stored[0].last_success_at.as_deref(),
            Some("2023-11-14T22:13:20Z")
        );
        assert_eq!(
            stored[0].last_attempt_at.as_deref(),
            Some("2023-11-14T22:13:20Z")
        );
        assert_eq!(stored[0].failure_streak, 0);
        assert_eq!(stored[0].sync_status, "ready");
        assert!(stored[0].initial_sync_complete);
        assert_eq!(stored[0].cache_revision, 1);
        assert!(stored[0].last_error_code.is_empty());
    }

    #[test]
    fn window_empty_page_with_next_sync_token_does_not_publish() {
        // Google may return nextSyncToken on a window query; Path A throws it away.
        let http = FakeHttp::new(vec![("/events", 200, r#"{"items":[],"nextSyncToken":"st-window"}"#)]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let watches = FakeWatchChannelRepo::new();

        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
        ))
        .unwrap();

        assert!(output.sync_errors.is_empty(), "{:?}", output.sync_errors);
        assert!(output.events.is_empty());
        assert_eq!(output.source, "window");
        assert!(calendars.sync_states.lock().unwrap().is_empty());

        let stored = calendars.stored.lock().unwrap();
        assert!(stored[0].sync_token.is_empty());
        assert!(!stored[0].initial_sync_complete);
        assert_eq!(stored[0].dirty_requested_generation, 1);
        assert_eq!(
            output.sync.calendars[0].state,
            crate::calendar_sync::CalendarReplicaState::NeverInitialized
        );
    }

    #[test]
    fn google_list_failure_records_attempt_not_success() {
        // Seed a previously healthy calendar; call sync_calendar directly
        // (list_events would be cache-only), then list_events for the envelope.
        let mut cal = calendar("cal-1", "primary@example.com", true);
        cal.sync_token = "old-tok".to_string();
        cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
        cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
        cal.initial_sync_complete = true;
        cal.failure_streak = 0;
        cal.sync_status = "ready".to_string();

        let http = FakeHttp::new(vec![("/events", 500, "")]);
        let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
        let events = FakeEventRepo::new();

        let err = pollster::block_on(sync_calendar(
            &http, &calendars, &events, &access(), &cal, "2023-11-14T22:13:20Z",
        ))
        .unwrap_err();
        assert!(
            matches!(err, CalendarError::GoogleApi(ref m) if m.contains("500")),
            "{err:?}"
        );

        let stored = calendars.stored.lock().unwrap();
        assert_eq!(
            stored[0].last_attempt_at.as_deref(),
            Some("2023-11-14T22:13:20Z")
        );
        assert_eq!(
            stored[0].last_success_at.as_deref(),
            Some("2023-11-14T21:00:00Z"),
            "success timestamp must not move on failure"
        );
        assert_eq!(stored[0].sync_token, "old-tok");
        assert_eq!(stored[0].failure_streak, 1);
        assert_eq!(stored[0].sync_status, "retrying");
        assert_eq!(stored[0].last_error_code, "google_transient");
        assert!(calendars.sync_states.lock().unwrap().is_empty());
        drop(stored);

        let watches = FakeWatchChannelRepo::new();
        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
        ))
        .unwrap();

        // Cache-only: no additional Google calls beyond the failed sync.
        assert_eq!(http.gets.lock().unwrap().len(), 1);
        assert_eq!(
            output.sync.status,
            crate::calendar_sync::SyncAggregateStatus::Degraded
        );
        let health = &output.sync.calendars[0];
        assert_eq!(health.state, crate::calendar_sync::CalendarReplicaState::Retrying);
        assert_eq!(health.error_code.as_deref(), Some("google_transient"));

        let json = serde_json::to_string(&output.sync).unwrap();
        assert!(!json.contains("old-tok"), "{json}");
    }

    #[test]
    fn upsert_failure_records_storage_transient_without_advancing_token() {
        let mut cal = calendar("cal-1", "primary@example.com", true);
        cal.sync_token = "old-tok".to_string();
        cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
        cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
        cal.initial_sync_complete = true;
        cal.sync_status = "ready".to_string();

        let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
        let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
        let events = FakeEventRepo::new();
        *events.fail_upsert.lock().unwrap() = true;

        let err = pollster::block_on(sync_calendar(
            &http, &calendars, &events, &access(), &cal, "2023-11-14T22:13:20Z",
        ))
        .unwrap_err();
        assert!(matches!(err, CalendarError::Repo(_)), "{err:?}");

        let stored = calendars.stored.lock().unwrap();
        assert_eq!(stored[0].sync_token, "old-tok");
        assert_eq!(
            stored[0].last_success_at.as_deref(),
            Some("2023-11-14T21:00:00Z")
        );
        assert_eq!(
            stored[0].last_attempt_at.as_deref(),
            Some("2023-11-14T22:13:20Z")
        );
        assert_eq!(stored[0].failure_streak, 1);
        assert_eq!(stored[0].sync_status, "retrying");
        assert_eq!(stored[0].last_error_code, "storage_transient");
        assert!(calendars.sync_states.lock().unwrap().is_empty());
    }

    #[test]
    fn missing_terminal_next_sync_token_is_not_success() {
        // Single page 200 with items but no nextSyncToken / nextPageToken.
        let body = r#"{"items":[
            {"id": "evt-1", "summary": "Standup",
             "start": {"dateTime": "2026-08-18T09:00:00Z"},
             "end": {"dateTime": "2026-08-18T09:30:00Z"}}
        ]}"#;
        let mut cal = calendar("cal-1", "primary@example.com", true);
        cal.sync_token = "old-tok".to_string();
        cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
        cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
        cal.initial_sync_complete = true;
        cal.sync_status = "ready".to_string();

        let http = FakeHttp::new(vec![("/events", 200, body)]);
        let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
        let events = FakeEventRepo::new();

        let err = pollster::block_on(sync_calendar(
            &http, &calendars, &events, &access(), &cal, "2023-11-14T22:13:20Z",
        ))
        .unwrap_err();
        assert!(
            matches!(err, CalendarError::InvalidResponse(ref m) if m.contains("missing nextSyncToken")),
            "{err:?}"
        );

        // Apply may have happened; publication did not.
        assert_eq!(events.upserted_batch.lock().unwrap().len(), 1);

        let stored = calendars.stored.lock().unwrap();
        assert_eq!(stored[0].sync_token, "old-tok");
        assert_eq!(
            stored[0].last_success_at.as_deref(),
            Some("2023-11-14T21:00:00Z")
        );
        assert_eq!(
            stored[0].last_synced_at.as_deref(),
            Some("2023-11-14T21:00:00Z")
        );
        assert_eq!(
            stored[0].last_attempt_at.as_deref(),
            Some("2023-11-14T22:13:20Z")
        );
        assert_eq!(stored[0].last_error_code, "missing_sync_token");
        assert_eq!(stored[0].sync_status, "retrying");
        assert_eq!(stored[0].failure_streak, 1);
        assert!(calendars.sync_states.lock().unwrap().is_empty());
        drop(stored);

        // list_events is cache-only and surfaces the persisted health +
        // does not re-sync; first-paint path would also surface sync_errors.
        let watches = FakeWatchChannelRepo::new();
        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
        ))
        .unwrap();
        assert_eq!(
            output.sync.calendars[0].error_code.as_deref(),
            Some("missing_sync_token")
        );
        assert_eq!(
            output.sync.calendars[0].state,
            crate::calendar_sync::CalendarReplicaState::Retrying
        );
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
        assert_eq!(output.source, "cache");
        assert_eq!(
            calendars.stored.lock().unwrap()[0].dirty_requested_generation,
            0,
            "cache-only must not bump dirty"
        );

        // Health envelope: old last_synced_at → degraded + stale, but compat
        // state is still ready / initial_sync_complete.
        assert_eq!(
            output.sync.status,
            crate::calendar_sync::SyncAggregateStatus::Degraded
        );
        assert_eq!(output.sync.calendars.len(), 1);
        let health = &output.sync.calendars[0];
        assert!(health.stale);
        assert_eq!(health.state, crate::calendar_sync::CalendarReplicaState::Ready);
        assert!(health.initial_sync_complete);
        assert!(health.error_code.is_none());
    }

    #[test]
    fn mixed_initialized_and_never_init_source_is_mixed() {
        let mut ready = calendar("cal-ready", "ready@example.com", true);
        ready.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
        ready.initial_sync_complete = true;
        ready.sync_token = "tok-ready".to_string();
        let never = calendar("cal-new", "new@example.com", true);
        let http = FakeHttp::new(vec![
            (
                "new%40example.com/events",
                200,
                r#"{"items":[{"id":"n1","summary":"New","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}}],"nextSyncToken":"st-discard"}"#,
            ),
        ]);
        let calendars = FakeCalendarRepo::with(vec![ready, never]);
        let events = FakeEventRepo::new();
        let watches = FakeWatchChannelRepo::new();

        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
        ))
        .unwrap();

        assert_eq!(output.source, "mixed");
        let gets = http.gets.lock().unwrap();
        assert_eq!(gets.len(), 1, "window only for never-init: {gets:?}");
        assert!(gets[0].contains("new%40example.com"), "{gets:?}");
        assert!(gets[0].contains("singleEvents=true"), "{gets:?}");
        assert!(!gets[0].contains("syncToken"), "{gets:?}");

        let stored = calendars.stored.lock().unwrap();
        let ready_row = stored.iter().find(|c| c.id == "cal-ready").unwrap();
        let new_row = stored.iter().find(|c| c.id == "cal-new").unwrap();
        assert_eq!(ready_row.sync_token, "tok-ready");
        assert_eq!(ready_row.dirty_requested_generation, 0);
        assert!(new_row.sync_token.is_empty());
        assert_eq!(new_row.dirty_requested_generation, 1);
        assert!(!new_row.initial_sync_complete);
        assert_eq!(output.events.len(), 1);
        assert_eq!(output.events[0].google_event_id, "n1");
    }

    #[test]
    fn list_events_returns_sync_envelope_when_event_window_is_empty() {
        // Already synced, recent last_synced_at, zero events in the window.
        let mut cal = calendar("cal-1", "primary@example.com", true);
        cal.last_synced_at = Some("2023-11-14T22:00:00Z".to_string());
        let http = FakeHttp::new(vec![]);
        let calendars = FakeCalendarRepo::with(vec![cal]);
        let events = FakeEventRepo::new();

        let watches = FakeWatchChannelRepo::new();
        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
        ))
        .unwrap();

        assert!(output.events.is_empty());
        assert!(http.gets.lock().unwrap().is_empty());
        assert_eq!(output.source, "cache");
        assert_eq!(output.sync.calendars.len(), 1);
        assert_eq!(output.sync.calendars[0].calendar_id, "cal-1");
        assert_eq!(
            output.sync.calendars[0].state,
            crate::calendar_sync::CalendarReplicaState::Ready
        );
        assert!(!output.sync.calendars[0].stale);
        assert_eq!(
            output.sync.status,
            crate::calendar_sync::SyncAggregateStatus::Ready
        );
    }

    #[test]
    fn list_events_sync_envelope_never_leaks_sync_token() {
        let mut cal = calendar("cal-1", "primary@example.com", true);
        cal.sync_token = "secret-sync-token-xyz".to_string();
        cal.last_error_code = "storage_transient".to_string();
        cal.sync_status = "retrying".to_string();
        cal.last_success_at = Some("2023-11-10T00:00:00Z".to_string());
        cal.last_synced_at = Some("2023-11-10T00:00:00Z".to_string());
        cal.initial_sync_complete = true;
        cal.lease_owner = "lease-secret-should-not-leak".to_string();

        let http = FakeHttp::new(vec![]);
        let calendars = FakeCalendarRepo::with(vec![cal]);
        let events = FakeEventRepo::new();

        let watches = FakeWatchChannelRepo::new();
        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
        ))
        .unwrap();

        assert_eq!(
            output.sync.calendars[0].error_code.as_deref(),
            Some("storage_transient")
        );
        assert_eq!(
            output.sync.calendars[0].state,
            crate::calendar_sync::CalendarReplicaState::Retrying
        );

        let json = serde_json::to_string(&output.sync).unwrap();
        assert!(json.contains("storage_transient"), "{json}");
        assert!(!json.contains("secret-sync-token-xyz"), "{json}");
        assert!(!json.contains("sync_token"), "{json}");
        assert!(!json.contains("lease-secret-should-not-leak"), "{json}");
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
        // Window path still ran (and failed) → source is window; dirty was bumped.
        assert_eq!(output.source, "window");

        let stored = calendars.stored.lock().unwrap();
        assert!(!stored[0].sync_enabled);
        // Window path does not record_sync_failure — disable alone drives health.
        assert!(stored[0].sync_token.is_empty());
        assert_eq!(stored[0].dirty_requested_generation, 1);
        drop(stored);
        assert_eq!(
            output.sync.calendars[0].state,
            crate::calendar_sync::CalendarReplicaState::Disabled
        );
    }

    #[test]
    fn events_list_410_after_retry_records_gone() {
        // First 410 retries in-invocation; second 410 is gone (not success).
        let http = FakeHttp::new(vec![("/events", 410, "")]);
        let mut cal = calendar("cal-1", "primary@example.com", true);
        cal.sync_token = "stale-token".to_string();
        let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
        let events = FakeEventRepo::new();

        let err = pollster::block_on(sync_calendar(
            &http, &calendars, &events, &access(), &cal, "2023-11-14T22:13:20Z",
        ))
        .unwrap_err();
        assert!(
            matches!(err, CalendarError::GoogleApi(ref m) if m.contains("410")),
            "{err:?}"
        );

        let stored = calendars.stored.lock().unwrap();
        assert_eq!(stored[0].sync_token, "stale-token", "token not cleared on gone");
        assert_eq!(stored[0].last_error_code, "gone");
        assert_eq!(stored[0].sync_status, "retrying");
        assert_eq!(stored[0].failure_streak, 1);
        assert!(stored[0].last_success_at.is_none());
        assert_eq!(
            stored[0].last_attempt_at.as_deref(),
            Some("2023-11-14T22:13:20Z")
        );
    }

    #[test]
    fn window_410_is_error_without_merge_full_or_token_write() {
        // Window never sends syncToken; 410 is a plain error (no merge-full loop).
        let http = FakeHttp::new(vec![("/events", 410, "")]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let watches = FakeWatchChannelRepo::new();
        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
        ))
        .unwrap();

        assert_eq!(http.gets.lock().unwrap().len(), 1, "no 410 retry on window");
        assert_eq!(output.sync_errors.len(), 1);
        assert!(output.sync_errors[0].contains("410"), "{}", output.sync_errors[0]);
        assert!(calendars.sync_states.lock().unwrap().is_empty());
        assert!(calendars.stored.lock().unwrap()[0].sync_token.is_empty());
        assert_eq!(
            calendars.stored.lock().unwrap()[0].dirty_requested_generation,
            1
        );
        assert_eq!(output.source, "window");
    }

    #[test]
    fn window_follows_next_page_token_without_publishing_sync_token() {
        let page_one = r#"{"items":[{"id":"p1","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}}],"nextPageToken":"tok-2"}"#;
        let page_two = r#"{"items":[{"id":"p2","start":{"dateTime":"2026-08-18T10:00:00Z"},"end":{"dateTime":"2026-08-18T10:30:00Z"}}],"nextSyncToken":"st-page"}"#;
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
        assert!(gets[0].contains("singleEvents=true"), "{gets:?}");
        assert!(gets[1].contains("pageToken=tok-2"), "{gets:?}");
        assert!(gets.iter().all(|u| !u.contains("syncToken")), "{gets:?}");
        assert_eq!(events.upserted_batch.lock().unwrap().len(), 2);
        assert_eq!(output.events.len(), 2);
        assert!(output.sync_errors.is_empty(), "{:?}", output.sync_errors);
        assert!(calendars.sync_states.lock().unwrap().is_empty());
        assert!(calendars.stored.lock().unwrap()[0].sync_token.is_empty());
        assert_eq!(
            output.sync.calendars[0].state,
            crate::calendar_sync::CalendarReplicaState::NeverInitialized
        );
        assert_eq!(output.source, "window");
    }

    fn seeded_event(
        id: &str,
        calendar_id: &str,
        google_event_id: &str,
        task_id: &str,
    ) -> CalendarEvent {
        CalendarEvent {
            id: id.to_string(),
            calendar_id: calendar_id.to_string(),
            google_event_id: google_event_id.to_string(),
            google_etag: String::new(),
            google_updated_at: String::new(),
            last_synced_at: "2023-11-14T21:00:00Z".to_string(),
            title: google_event_id.to_string(),
            description: String::new(),
            start_time: "2026-08-18T09:00:00Z".to_string(),
            end_time: "2026-08-18T09:30:00Z".to_string(),
            recurrence: String::new(),
            task_id: task_id.to_string(),
            ical_uid: String::new(),
            sequence: 0,
            status: "confirmed".to_string(),
            recurring_event_id: String::new(),
            original_start: String::new(),
            start_time_zone: String::new(),
            end_time_zone: String::new(),
            is_all_day: false,
            raw_json: String::new(),
            created_at: "2023-11-14T21:00:00Z".to_string(),
            updated_at: "2023-11-14T21:00:00Z".to_string(),
            deleted_at: None,
        }
    }

    #[test]
    fn replica_paginated_apply_publishes_token_only_after_last_page() {
        let page_one = r#"{"items":[{"id":"p1","summary":"A","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}}],"nextPageToken":"tok-2"}"#;
        let page_two = r#"{"items":[{"id":"p2","summary":"B","start":{"dateTime":"2026-08-18T10:00:00Z"},"end":{"dateTime":"2026-08-18T10:30:00Z"}}],"nextSyncToken":"st-final"}"#;
        let now = "2023-11-14T22:13:20Z";

        // Phase 1: page 2 fails — page 1 applied, token stays old.
        let mut cal = calendar("cal-1", "primary@example.com", true);
        cal.sync_token = "old-tok".to_string();
        cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
        cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
        cal.initial_sync_complete = true;
        let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
        let events = FakeEventRepo::new();
        let http_fail = FakeHttp::new(vec![
            ("pageToken=tok-2", 500, ""),
            ("/events", 200, page_one),
        ]);
        let err = pollster::block_on(sync_calendar(
            &http_fail, &calendars, &events, &access(), &cal, now,
        ))
        .unwrap_err();
        assert!(matches!(err, CalendarError::GoogleApi(ref m) if m.contains("500")), "{err:?}");
        assert_eq!(events.upserted_batch.lock().unwrap().len(), 1);
        assert_eq!(events.upserted_batch.lock().unwrap()[0].google_event_id, "p1");
        assert_eq!(calendars.stored.lock().unwrap()[0].sync_token, "old-tok");
        assert!(calendars.sync_states.lock().unwrap().is_empty());
        assert!(events.deleted_stale.lock().unwrap().is_empty());

        // Phase 2: both pages succeed — terminal token published + fingerprint.
        let http_ok = FakeHttp::new(vec![
            ("pageToken=tok-2", 200, page_two),
            ("/events", 200, page_one),
        ]);
        pollster::block_on(sync_calendar(
            &http_ok, &calendars, &events, &access(), &cal, now,
        ))
        .unwrap();
        let upserted = events.upserted_batch.lock().unwrap();
        assert!(upserted.iter().any(|e| e.google_event_id == "p1"));
        assert!(upserted.iter().any(|e| e.google_event_id == "p2"));
        let stored = calendars.stored.lock().unwrap();
        assert_eq!(stored[0].sync_token, "st-final");
        assert_eq!(
            stored[0].sync_query_fingerprint,
            crate::calendar_sync::replica_query_fingerprint()
        );
        assert!(events.deleted_stale.lock().unwrap().is_empty());
    }

    #[test]
    fn replica_410_first_page_merge_full_preserves_task_id_and_ghosts() {
        let merge = r#"{"items":[
            {"id":"keep","summary":"Keep","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}}
        ],"nextSyncToken":"st-merge"}"#;
        let mut cal = calendar("cal-1", "primary@example.com", true);
        cal.sync_token = "stale-token".to_string();
        cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
        cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
        cal.initial_sync_complete = true;
        let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
        let events = FakeEventRepo::new();
        events.stored.lock().unwrap().extend([
            seeded_event("e-keep", "cal-1", "keep", "keep-me"),
            seeded_event("e-ghost", "cal-1", "ghost", ""),
        ]);
        let http = FakeHttp::new(vec![
            ("syncToken=stale-token", 410, ""),
            ("/events", 200, merge),
        ]);
        pollster::block_on(sync_calendar(
            &http, &calendars, &events, &access(), &cal, "2023-11-14T22:13:20Z",
        ))
        .unwrap();

        let stored_ev = events.stored.lock().unwrap();
        let keep = stored_ev.iter().find(|e| e.google_event_id == "keep").unwrap();
        assert_eq!(keep.task_id, "keep-me", "COALESCE must preserve task_id");
        let ghost = stored_ev.iter().find(|e| e.google_event_id == "ghost").unwrap();
        assert!(ghost.deleted_at.is_none(), "ghost must not be truncated");
        assert!(events.deleted_stale.lock().unwrap().is_empty());
        assert_eq!(calendars.stored.lock().unwrap()[0].sync_token, "st-merge");
    }

    #[test]
    fn replica_410_later_page_merge_full_keeps_partial_apply() {
        let page_one = r#"{"items":[{"id":"a","summary":"A","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}}],"nextPageToken":"tok-2"}"#;
        let merge = r#"{"items":[
            {"id":"a","summary":"A2","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}},
            {"id":"keep","summary":"Keep","start":{"dateTime":"2026-08-18T11:00:00Z"},"end":{"dateTime":"2026-08-18T11:30:00Z"}}
        ],"nextSyncToken":"st-mf"}"#;
        let mut cal = calendar("cal-1", "primary@example.com", true);
        cal.sync_token = "old".to_string();
        cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
        cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
        cal.initial_sync_complete = true;
        let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
        let events = FakeEventRepo::new();
        events
            .stored
            .lock()
            .unwrap()
            .push(seeded_event("e-keep", "cal-1", "keep", "keep-me"));
        let http = FakeHttp::new(vec![
            ("pageToken=tok-2", 410, ""),
            ("syncToken=old", 200, page_one),
            ("/events", 200, merge),
        ]);
        pollster::block_on(sync_calendar(
            &http, &calendars, &events, &access(), &cal, "2023-11-14T22:13:20Z",
        ))
        .unwrap();

        assert!(events.deleted_stale.lock().unwrap().is_empty());
        assert_eq!(calendars.stored.lock().unwrap()[0].sync_token, "st-mf");
        let keep = events
            .stored
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.google_event_id == "keep")
            .unwrap()
            .clone();
        assert_eq!(keep.task_id, "keep-me");
        // Partial apply of A from page1 is OK (also in merge-full).
        assert!(events
            .stored
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.google_event_id == "a"));
    }

    #[test]
    fn replica_failure_after_page1_delete_replays_idempotently() {
        let page_one = r#"{"items":[
            {"id":"gone","status":"cancelled"},
            {"id":"live","summary":"Live","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}}
        ],"nextPageToken":"tok-2"}"#;
        let page_two = r#"{"items":[{"id":"p2","summary":"P2","start":{"dateTime":"2026-08-18T10:00:00Z"},"end":{"dateTime":"2026-08-18T10:30:00Z"}}],"nextSyncToken":"st-done"}"#;
        let mut cal = calendar("cal-1", "primary@example.com", true);
        cal.sync_token = "old-tok".to_string();
        cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
        cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
        cal.initial_sync_complete = true;
        let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
        let events = FakeEventRepo::new();
        events
            .stored
            .lock()
            .unwrap()
            .push(seeded_event("e-gone", "cal-1", "gone", ""));

        let http_fail = FakeHttp::new(vec![
            ("pageToken=tok-2", 500, ""),
            ("/events", 200, page_one),
        ]);
        let err = pollster::block_on(sync_calendar(
            &http_fail,
            &calendars,
            &events,
            &access(),
            &cal,
            "2023-11-14T22:13:20Z",
        ))
        .unwrap_err();
        assert!(matches!(err, CalendarError::GoogleApi(_)), "{err:?}");
        assert_eq!(calendars.stored.lock().unwrap()[0].sync_token, "old-tok");
        assert!(events
            .stored
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.google_event_id == "gone")
            .unwrap()
            .deleted_at
            .is_some());

        let http_ok = FakeHttp::new(vec![
            ("pageToken=tok-2", 200, page_two),
            ("/events", 200, page_one),
        ]);
        pollster::block_on(sync_calendar(
            &http_ok,
            &calendars,
            &events,
            &access(),
            &cal,
            "2023-11-14T22:13:20Z",
        ))
        .unwrap();
        assert_eq!(calendars.stored.lock().unwrap()[0].sync_token, "st-done");
        // Idempotent: still soft-deleted once, live + p2 present.
        assert!(events
            .stored
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.google_event_id == "gone")
            .unwrap()
            .deleted_at
            .is_some());
        assert!(events
            .stored
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.google_event_id == "live" && e.deleted_at.is_none()));
        assert!(events
            .stored
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.google_event_id == "p2"));
    }

    #[test]
    fn window_poison_on_one_calendar_does_not_block_sibling() {
        // list_events Path A: invalid JSON on A must not block B's window.
        // Neither calendar publishes a replica token / becomes ready.
        let cal_a = calendar("cal-a", "a@example.com", true);
        let cal_b = calendar("cal-b", "b@example.com", true);
        let http = FakeHttp::new(vec![
            ("a%40example.com/events", 200, "not-json{{{"),
            (
                "b%40example.com/events",
                200,
                r#"{"items":[{"id":"b1","summary":"B","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}}],"nextSyncToken":"st-b"}"#,
            ),
        ]);
        let calendars = FakeCalendarRepo::with(vec![cal_a, cal_b]);
        let events = FakeEventRepo::new();
        let watches = FakeWatchChannelRepo::new();
        let output = pollster::block_on(list_events(
            &http,
            &calendars,
            &events,
            &watches,
            &access(),
            "u-1",
            "2026-08-01T00:00:00Z",
            "2026-09-01T00:00:00Z",
            NOW_UNIX,
            None,
        ))
        .unwrap();

        assert_eq!(output.sync_errors.len(), 1, "{:?}", output.sync_errors);
        assert!(
            output.sync_errors[0].contains("cal-a") || output.sync_errors[0].contains("a@"),
            "{:?}",
            output.sync_errors
        );
        assert_eq!(output.events.len(), 1);
        assert_eq!(output.events[0].google_event_id, "b1");
        assert_eq!(output.source, "window");

        let stored = calendars.stored.lock().unwrap();
        let a = stored.iter().find(|c| c.id == "cal-a").unwrap();
        let b = stored.iter().find(|c| c.id == "cal-b").unwrap();
        // Window path does not record_sync_failure — health stays never_init.
        assert!(a.sync_token.is_empty());
        assert!(!a.initial_sync_complete);
        assert_eq!(a.dirty_requested_generation, 1);
        assert!(b.sync_token.is_empty(), "window nextSyncToken discarded");
        assert!(!b.initial_sync_complete);
        assert_eq!(b.dirty_requested_generation, 1);
        assert_eq!(
            output.sync.calendars.iter().find(|c| c.calendar_id == "cal-a").unwrap().state,
            crate::calendar_sync::CalendarReplicaState::NeverInitialized
        );
        assert_eq!(
            output.sync.calendars.iter().find(|c| c.calendar_id == "cal-b").unwrap().state,
            crate::calendar_sync::CalendarReplicaState::NeverInitialized
        );
    }

    #[test]
    fn replica_poison_on_one_calendar_records_mapping_poison() {
        // Replica path (sync_calendar) still classifies invalid JSON as
        // mapping_poison via record_sync_failure.
        let cal = calendar("cal-a", "a@example.com", true);
        let http = FakeHttp::new(vec![("a%40example.com/events", 200, "not-json{{{")]);
        let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
        let events = FakeEventRepo::new();
        let err = pollster::block_on(sync_calendar(
            &http, &calendars, &events, &access(), &cal, "2023-11-14T22:13:20Z",
        ))
        .unwrap_err();
        assert!(matches!(err, CalendarError::InvalidResponse(_)), "{err:?}");
        let stored = calendars.stored.lock().unwrap();
        assert_eq!(stored[0].last_error_code, "mapping_poison");
        assert_eq!(stored[0].sync_status, "retrying");
        assert!(stored[0].sync_token.is_empty());
    }

    #[test]
    fn replica_unexpired_foreign_lease_skips_without_failure() {
        let mut cal = calendar("cal-1", "primary@example.com", true);
        cal.sync_token = "old-tok".to_string();
        cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
        cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
        cal.initial_sync_complete = true;
        let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
        calendars.force_lease("cal-1", "other-owner", Some("2099-01-01T00:00:00Z"));
        let events = FakeEventRepo::new();
        let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);

        pollster::block_on(sync_calendar(
            &http,
            &calendars,
            &events,
            &access(),
            &cal,
            "2023-11-14T22:13:20Z",
        ))
        .unwrap();

        assert!(
            http.gets.lock().unwrap().is_empty(),
            "must not fetch when lease is held"
        );
        assert!(events.upserted_batch.lock().unwrap().is_empty());
        let stored = calendars.stored.lock().unwrap();
        assert_eq!(stored[0].sync_token, "old-tok");
        assert_eq!(stored[0].failure_streak, 0);
        assert_eq!(stored[0].lease_owner, "other-owner");
        // Attempt was stamped; no failure.
        assert_eq!(
            stored[0].last_attempt_at.as_deref(),
            Some("2023-11-14T22:13:20Z")
        );
        assert!(stored[0].last_error_code.is_empty());
    }

    #[test]
    fn replica_expired_foreign_lease_is_stolen_and_walk_succeeds() {
        let cal = calendar("cal-1", "primary@example.com", true);
        let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
        calendars.force_lease("cal-1", "stale-owner", Some("2020-01-01T00:00:00Z"));
        let events = FakeEventRepo::new();
        let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);

        pollster::block_on(sync_calendar(
            &http,
            &calendars,
            &events,
            &access(),
            &cal,
            "2023-11-14T22:13:20Z",
        ))
        .unwrap();

        assert_eq!(calendars.stored.lock().unwrap()[0].sync_token, "st-9");
        assert!(calendars.stored.lock().unwrap()[0].lease_owner.is_empty());
        assert_eq!(events.upserted_batch.lock().unwrap().len(), 2);
    }

    #[test]
    fn record_sync_success_if_owner_rejects_wrong_owner() {
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        calendars.force_lease("cal-1", "owner-a", Some("2099-01-01T00:00:00Z"));
        let ok = pollster::block_on(calendars.record_sync_success_if_owner(
            "cal-1",
            "new-tok",
            "fp",
            "owner-b",
            "2023-11-14T22:13:20Z",
        ))
        .unwrap();
        assert!(!ok);
        assert!(calendars.stored.lock().unwrap()[0].sync_token.is_empty());
        assert!(calendars.sync_states.lock().unwrap().is_empty());
    }

    #[test]
    fn replica_mid_walk_lease_loss_does_not_publish_token() {
        let page_one = r#"{"items":[{"id":"p1","summary":"A","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}}],"nextPageToken":"tok-2"}"#;
        let page_two = r#"{"items":[{"id":"p2","summary":"B","start":{"dateTime":"2026-08-18T10:00:00Z"},"end":{"dateTime":"2026-08-18T10:30:00Z"}}],"nextSyncToken":"st-stolen"}"#;
        let mut cal = calendar("cal-1", "primary@example.com", true);
        cal.sync_token = "old-tok".to_string();
        cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
        cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
        cal.initial_sync_complete = true;
        let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
        // After re-read (#1) + page1 fence (#2), steal before renew.
        *calendars.steal_lease_after_get_by_id.lock().unwrap() = Some(2);
        let events = FakeEventRepo::new();
        let http = FakeHttp::new(vec![
            ("pageToken=tok-2", 200, page_two),
            ("/events", 200, page_one),
        ]);

        let err = pollster::block_on(sync_calendar(
            &http,
            &calendars,
            &events,
            &access(),
            &cal,
            "2023-11-14T22:13:20Z",
        ))
        .unwrap_err();
        assert!(
            matches!(err, CalendarError::Invalid(ref m) if m.contains("lost replica lease")),
            "{err:?}"
        );
        // Page 1 may have applied.
        assert!(events
            .upserted_batch
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.google_event_id == "p1"));
        assert_eq!(calendars.stored.lock().unwrap()[0].sync_token, "old-tok");
        assert!(calendars.sync_states.lock().unwrap().is_empty());
        // Must not have applied page 2 / published st-stolen.
        assert!(!events
            .upserted_batch
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.google_event_id == "p2"));
    }

    #[test]
    fn all_day_and_no_time_events_are_upserted_out_of_projection() {
        // Window write-through reuses classify_replica_item; token not advanced.
        let body = r#"{"items":[
            {"id": "all-day", "summary": "Holiday",
             "start": {"date": "2026-08-01"}, "end": {"date": "2026-08-02"}},
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
        assert_eq!(upserted.len(), 3, "all-day, no-time, and timed are upserted");
        let all_day = upserted.iter().find(|e| e.google_event_id == "all-day").unwrap();
        assert!(all_day.is_all_day);
        assert_eq!(all_day.start_time, "2026-08-01T00:00:00Z");
        assert_eq!(all_day.end_time, "2026-08-02T00:00:00Z");
        let no_time = upserted.iter().find(|e| e.google_event_id == "no-time").unwrap();
        assert!(!no_time.is_all_day);
        assert!(no_time.start_time.is_empty());
        assert!(
            events.deleted_by_google_event_id.lock().unwrap().is_empty(),
            "all-day/no-time are not deleted"
        );
        // GET projection only surfaces the timed living event.
        assert_eq!(output.events.len(), 1);
        assert_eq!(output.events[0].google_event_id, "real");
        assert!(calendars.sync_states.lock().unwrap().is_empty());
        assert!(calendars.stored.lock().unwrap()[0].sync_token.is_empty());
        assert_eq!(output.source, "window");
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

        // Ordinary cancelled events (no recurringEventId) are soft-deleted.
        assert_eq!(
            *events.deleted_by_google_event_id.lock().unwrap(),
            vec![("cal-1".to_string(), "cancelled".to_string())]
        );
        let upserted = events.upserted_batch.lock().unwrap();
        assert_eq!(upserted.len(), 1, "only the timed, non-cancelled event");
        assert_eq!(upserted[0].google_event_id, "real");
        assert_eq!(output.events.len(), 1);
        // Window does not advance the replica token.
        assert!(calendars.sync_states.lock().unwrap().is_empty());
        assert!(calendars.stored.lock().unwrap()[0].sync_token.is_empty());
        assert_eq!(output.source, "window");
    }

    #[test]
    fn cancelled_exception_is_upserted_not_deleted_and_out_of_projection() {
        let body = r#"{"items":[
            {"id": "exc-1", "status": "cancelled", "recurringEventId": "master-1",
             "originalStartTime": {"dateTime": "2026-08-20T15:00:00Z"}},
            {"id": "real", "summary": "Real",
             "start": {"dateTime": "2026-08-18T09:00:00Z"}, "end": {"dateTime": "2026-08-18T09:30:00Z"}}
        ], "nextSyncToken": "st-exc"}"#;
        let http = FakeHttp::new(vec![("/events", 200, body)]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let watches = FakeWatchChannelRepo::new();
        let output = pollster::block_on(list_events(
            &http, &calendars, &events, &watches, &access(), "u-1",
            "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
        ))
        .unwrap();

        assert!(
            events.deleted_by_google_event_id.lock().unwrap().is_empty(),
            "cancelled exceptions must not be soft-deleted"
        );
        let upserted = events.upserted_batch.lock().unwrap();
        assert_eq!(upserted.len(), 2);
        let exc = upserted.iter().find(|e| e.google_event_id == "exc-1").unwrap();
        assert_eq!(exc.status, "cancelled");
        assert_eq!(exc.recurring_event_id, "master-1");
        assert_eq!(exc.start_time, "2026-08-20T15:00:00Z");
        // Projection: only the living timed event.
        assert_eq!(output.events.len(), 1);
        assert_eq!(output.events[0].google_event_id, "real");
        assert!(calendars.sync_states.lock().unwrap().is_empty());
        assert!(calendars.stored.lock().unwrap()[0].sync_token.is_empty());
        assert_eq!(output.source, "window");
    }

    #[test]
    fn natural_key_upsert_returns_persisted_id() {
        let events = FakeEventRepo::new();
        let mut row = NewCalendarEvent {
            calendar_id: "cal-1".into(),
            google_event_id: "g-1".into(),
            google_etag: "e1".into(),
            google_updated_at: "2026-08-17T10:00:00Z".into(),
            last_synced_at: "2026-08-17T12:00:00Z".into(),
            title: "First".into(),
            description: String::new(),
            start_time: "2026-08-18T09:00:00Z".into(),
            end_time: "2026-08-18T09:30:00Z".into(),
            recurrence: String::new(),
            task_id: String::new(),
            ical_uid: "uid".into(),
            sequence: 0,
            status: "confirmed".into(),
            recurring_event_id: String::new(),
            original_start: String::new(),
            start_time_zone: String::new(),
            end_time_zone: String::new(),
            is_all_day: false,
            raw_json: "{}".into(),
        };
        let id1 = pollster::block_on(events.upsert(row.clone(), "2026-08-17T12:00:00Z")).unwrap();
        row.title = "Second".into();
        row.sequence = 1;
        let id2 = pollster::block_on(events.upsert(row, "2026-08-17T13:00:00Z")).unwrap();
        assert_eq!(id1, id2, "second upsert must return the persisted id");
        let stored = events.stored.lock().unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].id, id1);
        assert_eq!(stored[0].title, "Second");
        assert_eq!(stored[0].sequence, 1);
        assert!(stored[0].deleted_at.is_none());
    }

    #[test]
    fn upsert_after_delete_clears_deleted_at() {
        let events = FakeEventRepo::new();
        let row = NewCalendarEvent {
            calendar_id: "cal-1".into(),
            google_event_id: "g-1".into(),
            google_etag: "e1".into(),
            google_updated_at: "2026-08-17T10:00:00Z".into(),
            last_synced_at: "2026-08-17T12:00:00Z".into(),
            title: "Live".into(),
            description: String::new(),
            start_time: "2026-08-18T09:00:00Z".into(),
            end_time: "2026-08-18T09:30:00Z".into(),
            recurrence: String::new(),
            task_id: String::new(),
            ical_uid: String::new(),
            sequence: 0,
            status: "confirmed".into(),
            recurring_event_id: String::new(),
            original_start: String::new(),
            start_time_zone: String::new(),
            end_time_zone: String::new(),
            is_all_day: false,
            raw_json: String::new(),
        };
        let id = pollster::block_on(events.upsert(row.clone(), "2026-08-17T12:00:00Z")).unwrap();
        pollster::block_on(events.delete_by_google_event_id("cal-1", "g-1", "2026-08-17T12:30:00Z"))
            .unwrap();
        assert!(
            events.stored.lock().unwrap()[0].deleted_at.is_some(),
            "soft-deleted"
        );
        let id2 = pollster::block_on(events.upsert(row, "2026-08-17T13:00:00Z")).unwrap();
        assert_eq!(id, id2);
        let stored = events.stored.lock().unwrap();
        assert_eq!(stored.len(), 1);
        assert!(stored[0].deleted_at.is_none(), "upsert restores deleted_at");
        assert_eq!(stored[0].title, "Live");
    }

    #[test]
    fn window_cancelled_event_delete_failure_does_not_advance_sync_token() {
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
        // Delete failure aborts the window apply: no upsert of the living
        // sibling, token must not advance (window never publishes anyway).
        assert!(events.upserted_batch.lock().unwrap().is_empty());
        assert!(calendars.sync_states.lock().unwrap().is_empty());
        assert_eq!(output.sync_errors.len(), 1);
        assert!(
            output.sync_errors[0].contains("cache delete failed"),
            "{}",
            output.sync_errors[0]
        );

        let stored = calendars.stored.lock().unwrap();
        assert!(stored[0].sync_token.is_empty(), "token never advanced");
        assert!(!stored[0].initial_sync_complete);
        assert_eq!(stored[0].dirty_requested_generation, 1);
        // Window path does not record_sync_failure.
        assert!(stored[0].last_error_code.is_empty());
        assert_eq!(
            output.sync.calendars[0].state,
            crate::calendar_sync::CalendarReplicaState::NeverInitialized
        );
        assert_eq!(output.source, "window");
    }

    #[test]
    fn replica_cancelled_event_delete_failure_records_storage_transient() {
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
        let cal = calendars.stored.lock().unwrap()[0].clone();

        let err = pollster::block_on(sync_calendar(
            &http, &calendars, &events, &access(), &cal, "2023-11-14T22:13:20Z",
        ))
        .unwrap_err();
        assert!(matches!(err, CalendarError::Repo(_)), "{err:?}");
        assert!(calendars.sync_states.lock().unwrap().is_empty());
        let stored = calendars.stored.lock().unwrap();
        assert!(stored[0].sync_token.is_empty());
        assert_eq!(stored[0].last_error_code, "storage_transient");
        assert_eq!(stored[0].failure_streak, 1);
        assert_eq!(stored[0].sync_status, "retrying");
        assert_eq!(
            stored[0].last_attempt_at.as_deref(),
            Some("2023-11-14T22:13:20Z")
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
        assert_eq!(output.source, "window");
        assert_eq!(
            calendars.stored.lock().unwrap()[0].dirty_requested_generation,
            1
        );
    }

    // ──────────────────────────────────────────
    // list_calendars
    // ──────────────────────────────────────────

    #[test]
    fn list_calendars_empty_store_imports_calendar_list_without_syncing_events() {
        // Only a calendarList route + the FakeHttp default for bare
        // `calendars.get` backfill URLs: any events.list or watch call would
        // make the fake panic — this test proves list_calendars does neither.
        let http = FakeHttp::new(vec![("calendarList", 200, CALENDAR_LIST_JSON)]);
        let calendars = FakeCalendarRepo::with(vec![]);

        let output = pollster::block_on(list_calendars(
            &http,
            &calendars,
            &access(),
            "u-1",
            "2026-08-17T00:00:00Z",
        ))
        .unwrap();

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

        // The calendarList import plus one `calendars.get` event-label
        // backfill per imported row (the FakeHttp default answers them) —
        // and nothing else.
        let gets = http.gets.lock().unwrap();
        assert_eq!(
            gets.len(),
            3,
            "calendarList import + 2 event-label backfills"
        );
        assert!(gets[0].contains("calendarList"), "{gets:?}");
        assert!(
            gets[1..].iter().all(|url| url.contains("/calendar/v3/calendars/")
                && !url.contains("/events")
                && !url.contains("calendarList")),
            "backfills hit the bare calendar resource: {gets:?}"
        );
        assert!(http.posts.lock().unwrap().is_empty(), "no watch POSTs");
        assert_eq!(calendars.upserted.lock().unwrap().len(), 2);
        assert!(
            calendars.sync_states.lock().unwrap().is_empty(),
            "no event sync"
        );
        // Every imported row got the backfilled cache (`[]` = fetched, no
        // labels — the FakeHttp default body has no labels).
        assert_eq!(calendars.label_updates.lock().unwrap().len(), 2);
        assert!(
            calendars
                .stored
                .lock()
                .unwrap()
                .iter()
                .all(|cal| cal.event_labels == "[]"),
            "all imported rows have a fetched label cache"
        );
    }

    #[test]
    fn list_calendars_non_empty_store_is_cache_only() {
        // No routes: any HTTP call would make the fake panic.
        let http = FakeHttp::new(vec![]);
        let calendars =
            FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);

        let output = pollster::block_on(list_calendars(
            &http,
            &calendars,
            &access(),
            "u-1",
            "2026-08-17T00:00:00Z",
        ))
        .unwrap();

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

        let err = pollster::block_on(list_calendars(
            &http,
            &calendars,
            &access(),
            "u-1",
            "2026-08-17T00:00:00Z",
        ))
        .unwrap_err();

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
        // The first-paint window still runs with no callback configured.
        let gets = http.gets.lock().unwrap();
        assert_eq!(gets.len(), 1);
        assert!(gets[0].contains("singleEvents=true"), "{gets:?}");
        assert_eq!(output.events.len(), 2);
        assert!(output.sync_errors.is_empty());
        assert_eq!(output.source, "window");
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
        assert_eq!(output.events.len(), 2, "first-paint window unaffected");
        assert!(output.sync_errors.is_empty());
        assert_eq!(output.source, "window");
    }

    #[test]
    fn never_synced_calendar_is_watched_then_window_fetched() {
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

        // First-paint window (not replica) populated the cache; token not published.
        let gets = http.gets.lock().unwrap();
        assert_eq!(gets.len(), 1);
        assert!(gets[0].contains("singleEvents=true"), "{gets:?}");
        assert!(!gets[0].contains("syncToken"), "{gets:?}");
        assert_eq!(events.upserted_batch.lock().unwrap().len(), 2);
        assert_eq!(output.events.len(), 2);
        assert!(output.sync_errors.is_empty());
        assert!(calendars.stored.lock().unwrap()[0].sync_token.is_empty());
        assert_eq!(output.source, "window");
    }

    #[test]
    fn never_synced_calendar_is_watched_then_window_fetched_with_string_expiration() {
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

        // First-paint window still ran and populated the cache; token not published.
        assert_eq!(events.upserted_batch.lock().unwrap().len(), 2);
        assert_eq!(output.events.len(), 2);
        assert!(output.sync_errors.is_empty());
        assert!(calendars.stored.lock().unwrap()[0].sync_token.is_empty());
        assert_eq!(output.source, "window");
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
        let gets = http.gets.lock().unwrap();
        assert_eq!(gets.len(), 1, "window still runs");
        assert!(gets[0].contains("singleEvents=true"), "{gets:?}");
        assert!(output.sync_errors.is_empty());
        assert_eq!(output.source, "window");
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

    // ──────────────────────────────────────────
    // Leftover watch-channel stop (cron tail)
    // ──────────────────────────────────────────

    #[test]
    fn cron_stops_leftover_channels_on_disabled_living_calendar() {
        // Disabled living calendar, fresh last_success → not replica-due.
        // Leftover channel must still be stopped (callback None is fine).
        let http = FakeHttp::new(vec![("/channels/stop", 200, "{}")]);
        let mut cal = calendar("cal-1", "primary@example.com", false);
        cal.last_synced_at = Some("2023-11-14T22:12:20Z".to_string());
        cal.last_success_at = Some("2023-11-14T22:12:20Z".to_string());
        let calendars = FakeCalendarRepo::with(vec![cal]);
        let events = FakeEventRepo::new();
        let watches =
            FakeWatchChannelRepo::with(vec![watch_channel("cal-1", "2023-11-20T00:00:00Z")]);
        let tokens = FakeTokenRepo::with(vec![fresh_token("u-1", "at-1")]);
        let oauth = oauth_config();

        let report = pollster::block_on(run_fallback_cron(
            &http, &calendars, &events, &watches, &tokens, &oauth, None, NOW_UNIX,
        ));

        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.synced, 0);
        let posts = http.posts.lock().unwrap();
        assert_eq!(posts.len(), 1, "one channels.stop: {posts:?}");
        assert!(posts[0].0.contains("/channels/stop"), "{}", posts[0].0);
        let body: serde_json::Value = serde_json::from_str(&posts[0].1).unwrap();
        assert_eq!(body["id"], "minted-id");
        assert_eq!(body["resourceId"], "resource-1");
        assert_eq!(
            *watches.deleted_by_calendar_id.lock().unwrap(),
            vec!["cal-1".to_string()]
        );
        assert!(watches.stored.lock().unwrap().is_empty(), "row hard-deleted");
        let gets = http.gets.lock().unwrap();
        assert!(
            gets.iter().all(|u| !u.contains("/events")),
            "disabled calendar must not hit events.list: {gets:?}"
        );
    }

    #[test]
    fn cron_stops_leftover_channels_on_soft_deleted_calendar() {
        // Soft-deleted calendars are invisible to get_by_id / list_user_ids —
        // leftover stop needs get_by_id_unfiltered to recover user_id.
        let http = FakeHttp::new(vec![("/channels/stop", 200, "{}")]);
        let mut cal = calendar("cal-1", "primary@example.com", true);
        cal.deleted_at = Some("2023-11-14T20:00:00Z".to_string());
        let calendars = FakeCalendarRepo::with(vec![cal]);
        // Sanity: living-only read hides the row.
        assert!(
            pollster::block_on(calendars.get_by_id("cal-1"))
                .unwrap()
                .is_none()
        );
        assert!(
            pollster::block_on(calendars.get_by_id_unfiltered("cal-1"))
                .unwrap()
                .is_some()
        );
        let events = FakeEventRepo::new();
        let watches =
            FakeWatchChannelRepo::with(vec![watch_channel("cal-1", "2023-11-20T00:00:00Z")]);
        let tokens = FakeTokenRepo::with(vec![fresh_token("u-1", "at-1")]);
        let oauth = oauth_config();

        let report = pollster::block_on(run_fallback_cron(
            &http, &calendars, &events, &watches, &tokens, &oauth, None, NOW_UNIX,
        ));

        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let posts = http.posts.lock().unwrap();
        assert_eq!(posts.len(), 1, "one channels.stop: {posts:?}");
        assert!(posts[0].0.contains("/channels/stop"), "{}", posts[0].0);
        assert!(watches.stored.lock().unwrap().is_empty(), "row hard-deleted");
        assert_eq!(
            *watches.deleted_by_calendar_id.lock().unwrap(),
            vec!["cal-1".to_string()]
        );
    }

    #[test]
    fn cron_failed_leftover_stop_leaves_row_for_next_tick() {
        // Disabled leftover fails stop → row remains; sibling still progresses.
        let http = FakeHttp::new(vec![
            ("/channels/stop", 500, r#"{"error":"boom"}"#),
            ("/events", 200, EVENTS_JSON),
        ]);
        let mut disabled = calendar("cal-disabled", "disabled@example.com", false);
        disabled.last_synced_at = Some("2023-11-14T22:12:20Z".to_string());
        disabled.last_success_at = Some("2023-11-14T22:12:20Z".to_string());
        // Stale living calendar must still sync despite leftover stop failure.
        let mut living = calendar("cal-live", "primary@example.com", true);
        living.last_synced_at = Some("2023-11-14T21:53:20Z".to_string());
        living.last_success_at = Some("2023-11-14T21:53:20Z".to_string());
        let calendars = FakeCalendarRepo::with(vec![disabled, living]);
        let events = FakeEventRepo::new();
        let watches = FakeWatchChannelRepo::with(vec![watch_channel(
            "cal-disabled",
            "2023-11-20T00:00:00Z",
        )]);
        let tokens = FakeTokenRepo::with(vec![fresh_token("u-1", "at-1")]);
        let oauth = oauth_config();

        let report = pollster::block_on(run_fallback_cron(
            &http, &calendars, &events, &watches, &tokens, &oauth, None, NOW_UNIX,
        ));

        assert_eq!(report.synced, 1, "living calendar still synced");
        assert_eq!(
            report.published,
            vec![("u-1".to_string(), "cal-live".to_string())]
        );
        assert!(
            report.errors.iter().any(|e| e.contains("cal-disabled")),
            "error mentions leftover calendar: {:?}",
            report.errors
        );
        assert_eq!(
            watches.stored.lock().unwrap().len(),
            1,
            "channel row left for retry"
        );
        assert!(watches.deleted_by_calendar_id.lock().unwrap().is_empty());
        let posts = http.posts.lock().unwrap();
        assert!(
            posts.iter().any(|(u, _)| u.contains("/channels/stop")),
            "stop was attempted: {posts:?}"
        );
    }

    #[test]
    fn cron_does_not_stop_channels_on_living_sync_enabled_calendar() {
        // Enabled + fresh + covered horizon → no leftover stop, no renew.
        let http = FakeHttp::new(vec![]);
        let mut cal = calendar("cal-1", "primary@example.com", true);
        cal.last_synced_at = Some("2023-11-14T22:12:20Z".to_string());
        cal.last_success_at = Some("2023-11-14T22:12:20Z".to_string());
        let calendars = FakeCalendarRepo::with(vec![cal]);
        let events = FakeEventRepo::new();
        // Far future: spans renew horizon (now + 24h).
        let watches =
            FakeWatchChannelRepo::with(vec![watch_channel("cal-1", "2023-11-20T00:00:00Z")]);
        let tokens = FakeTokenRepo::with(vec![fresh_token("u-1", "at-1")]);
        let oauth = oauth_config();

        let report = pollster::block_on(run_fallback_cron(
            &http,
            &calendars,
            &events,
            &watches,
            &tokens,
            &oauth,
            Some(CALLBACK_URL),
            NOW_UNIX,
        ));

        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.synced, 0);
        assert_eq!(report.renewed, 0);
        assert!(http.posts.lock().unwrap().is_empty(), "no stop/watch POST");
        assert_eq!(watches.stored.lock().unwrap().len(), 1, "channel untouched");
        assert!(watches.deleted_by_calendar_id.lock().unwrap().is_empty());
        assert!(watches.deleted_by_id.lock().unwrap().is_empty());
    }

    #[test]
    fn cron_syncs_stale_and_skips_fresh() {
        let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
        let mut stale = calendar("cal-a", "primary@example.com", true);
        // Freshness is last_success_at (ADR 0005), not last_synced_at.
        stale.last_synced_at = Some("2023-11-14T21:53:20Z".to_string()); // 20 min ago
        stale.last_success_at = Some("2023-11-14T21:53:20Z".to_string());
        let mut fresh = calendar("cal-b", "secondary@example.com", true);
        fresh.last_synced_at = Some("2023-11-14T22:12:20Z".to_string()); // 1 min ago
        fresh.last_success_at = Some("2023-11-14T22:12:20Z".to_string());
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
        assert_eq!(
            report.published,
            vec![("u-1".to_string(), "cal-a".to_string())]
        );
        let gets = http.gets.lock().unwrap();
        let event_gets: Vec<_> = gets.iter().filter(|u| u.contains("/events")).collect();
        let list_gets: Vec<_> = gets.iter().filter(|u| u.contains("calendarList")).collect();
        assert_eq!(event_gets.len(), 1, "fresh calendar must not hit events.list: {gets:?}");
        assert!(
            event_gets[0].contains("primary%40example.com/events"),
            "{:?}",
            gets
        );
        assert_eq!(list_gets.len(), 1, "one incremental calendarList GET: {gets:?}");
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
        let gets = http.gets.lock().unwrap();
        assert_eq!(
            gets.iter().filter(|u| u.contains("/events")).count(),
            1,
            "{gets:?}"
        );
        assert_eq!(
            gets.iter().filter(|u| u.contains("calendarList")).count(),
            1,
            "{gets:?}"
        );
        assert_eq!(calendars.sync_states.lock().unwrap().len(), 1);
    }

    #[test]
    fn cron_watch_404_disables() {
        let http = FakeHttp::new(vec![("/events/watch", 404, r#"{"error":"not found"}"#)]);
        let mut cal = calendar("cal-1", "primary@example.com", true);
        // Fresh last_success → not replica-due; only renews (and 404-disables).
        cal.last_synced_at = Some("2023-11-14T22:12:20Z".to_string());
        cal.last_success_at = Some("2023-11-14T22:12:20Z".to_string());
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
        assert!(report.published.is_empty());
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("404"), "{}", report.errors[0]);
        assert_eq!(
            *calendars.disabled.lock().unwrap(),
            vec![("cal-1".to_string(), false)]
        );
        let gets = http.gets.lock().unwrap();
        assert!(
            gets.iter().all(|u| !u.contains("/events")),
            "fresh calendar was not synced: {gets:?}"
        );
        assert_eq!(
            gets.iter().filter(|u| u.contains("calendarList")).count(),
            1,
            "incremental list still runs: {gets:?}"
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
        cal_a.last_success_at = Some("2023-11-14T21:53:20Z".to_string());
        let mut cal_b = calendar_for_user("u-b", "cal-b", "secondary@example.com", true);
        cal_b.last_synced_at = Some("2023-11-14T21:53:20Z".to_string()); // stale
        cal_b.last_success_at = Some("2023-11-14T21:53:20Z".to_string());
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
        assert_eq!(
            report.published,
            vec![("u-b".to_string(), "cal-b".to_string())]
        );
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("u-a"), "{}", report.errors[0]);
        // NoToken must not flip healthy calendars to authorization_required.
        let stored = calendars.stored.lock().unwrap();
        let a = stored.iter().find(|c| c.id == "cal-a").unwrap();
        assert_ne!(a.sync_status, "authorization_required");
        let gets = http.gets.lock().unwrap();
        let event_gets: Vec<_> = gets.iter().filter(|u| u.contains("/events")).collect();
        assert_eq!(event_gets.len(), 1, "{gets:?}");
        assert!(
            event_gets[0].contains("secondary%40example.com"),
            "{:?}",
            gets
        );
        // Only u-b got a token → only u-b runs calendarList.
        assert_eq!(
            gets.iter().filter(|u| u.contains("calendarList")).count(),
            1,
            "{gets:?}"
        );
    }

    // ──────────────────────────────────────────
    // replica_due / dirty-generation cron
    // ──────────────────────────────────────────

    #[test]
    fn replica_due_matrix() {
        let now = NOW_UNIX;
        let one_min_ago = unix_secs_to_rfc3339(now - 60);
        let twenty_min_ago = unix_secs_to_rfc3339(now - 20 * 60);
        let future_retry = unix_secs_to_rfc3339(now + 600);
        let past_retry = unix_secs_to_rfc3339(now - 60);

        // dirty + last_success 1 minute ago → due
        let mut cal = calendar("c", "primary@example.com", true);
        cal.dirty_requested_generation = 1;
        cal.dirty_applied_generation = 0;
        cal.last_success_at = Some(one_min_ago.clone());
        assert!(replica_due(&cal, now), "dirty should be due");

        // clean + last_success 1 minute ago → not due
        cal.dirty_requested_generation = 0;
        cal.dirty_applied_generation = 0;
        cal.last_success_at = Some(one_min_ago.clone());
        assert!(!replica_due(&cal, now), "fresh clean should not be due");

        // clean + last_success 20 minutes ago → due
        cal.last_success_at = Some(twenty_min_ago.clone());
        assert!(replica_due(&cal, now), "15m backstop");

        // clean + last_success missing → due
        cal.last_success_at = None;
        assert!(replica_due(&cal, now), "never-succeeded is due");

        // authorization_required + dirty → not due
        cal.dirty_requested_generation = 5;
        cal.dirty_applied_generation = 0;
        cal.last_success_at = Some(twenty_min_ago.clone());
        cal.sync_status = "authorization_required".to_string();
        assert!(!replica_due(&cal, now), "auth_required must not hot-loop");

        // next_retry_at in the future + last_success 20m ago → not due (backoff wins)
        cal.sync_status = String::new();
        cal.dirty_requested_generation = 0;
        cal.dirty_applied_generation = 0;
        cal.last_success_at = Some(twenty_min_ago.clone());
        cal.next_retry_at = Some(future_retry);
        assert!(!replica_due(&cal, now), "future backoff blocks even stale");

        // next_retry_at in the past + last_success 1m ago + not dirty → due
        cal.next_retry_at = Some(past_retry);
        cal.last_success_at = Some(one_min_ago.clone());
        assert!(replica_due(&cal, now), "expired backoff retries");

        // full_sync_requested + last_success 1m ago + not dirty → due
        cal.next_retry_at = None;
        cal.full_sync_requested = true;
        cal.last_success_at = Some(one_min_ago);
        assert!(replica_due(&cal, now), "full_sync_requested forces due");

        // freeBusyReader + dirty + last_success 20m ago → not due
        cal.full_sync_requested = false;
        cal.access_role = "freeBusyReader".to_string();
        cal.dirty_requested_generation = 3;
        cal.dirty_applied_generation = 0;
        cal.last_success_at = Some(twenty_min_ago);
        cal.next_retry_at = None;
        assert!(
            !replica_due(&cal, now),
            "freeBusyReader must never start a replica walk"
        );
    }

    #[test]
    fn cron_picks_dirty_even_when_last_success_is_fresh() {
        let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
        let mut dirty = calendar("cal-a", "primary@example.com", true);
        dirty.last_success_at = Some("2023-11-14T22:12:20Z".to_string()); // 1 min ago
        dirty.last_synced_at = dirty.last_success_at.clone();
        dirty.dirty_requested_generation = 1;
        dirty.dirty_applied_generation = 0;
        let mut clean = calendar("cal-b", "secondary@example.com", true);
        clean.last_success_at = Some("2023-11-14T22:12:20Z".to_string());
        clean.last_synced_at = clean.last_success_at.clone();
        clean.dirty_requested_generation = 0;
        clean.dirty_applied_generation = 0;
        let calendars = FakeCalendarRepo::with(vec![dirty, clean]);
        let events = FakeEventRepo::new();
        let watches = FakeWatchChannelRepo::new();
        let tokens = FakeTokenRepo::with(vec![fresh_token("u-1", "at-1")]);
        let oauth = oauth_config();

        let report = pollster::block_on(run_fallback_cron(
            &http, &calendars, &events, &watches, &tokens, &oauth, None, NOW_UNIX,
        ));

        assert_eq!(report.synced, 1);
        assert_eq!(
            report.published,
            vec![("u-1".to_string(), "cal-a".to_string())]
        );
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let gets = http.gets.lock().unwrap();
        let event_gets: Vec<_> = gets.iter().filter(|u| u.contains("/events")).collect();
        assert_eq!(event_gets.len(), 1, "{gets:?}");
        assert!(
            event_gets[0].contains("primary%40example.com/events"),
            "{:?}",
            gets
        );
        assert_eq!(
            gets.iter().filter(|u| u.contains("calendarList")).count(),
            1,
            "{gets:?}"
        );
        let stored = calendars.stored.lock().unwrap();
        let a = stored.iter().find(|c| c.id == "cal-a").unwrap();
        assert_eq!(a.dirty_applied_generation, 1);
        let b = stored.iter().find(|c| c.id == "cal-b").unwrap();
        assert_eq!(b.dirty_applied_generation, 0);
    }

    #[test]
    fn cron_success_sets_applied_to_generation_at_start_mid_run_dirty_remains() {
        let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
        let mut cal = calendar("cal-1", "primary@example.com", true);
        cal.last_success_at = Some("2023-11-14T22:12:20Z".to_string()); // fresh
        cal.last_synced_at = cal.last_success_at.clone();
        cal.dirty_requested_generation = 5;
        cal.dirty_applied_generation = 1;
        let calendars = FakeCalendarRepo::with(vec![cal]);
        // After the under-lease snapshot get_by_id (#1), bump requested 5 → 6.
        *calendars.bump_dirty_after_get_by_id.lock().unwrap() = Some(1);
        let events = FakeEventRepo::new();
        let watches = FakeWatchChannelRepo::new();
        let tokens = FakeTokenRepo::with(vec![fresh_token("u-1", "at-1")]);
        let oauth = oauth_config();

        let report = pollster::block_on(run_fallback_cron(
            &http, &calendars, &events, &watches, &tokens, &oauth, None, NOW_UNIX,
        ));

        assert_eq!(report.synced, 1);
        assert_eq!(
            report.published,
            vec![("u-1".to_string(), "cal-1".to_string())]
        );
        let stored = calendars.stored.lock().unwrap();
        assert_eq!(stored[0].dirty_applied_generation, 5, "snapshot at start");
        assert_eq!(
            stored[0].dirty_requested_generation, 6,
            "mid-run bump preserved"
        );
        assert!(
            replica_due(&stored[0], NOW_UNIX),
            "still dirty after mid-run bump"
        );
    }

    #[test]
    fn cron_failure_leaves_dirty_other_calendars_progress_and_lease_busy_not_published() {
        // cal-a: dirty, events.list 500 → failure, applied stays 0
        // cal-b: dirty, events.list 200 → published
        // cal-c: dirty, foreign unexpired lease → LeaseBusy, not published
        let http = FakeHttp::new(vec![
            ("primary%40example.com/events", 500, r#"{"error":"boom"}"#),
            ("secondary%40example.com/events", 200, EVENTS_JSON),
            // tertiary would 200 if reached — lease busy must not fetch.
            ("tertiary%40example.com/events", 200, EVENTS_JSON),
        ]);
        let mut cal_a = calendar("cal-a", "primary@example.com", true);
        cal_a.last_success_at = Some("2023-11-14T22:12:20Z".to_string());
        cal_a.dirty_requested_generation = 2;
        cal_a.dirty_applied_generation = 0;
        let mut cal_b = calendar("cal-b", "secondary@example.com", true);
        cal_b.last_success_at = Some("2023-11-14T22:12:20Z".to_string());
        cal_b.dirty_requested_generation = 1;
        cal_b.dirty_applied_generation = 0;
        let mut cal_c = calendar("cal-c", "tertiary@example.com", true);
        cal_c.last_success_at = Some("2023-11-14T22:12:20Z".to_string());
        cal_c.dirty_requested_generation = 3;
        cal_c.dirty_applied_generation = 0;
        let calendars = FakeCalendarRepo::with(vec![cal_a, cal_b, cal_c]);
        calendars.force_lease("cal-c", "other-owner", Some("2099-01-01T00:00:00Z"));
        let events = FakeEventRepo::new();
        let watches = FakeWatchChannelRepo::new();
        let tokens = FakeTokenRepo::with(vec![fresh_token("u-1", "at-1")]);
        let oauth = oauth_config();

        let report = pollster::block_on(run_fallback_cron(
            &http, &calendars, &events, &watches, &tokens, &oauth, None, NOW_UNIX,
        ));

        assert_eq!(report.synced, 1);
        assert_eq!(
            report.published,
            vec![("u-1".to_string(), "cal-b".to_string())]
        );
        assert_eq!(report.errors.len(), 1);
        assert!(
            report.errors[0].contains("cal-a"),
            "{:?}",
            report.errors
        );

        let stored = calendars.stored.lock().unwrap();
        let a = stored.iter().find(|c| c.id == "cal-a").unwrap();
        assert_eq!(a.dirty_applied_generation, 0);
        assert_eq!(a.dirty_requested_generation, 2);
        let b = stored.iter().find(|c| c.id == "cal-b").unwrap();
        assert_eq!(b.dirty_applied_generation, 1);
        let c = stored.iter().find(|c| c.id == "cal-c").unwrap();
        assert_eq!(c.dirty_applied_generation, 0);
        assert_eq!(c.dirty_requested_generation, 3);

        let gets = http.gets.lock().unwrap();
        assert!(
            !gets.iter().any(|u| u.contains("tertiary")),
            "LeaseBusy must not fetch: {:?}",
            gets
        );
    }

    // ──────────────────────────────────────────
    // refresh_calendar_list (incremental)
    // ──────────────────────────────────────────

    #[test]
    fn calendar_list_incremental_new_calendar_appears() {
        let now = unix_secs_to_rfc3339(NOW_UNIX);
        let body = r#"{
            "items": [
                {"id": "new@example.com", "summary": "New", "timeZone": "UTC", "accessRole": "owner"}
            ],
            "nextSyncToken": "tok-2"
        }"#;
        let http = FakeHttp::new(vec![("calendarList", 200, body)]);
        let calendars =
            FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        // with() seeds "test-list-token"; set the token the fixture expects.
        pollster::block_on(calendars.set_calendar_list_sync_token("u-1", "tok-1", &now)).unwrap();
        let watches = FakeWatchChannelRepo::new();

        let new_ids = pollster::block_on(refresh_calendar_list(
            &http,
            &calendars,
            Some(&watches),
            &access(),
            "u-1",
            &now,
        ))
        .unwrap();

        assert_eq!(new_ids.len(), 1, "one newly inserted local id");
        let stored = calendars.stored.lock().unwrap();
        let primary = stored
            .iter()
            .find(|c| c.google_calendar_id == "primary@example.com")
            .unwrap();
        assert!(primary.sync_enabled);
        assert!(primary.deleted_at.is_none());
        let newbie = stored
            .iter()
            .find(|c| c.google_calendar_id == "new@example.com")
            .unwrap();
        assert!(newbie.sync_enabled);
        assert!(newbie.deleted_at.is_none());
        assert_eq!(newbie.id, new_ids[0]);
        drop(stored);
        let token = pollster::block_on(calendars.get_calendar_list_sync_token("u-1"))
            .unwrap()
            .unwrap();
        assert_eq!(token, "tok-2");
        let gets = http.gets.lock().unwrap();
        assert!(gets[0].contains("syncToken=tok-1"), "{gets:?}");
        assert!(gets[0].contains("showDeleted=true"), "{gets:?}");
    }

    #[test]
    fn calendar_list_incremental_deleted_disables_and_stops_watches() {
        let now = unix_secs_to_rfc3339(NOW_UNIX);
        let body = r#"{
            "items": [
                {"id": "primary@example.com", "deleted": true}
            ],
            "nextSyncToken": "tok-del"
        }"#;
        let http = FakeHttp::new(vec![
            ("calendarList", 200, body),
            ("/channels/stop", 200, "{}"),
        ]);
        let calendars =
            FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        pollster::block_on(calendars.set_calendar_list_sync_token("u-1", "tok-1", &now)).unwrap();
        // Preload an event so soft-delete of the calendar does not wipe events.
        let events = FakeEventRepo::new();
        events
            .stored
            .lock()
            .unwrap()
            .push(seeded_event("ev-1", "cal-1", "g-1", ""));
        let watches =
            FakeWatchChannelRepo::with(vec![watch_channel("cal-1", "2023-11-21T22:13:20Z")]);

        let dirty_before = calendars.stored.lock().unwrap()[0].dirty_requested_generation;

        pollster::block_on(refresh_calendar_list(
            &http,
            &calendars,
            Some(&watches),
            &access(),
            "u-1",
            &now,
        ))
        .unwrap();

        let stored = calendars.stored.lock().unwrap();
        assert_eq!(stored[0].sync_enabled, false);
        assert!(stored[0].deleted_at.is_some());
        assert_eq!(
            stored[0].dirty_requested_generation, dirty_before,
            "deleted list entry must not bump dirty"
        );
        drop(stored);
        assert_eq!(
            *watches.deleted_by_calendar_id.lock().unwrap(),
            vec!["cal-1".to_string()]
        );
        let posts = http.posts.lock().unwrap();
        assert!(
            posts.iter().any(|(u, _)| u.contains("/channels/stop")),
            "watch stop POST expected: {posts:?}"
        );
        // Events repo not wiped.
        let evs = events.stored.lock().unwrap();
        assert_eq!(evs.len(), 1);
        assert!(evs[0].deleted_at.is_none());
    }

    #[test]
    fn calendar_list_incremental_absence_does_not_delete() {
        let now = unix_secs_to_rfc3339(NOW_UNIX);
        let body = r#"{"items":[],"nextSyncToken":"tok-empty"}"#;
        let http = FakeHttp::new(vec![("calendarList", 200, body)]);
        let calendars =
            FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        pollster::block_on(calendars.set_calendar_list_sync_token("u-1", "tok-1", &now)).unwrap();
        let watches = FakeWatchChannelRepo::new();

        pollster::block_on(refresh_calendar_list(
            &http,
            &calendars,
            Some(&watches),
            &access(),
            "u-1",
            &now,
        ))
        .unwrap();

        let stored = calendars.stored.lock().unwrap();
        assert_eq!(stored.len(), 1);
        assert!(stored[0].sync_enabled);
        assert!(stored[0].deleted_at.is_none());
        drop(stored);
        let token = pollster::block_on(calendars.get_calendar_list_sync_token("u-1"))
            .unwrap()
            .unwrap();
        assert_eq!(token, "tok-empty");
    }

    #[test]
    fn calendar_list_full_absence_orphans() {
        let now = unix_secs_to_rfc3339(NOW_UNIX);
        let body = r#"{
            "items": [
                {"id": "b@example.com", "summary": "B", "accessRole": "owner"}
            ],
            "nextSyncToken": "tok-full"
        }"#;
        let http = FakeHttp::new(vec![
            ("calendarList", 200, body),
            ("/channels/stop", 200, "{}"),
        ]);
        let calendars = FakeCalendarRepo::with(vec![
            calendar("cal-a", "a@example.com", true),
            calendar("cal-b", "b@example.com", true),
        ]);
        // Clear the seeded list token so this walk is full (orphan path).
        pollster::block_on(calendars.set_calendar_list_sync_token("u-1", "", &now)).unwrap();
        let watches =
            FakeWatchChannelRepo::with(vec![watch_channel("cal-a", "2023-11-21T22:13:20Z")]);

        pollster::block_on(refresh_calendar_list(
            &http,
            &calendars,
            Some(&watches),
            &access(),
            "u-1",
            &now,
        ))
        .unwrap();

        let stored = calendars.stored.lock().unwrap();
        let a = stored.iter().find(|c| c.id == "cal-a").unwrap();
        assert!(!a.sync_enabled);
        assert!(a.deleted_at.is_some());
        let b = stored.iter().find(|c| c.id == "cal-b").unwrap();
        assert!(b.sync_enabled);
        assert!(b.deleted_at.is_none());
        drop(stored);
        assert_eq!(
            *watches.deleted_by_calendar_id.lock().unwrap(),
            vec!["cal-a".to_string()]
        );
        let token = pollster::block_on(calendars.get_calendar_list_sync_token("u-1"))
            .unwrap()
            .unwrap();
        assert_eq!(token, "tok-full");
        // Full list URL must not carry syncToken.
        let gets = http.gets.lock().unwrap();
        assert!(
            gets.iter().any(|u| u.contains("calendarList") && !u.contains("syncToken=")),
            "{gets:?}"
        );
    }

    #[test]
    fn calendar_list_410_merge_full_does_not_wipe_on_mid_failure() {
        let now = unix_secs_to_rfc3339(NOW_UNIX);
        // First GET is incremental (has syncToken) → 410.
        // Second GET is full (no syncToken) → 500.
        let http = FakeHttp::new(vec![
            ("syncToken=", 410, r#"{"error":"gone"}"#),
            ("calendarList", 500, r#"{"error":"boom"}"#),
        ]);
        let calendars =
            FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        pollster::block_on(calendars.set_calendar_list_sync_token("u-1", "old", &now)).unwrap();
        let watches = FakeWatchChannelRepo::new();

        let err = pollster::block_on(refresh_calendar_list(
            &http,
            &calendars,
            Some(&watches),
            &access(),
            "u-1",
            &now,
        ))
        .unwrap_err();
        assert!(err.to_string().contains("500") || err.to_string().contains("calendarList"), "{err}");

        let stored = calendars.stored.lock().unwrap();
        assert!(stored[0].sync_enabled);
        assert!(stored[0].deleted_at.is_none());
        drop(stored);
        // Cursor cleared on 410 start — never a fabricated new success token.
        let token = pollster::block_on(calendars.get_calendar_list_sync_token("u-1"))
            .unwrap()
            .unwrap_or_default();
        assert_eq!(token, "", "must not persist a new success token after failed walk");
    }

    #[test]
    fn calendar_list_metadata_upsert_does_not_re_enable_disabled() {
        let now = unix_secs_to_rfc3339(NOW_UNIX);
        let body = r#"{
            "items": [
                {"id": "primary@example.com", "summary": "Renamed", "accessRole": "writer", "timeZone": "Asia/Colombo"}
            ],
            "nextSyncToken": "tok-meta"
        }"#;
        let http = FakeHttp::new(vec![("calendarList", 200, body)]);
        let mut cal = calendar("cal-1", "primary@example.com", false);
        cal.summary = "Old".to_string();
        cal.access_role = "owner".to_string();
        let calendars = FakeCalendarRepo::with(vec![cal]);
        // Full walk also exercises the same upsert contract.
        pollster::block_on(calendars.set_calendar_list_sync_token("u-1", "", &now)).unwrap();
        let watches = FakeWatchChannelRepo::new();

        pollster::block_on(refresh_calendar_list(
            &http,
            &calendars,
            Some(&watches),
            &access(),
            "u-1",
            &now,
        ))
        .unwrap();

        let stored = calendars.stored.lock().unwrap();
        assert_eq!(stored.len(), 1);
        assert!(
            !stored[0].sync_enabled,
            "deliberate disable must survive metadata upsert"
        );
        assert_eq!(stored[0].summary, "Renamed");
        assert_eq!(stored[0].access_role, "writer");
        assert_eq!(stored[0].time_zone, "Asia/Colombo");
        assert!(stored[0].deleted_at.is_none());
    }

    #[test]
    fn cron_publishes_newly_appeared_calendar_from_list_refresh() {
        let now = unix_secs_to_rfc3339(NOW_UNIX);
        let list_body = r#"{
            "items": [
                {"id": "new@example.com", "summary": "New", "accessRole": "owner"}
            ],
            "nextSyncToken": "tok-cron"
        }"#;
        // New calendar is never_initialized → replica_due; script events.list.
        let http = FakeHttp::new(vec![
            ("calendarList", 200, list_body),
            ("/events", 200, EVENTS_JSON),
        ]);
        let mut existing = calendar("cal-1", "primary@example.com", true);
        // Fresh so only the new calendar is replica-due (never synced).
        existing.last_success_at = Some("2023-11-14T22:12:20Z".to_string());
        existing.last_synced_at = existing.last_success_at.clone();
        let calendars = FakeCalendarRepo::with(vec![existing]);
        pollster::block_on(calendars.set_calendar_list_sync_token("u-1", "tok-1", &now)).unwrap();
        let events = FakeEventRepo::new();
        let watches = FakeWatchChannelRepo::new();
        let tokens = FakeTokenRepo::with(vec![fresh_token("u-1", "at-1")]);
        let oauth = oauth_config();

        let report = pollster::block_on(run_fallback_cron(
            &http, &calendars, &events, &watches, &tokens, &oauth, None, NOW_UNIX,
        ));

        // published includes the new calendar id from list refresh, plus the
        // replica publish for that new calendar (never-synced → due).
        assert!(
            report
                .published
                .iter()
                .any(|(u, _)| u == "u-1"),
            "{:?}",
            report.published
        );
        let new_rows: Vec<_> = calendars
            .stored
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.google_calendar_id == "new@example.com")
            .cloned()
            .collect();
        assert_eq!(new_rows.len(), 1);
        assert!(new_rows[0].sync_enabled);
        assert!(
            report
                .published
                .iter()
                .any(|(_, id)| id == &new_rows[0].id),
            "new calendar id must be in published: {:?}",
            report.published
        );
        assert!(report.errors.is_empty(), "{:?}", report.errors);
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
    fn exists_state_enqueues_dirty_for_the_channel_calendar() {
        let channel = webhook_channel("cal-1");
        assert_eq!(
            decide_webhook(
                "exists",
                Some(&channel),
                Some("tok-1"),
                Some(&calendar("cal-1", "primary@example.com", true)),
            ),
            WebhookDecision::EnqueueDirty {
                calendar_id: "cal-1".to_string(),
            }
        );
    }

    #[test]
    fn not_exists_state_marks_calendar_gone() {
        let channel = webhook_channel("cal-1");
        assert_eq!(
            decide_webhook(
                "not_exists",
                Some(&channel),
                Some("tok-1"),
                Some(&calendar("cal-1", "primary@example.com", true)),
            ),
            WebhookDecision::CalendarGone {
                calendar_id: "cal-1".to_string(),
            }
        );
    }

    #[test]
    fn exists_persist_bumps_dirty_and_returns_dirty_accepted() {
        let cal = calendar("cal-1", "primary@example.com", true);
        let calendars = FakeCalendarRepo::with(vec![cal]);
        let channel = webhook_channel("cal-1");
        let decision = decide_webhook(
            "exists",
            Some(&channel),
            Some("tok-1"),
            Some(&calendars.stored.lock().unwrap()[0].clone()),
        );
        assert_eq!(
            decision,
            WebhookDecision::EnqueueDirty {
                calendar_id: "cal-1".to_string(),
            }
        );

        let now = unix_secs_to_rfc3339(NOW_UNIX);
        let result = pollster::block_on(persist_webhook_decision(
            &calendars,
            &decision,
            &now,
        ));
        assert_eq!(
            result,
            WebhookPersistResult::DirtyAccepted {
                calendar_id: "cal-1".to_string(),
            }
        );
        let stored = calendars.stored.lock().unwrap();
        assert_eq!(stored[0].dirty_requested_generation, 1);
        assert!(stored[0].sync_enabled);
    }

    #[test]
    fn missing_dirty_write_is_not_accepted_work() {
        let cal = calendar("cal-1", "primary@example.com", true);
        let calendars = FakeCalendarRepo::with(vec![cal]);
        *calendars.fail_bump_dirty.lock().unwrap() = true;
        let decision = WebhookDecision::EnqueueDirty {
            calendar_id: "cal-1".to_string(),
        };
        let now = unix_secs_to_rfc3339(NOW_UNIX);
        let result = pollster::block_on(persist_webhook_decision(
            &calendars,
            &decision,
            &now,
        ));
        assert_eq!(
            result,
            WebhookPersistResult::DirtyPersistFailed {
                calendar_id: "cal-1".to_string(),
            }
        );
        assert!(!matches!(result, WebhookPersistResult::DirtyAccepted { .. }));
        assert_eq!(
            calendars.stored.lock().unwrap()[0].dirty_requested_generation,
            0
        );
    }

    #[test]
    fn non_actionable_webhook_states_do_not_bump_dirty() {
        let channel = webhook_channel("cal-1");
        let enabled = calendar("cal-1", "primary@example.com", true);
        let disabled = calendar("cal-1", "primary@example.com", false);
        let now = unix_secs_to_rfc3339(NOW_UNIX);

        let cases: Vec<(&str, Option<&WatchChannel>, Option<&str>, Option<&GoogleCalendar>)> = vec![
            ("sync", Some(&channel), Some("tok-1"), Some(&enabled)),
            ("exists", Some(&channel), Some("tok-2"), Some(&enabled)),
            ("exists", None, Some("tok-1"), Some(&enabled)),
            ("exists", Some(&channel), Some("tok-1"), Some(&disabled)),
        ];

        for (state, stored_ch, token, cal) in cases {
            let calendars = FakeCalendarRepo::with(vec![enabled.clone()]);
            let decision = decide_webhook(state, stored_ch, token, cal);
            assert_eq!(
                decision,
                WebhookDecision::Ignore,
                "state={state:?} token={token:?} should Ignore"
            );
            let result = pollster::block_on(persist_webhook_decision(
                &calendars,
                &decision,
                &now,
            ));
            assert_eq!(result, WebhookPersistResult::Ignored);
            assert_eq!(
                calendars.stored.lock().unwrap()[0].dirty_requested_generation,
                0,
                "state={state:?} must not bump dirty"
            );
        }
    }

    #[test]
    fn not_exists_persist_disables_without_dirty_bump() {
        let cal = calendar("cal-1", "primary@example.com", true);
        let calendars = FakeCalendarRepo::with(vec![cal]);
        let channel = webhook_channel("cal-1");
        let decision = decide_webhook(
            "not_exists",
            Some(&channel),
            Some("tok-1"),
            Some(&calendars.stored.lock().unwrap()[0].clone()),
        );
        assert_eq!(
            decision,
            WebhookDecision::CalendarGone {
                calendar_id: "cal-1".to_string(),
            }
        );

        let now = unix_secs_to_rfc3339(NOW_UNIX);
        let result = pollster::block_on(persist_webhook_decision(
            &calendars,
            &decision,
            &now,
        ));
        assert_eq!(
            result,
            WebhookPersistResult::GoneDisabled {
                calendar_id: "cal-1".to_string(),
            }
        );
        let stored = calendars.stored.lock().unwrap();
        assert!(!stored[0].sync_enabled);
        assert_eq!(stored[0].dirty_requested_generation, 0);
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
            routine_id: None,
            occurrence_id: None,
            color_hex: None,
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
        assert!(!output.event.id.is_empty(), "persisted id from upsert");
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
        assert!(
            body.get("eventLabelId").is_none(),
            "hand-created events carry no eventLabelId"
        );
        assert!(
            !url.contains("eventLabelVersion"),
            "hand-created events do not require eventLabelVersion: {url}"
        );

        // Cache upsert happened with the mapped row.
        let (google_id, upserted) = events.upserted_single.lock().unwrap().clone().unwrap();
        assert_eq!(google_id, "google-evt-created");
        assert_eq!(upserted.calendar_id, "cal-1");
    }

    #[test]
    fn create_with_color_hex_sends_event_label_id() {
        // `#535050` snaps chroma-first to graphite `#616161`; the cache maps
        // graphite to a stable fake id.
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events",
            200,
            CREATED_JSON,
        )]);
        let calendars = FakeCalendarRepo::with(vec![GoogleCalendar {
            event_labels: r##"[{"id":"label-graphite","backgroundColor":"#616161"}]"##.to_string(),
            ..calendar("cal-1", "primary@example.com", true)
        }]);
        let events = FakeEventRepo::new();
        let mut input = input();
        input.color_hex = Some("#535050".to_string());

        pollster::block_on(create_event(&http, &calendars, &events, &access(), &input, NOW_UNIX))
            .unwrap();

        let (url, body) = http.posts.lock().unwrap().first().unwrap().clone();
        assert!(url.contains("eventLabelVersion=1"), "{url}");
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["eventLabelId"], "label-graphite");
        assert!(
            body.get("colorId").is_none(),
            "create_event never sends colorId: {body}"
        );
    }

    #[test]
    fn create_with_blank_color_hex_omits_color_keys() {
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();
        for blank in ["", "   ", "\t"] {
            let http = FakeHttp::new(vec![(
                "/calendars/primary%40example.com/events",
                200,
                CREATED_JSON,
            )]);
            let mut input = input();
            input.color_hex = Some(blank.to_string());

            pollster::block_on(create_event(
                &http, &calendars, &events, &access(), &input, NOW_UNIX,
            ))
            .unwrap();

            let (url, body) = http.posts.lock().unwrap().first().unwrap().clone();
            let body: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert!(
                body.get("colorId").is_none() && body.get("eventLabelId").is_none(),
                "blank color_hex {blank:?} must omit both color keys"
            );
            assert!(
                !url.contains("eventLabelVersion"),
                "blank color_hex {blank:?} must not require eventLabelVersion: {url}"
            );
        }
    }

    #[test]
    fn create_with_color_hex_and_empty_label_cache_is_invalid() {
        // Empty string = cache miss. The start must fail with a 400-shaped
        // Invalid — no `calendars.get`, no POST.
        let calendars = FakeCalendarRepo::with(vec![GoogleCalendar {
            event_labels: String::new(),
            ..calendar("cal-1", "primary@example.com", true)
        }]);
        let events = FakeEventRepo::new();
        let http = FakeHttp::new(vec![]);
        let mut input = input();
        input.color_hex = Some("#4285f4".to_string());

        let err = pollster::block_on(create_event(
            &http, &calendars, &events, &access(), &input, NOW_UNIX,
        ))
        .unwrap_err();
        assert!(
            matches!(&err, CalendarError::Invalid(m) if m == "calendar event-label cache is empty"),
            "got {err:?}"
        );
        assert!(http.posts.lock().unwrap().is_empty(), "no Google POST");
    }

    #[test]
    fn create_with_color_hex_and_no_matching_label_is_invalid() {
        // Fetched-but-empty cache (`"[]"` = no labels on the calendar).
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let http = FakeHttp::new(vec![]);
        let mut cobalt_input = input();
        cobalt_input.color_hex = Some("#4285f4".to_string());

        let err = pollster::block_on(create_event(
            &http, &calendars, &events, &access(), &cobalt_input, NOW_UNIX,
        ))
        .unwrap_err();
        assert!(
            matches!(&err, CalendarError::Invalid(m) if m == "no event label matches category color"),
            "got {err:?}"
        );
        assert!(http.posts.lock().unwrap().is_empty(), "no Google POST");

        // A populated cache that lacks the snapped hex — same failure.
        let calendars = FakeCalendarRepo::with(vec![GoogleCalendar {
            event_labels: r##"[{"id":"label-banana","backgroundColor":"#f6bf26"}]"##.to_string(),
            ..calendar("cal-1", "primary@example.com", true)
        }]);
        let events = FakeEventRepo::new();
        let http = FakeHttp::new(vec![]);
        let mut banana_input = input();
        banana_input.color_hex = Some("#4285f4".to_string());

        let err = pollster::block_on(create_event(
            &http, &calendars, &events, &access(), &banana_input, NOW_UNIX,
        ))
        .unwrap_err();
        assert!(
            matches!(&err, CalendarError::Invalid(m) if m == "no event label matches category color"),
            "got {err:?}"
        );
        assert!(http.posts.lock().unwrap().is_empty(), "no Google POST");
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

    // ──────────────────────────────────────────
    // patch_event_fields
    // ──────────────────────────────────────────

    #[test]
    fn patch_fields_start_only_payload() {
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events/google-evt-created",
            200,
            PATCHED_JSON,
        )]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();

        pollster::block_on(patch_event_fields(
            &http,
            &calendars,
            &events,
            &access(),
            "cal-1",
            "google-evt-created",
            &PatchEventFields {
                start: Some("2026-08-19T09:00:00Z".to_string()),
                end: None,
                summary: None,
            },
            NOW_UNIX,
        ))
        .unwrap();

        let patches = http.patches.lock().unwrap();
        assert_eq!(patches.len(), 1);
        let body: serde_json::Value = serde_json::from_str(&patches[0].1).unwrap();
        assert_eq!(body["start"]["dateTime"], "2026-08-19T09:00:00Z");
        assert!(body.get("end").is_none(), "{body}");
        assert!(body.get("summary").is_none(), "{body}");
    }

    #[test]
    fn patch_fields_start_end_summary_payload() {
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events/google-evt-created",
            200,
            PATCHED_JSON,
        )]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();

        pollster::block_on(patch_event_fields(
            &http,
            &calendars,
            &events,
            &access(),
            "cal-1",
            "google-evt-created",
            &PatchEventFields {
                start: Some("2026-08-19T09:00:00Z".to_string()),
                end: Some("2026-08-19T11:00:00Z".to_string()),
                summary: Some("Renamed".to_string()),
            },
            NOW_UNIX,
        ))
        .unwrap();

        let patches = http.patches.lock().unwrap();
        assert_eq!(patches.len(), 1);
        let body: serde_json::Value = serde_json::from_str(&patches[0].1).unwrap();
        assert_eq!(body["start"]["dateTime"], "2026-08-19T09:00:00Z");
        assert_eq!(body["end"]["dateTime"], "2026-08-19T11:00:00Z");
        assert_eq!(body["summary"], "Renamed");
    }

    #[test]
    fn patch_fields_all_none_is_invalid() {
        let http = FakeHttp::new(vec![]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let err = pollster::block_on(patch_event_fields(
            &http,
            &calendars,
            &events,
            &access(),
            "cal-1",
            "google-evt-created",
            &PatchEventFields::default(),
            NOW_UNIX,
        ))
        .unwrap_err();
        assert!(
            matches!(err, CalendarError::Invalid(ref m) if m.contains("empty patch")),
            "got {err:?}"
        );
        assert!(http.patches.lock().unwrap().is_empty(), "no Google call");
    }

    // ──────────────────────────────────────────
    // delete_event / update_event_for_user
    // ──────────────────────────────────────────

    fn living_event(id: &str, calendar_id: &str, google_event_id: &str) -> CalendarEvent {
        CalendarEvent {
            id: id.to_string(),
            calendar_id: calendar_id.to_string(),
            google_event_id: google_event_id.to_string(),
            google_etag: "e1".to_string(),
            google_updated_at: "2026-08-17T12:00:00Z".to_string(),
            last_synced_at: "2026-08-17T12:00:00Z".to_string(),
            title: "Meeting".to_string(),
            description: String::new(),
            start_time: "2026-08-19T09:00:00Z".to_string(),
            end_time: "2026-08-19T10:00:00Z".to_string(),
            recurrence: String::new(),
            task_id: String::new(),
            ical_uid: String::new(),
            sequence: 0,
            status: String::new(),
            recurring_event_id: String::new(),
            original_start: String::new(),
            start_time_zone: String::new(),
            end_time_zone: String::new(),
            is_all_day: false,
            raw_json: String::new(),
            created_at: "2026-08-17T12:00:00Z".to_string(),
            updated_at: "2026-08-17T12:00:00Z".to_string(),
            deleted_at: None,
        }
    }

    #[test]
    fn delete_event_sends_cancelled_and_soft_deletes_cache() {
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events/g-evt-1",
            200,
            r#"{"id":"g-evt-1","status":"cancelled"}"#,
        )]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();
        events
            .stored
            .lock()
            .unwrap()
            .push(living_event("local-1", "cal-1", "g-evt-1"));

        pollster::block_on(delete_event(
            &http,
            &calendars,
            &events,
            &access(),
            "cal-1",
            "g-evt-1",
            "local-1",
            NOW_UNIX,
        ))
        .unwrap();

        let patches = http.patches.lock().unwrap();
        assert_eq!(patches.len(), 1);
        let body: serde_json::Value = serde_json::from_str(&patches[0].1).unwrap();
        assert_eq!(body["status"], "cancelled");
        assert!(body.get("end").is_none());

        let deleted = events.deleted.lock().unwrap();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].0, "local-1");
    }

    #[test]
    fn delete_event_google_404_still_soft_deletes_locally() {
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events/g-evt-1",
            404,
            r#"{"error":"notFound"}"#,
        )]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();

        pollster::block_on(delete_event(
            &http,
            &calendars,
            &events,
            &access(),
            "cal-1",
            "g-evt-1",
            "local-1",
            NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(events.deleted.lock().unwrap().len(), 1);
    }

    #[test]
    fn update_event_for_user_wrong_owner_is_not_found() {
        let http = FakeHttp::new(vec![]);
        let calendars =
            FakeCalendarRepo::with(vec![calendar_for_user("other-user", "cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();
        events
            .stored
            .lock()
            .unwrap()
            .push(living_event("local-1", "cal-1", "g-evt-1"));

        let err = pollster::block_on(update_event_for_user(
            &http,
            &calendars,
            &events,
            &access(),
            "u-1",
            "local-1",
            &PatchEventFields {
                start: None,
                end: None,
                summary: Some("Nope".to_string()),
            },
            NOW_UNIX,
        ))
        .unwrap_err();
        assert!(matches!(err, CalendarError::NotFound), "got {err:?}");
        assert!(http.patches.lock().unwrap().is_empty(), "no Google call");
    }

    #[test]
    fn delete_event_for_user_wrong_owner_is_not_found() {
        let http = FakeHttp::new(vec![]);
        let calendars =
            FakeCalendarRepo::with(vec![calendar_for_user("other-user", "cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();
        events
            .stored
            .lock()
            .unwrap()
            .push(living_event("local-1", "cal-1", "g-evt-1"));

        let err = pollster::block_on(delete_event_for_user(
            &http,
            &calendars,
            &events,
            &access(),
            "u-1",
            "local-1",
            NOW_UNIX,
        ))
        .unwrap_err();
        assert!(matches!(err, CalendarError::NotFound), "got {err:?}");
        assert!(http.patches.lock().unwrap().is_empty(), "no Google call");
        assert!(events.deleted.lock().unwrap().is_empty());
    }

    #[test]
    fn update_event_for_user_patches_owned_event() {
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events/g-evt-1",
            200,
            PATCHED_JSON,
        )]);
        let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
        let events = FakeEventRepo::new();
        events
            .stored
            .lock()
            .unwrap()
            .push(living_event("local-1", "cal-1", "g-evt-1"));

        let output = pollster::block_on(update_event_for_user(
            &http,
            &calendars,
            &events,
            &access(),
            "u-1",
            "local-1",
            &PatchEventFields {
                start: None,
                end: None,
                summary: Some("Renamed".to_string()),
            },
            NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(output.event.google_event_id, "google-evt-created");
        let body: serde_json::Value =
            serde_json::from_str(&http.patches.lock().unwrap()[0].1).unwrap();
        assert_eq!(body["summary"], "Renamed");
    }
}
