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


pub mod apply;
pub mod diagnostics;
pub mod replica;
pub mod repair;
pub mod repair_action;
pub mod sync;
pub mod window;
pub(crate) mod google;
pub(crate) mod journal;
pub(crate) mod list;
pub(crate) mod write;
pub(crate) mod write_journal;
pub(crate) mod watch;
pub(crate) mod webhook;
pub(crate) mod catalog;
pub(crate) mod cron;
pub(crate) mod labels;

#[cfg(test)]
mod tests;

/// In-memory operation journal for unit tests (tasks/agenda call sites).
#[cfg(test)]
pub(crate) use tests::support::FakeOperationRepo;

use thiserror::Error;

use crate::calendar::sync::EventsSyncEnvelope;
use crate::models::GoogleCalendar;
use crate::oauth::HttpError;
use crate::repo::RepoError;

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
    /// If-Match retries exhausted for this event/operation. The calendar is not disabled.
    #[error("calendar event write conflict")]
    Conflict,
    #[error("invalid google response: {0}")]
    InvalidResponse(String),
    #[error("http request failed: {0}")]
    Http(#[from] HttpError),
    #[error("database error: {0}")]
    Repo(#[from] RepoError),
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


pub use list::{
    list_events, list_events_after_refresh_failure, parse_event_time_range, CalendarListOutput,
};
pub use write::{
    create_event, delete_event, delete_event_for_user, patch_event, patch_event_fields,
    patch_event_summary, update_event_for_user, CreateEventOutput,
};
pub use watch::{
    ensure_watch, is_public_https_callback, renew_watch_if_needed, stop_watches_for_calendar,
};
pub use webhook::{
    decide_webhook, persist_webhook_decision, tokens_match, WebhookDecision, WebhookPersistResult,
};
pub use catalog::{list_calendars, list_calendars_after_refresh_failure};
pub use cron::{
    replica_due, run_fallback_cron, run_fallback_cron_with_clock, sync_calendar,
    sync_calendar_traced, CronReport, SyncCalendarOutcome, SyncCalendarResult,
};
pub use diagnostics::{
    classify_operator_warning, mint_run_id, operator_warning_record, CheckpointResult,
    OperatorWarningLevel, OperatorWarningRecord, OperatorWarningThresholds, ReplicaApplyReport,
    ReplicaWalkDiagnostic, ReplicaWalkMeta, ReplicaWalkPhase, ReplicaWalkTrigger,
};
pub use repair::repair_inflight_operations;
pub use repair_action::{
    request_calendar_repair, CalendarRepairResponse, CalendarRepairStatus,
    CALENDAR_REPAIR_COOLDOWN_SECS,
};
