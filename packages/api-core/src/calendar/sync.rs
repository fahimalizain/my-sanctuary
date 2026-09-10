//! Sanitized calendar replica health for `GET /api/calendar/events`.
//!
//! Builds the `sync` envelope from **already-persisted** `google_calendars`
//! health columns. Never exposes `sync_token`, OAuth credentials, lease
//! secrets, event bodies, or `raw_json`.
//!
//! Classification and backoff helpers are used by `sync_calendar` (ADR 0005);
//! this module also *reads* health for the request envelope.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::CalendarError;
use crate::models::GoogleCalendar;
use crate::time::{rfc3339_to_unix_secs, unix_secs_to_rfc3339};

/// A calendar with no valid success newer than this many seconds is `stale`
/// in the health envelope (~1 hour). Independent of the request-path gate
/// (parseable `last_synced_at` / `initial_sync_complete`).
pub const SYNC_HEALTH_STALE_SECS: i64 = 60 * 60;

/// Product projection name currently materialised by the replica.
/// Health-string name is unchanged; GET range list now includes all-day rows
/// (running-task list still filters `is_all_day = 0`).
pub const REPLICA_PROJECTION: &str = "timed_masters_and_exceptions";

/// Canonical replica query string hashed by [`replica_query_fingerprint`].
///
/// Current product query: `singleEvents=false`, no `timeMin`, no `eventTypes`.
const REPLICA_QUERY_CANONICAL: &str = "singleEvents=false&timeMin=&eventTypes=";

/// Allow clocks a little ahead of `now_unix` before treating a success
/// timestamp as future/invalid.
const FUTURE_SKEW_SECS: i64 = 120;

/// Base backoff delay (seconds) for the first failure streak step.
const BACKOFF_BASE_SECS: i64 = 30;

/// Cap on exponential backoff delay (seconds).
const BACKOFF_CAP_SECS: i64 = 3600;

/// Aggregate health across the user's calendars (enabled only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncAggregateStatus {
    Ready,
    Degraded,
    AuthorizationRequired,
}

impl SyncAggregateStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Degraded => "degraded",
            Self::AuthorizationRequired => "authorization_required",
        }
    }
}

impl fmt::Display for SyncAggregateStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Per-calendar replica state stored in `google_calendars.sync_status` and
/// exposed on the health view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalendarReplicaState {
    NeverInitialized,
    Ready,
    Retrying,
    Rebuilding,
    AuthorizationRequired,
    Disabled,
}

impl CalendarReplicaState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeverInitialized => "never_initialized",
            Self::Ready => "ready",
            Self::Retrying => "retrying",
            Self::Rebuilding => "rebuilding",
            Self::AuthorizationRequired => "authorization_required",
            Self::Disabled => "disabled",
        }
    }

    /// Parse a stored `sync_status` string. Unknown / empty → `None`.
    pub fn from_stored(s: &str) -> Option<Self> {
        match s {
            "never_initialized" => Some(Self::NeverInitialized),
            "ready" => Some(Self::Ready),
            "retrying" => Some(Self::Retrying),
            "rebuilding" => Some(Self::Rebuilding),
            "authorization_required" => Some(Self::AuthorizationRequired),
            "disabled" => Some(Self::Disabled),
            _ => None,
        }
    }
}

impl fmt::Display for CalendarReplicaState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Sanitized failure category. Never a raw Google body or sync token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncErrorCode {
    AuthRevoked,
    RateLimited,
    NotFound,
    Gone,
    StorageTransient,
    MappingPoison,
    MissingSyncToken,
    /// calendarList entry with `accessRole=freeBusyReader` — keep the row,
    /// never run the replica walk (events.list strips details).
    InsufficientAccess,
    GoogleTransient,
    Unknown,
}

impl SyncErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AuthRevoked => "auth_revoked",
            Self::RateLimited => "rate_limited",
            Self::NotFound => "not_found",
            Self::Gone => "gone",
            Self::StorageTransient => "storage_transient",
            Self::MappingPoison => "mapping_poison",
            Self::MissingSyncToken => "missing_sync_token",
            Self::InsufficientAccess => "insufficient_access",
            Self::GoogleTransient => "google_transient",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for SyncErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Top-level `sync` object on `GET /api/calendar/events`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsSyncEnvelope {
    pub status: SyncAggregateStatus,
    pub calendars: Vec<CalendarSyncView>,
}

/// Per-calendar sanitized health row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarSyncView {
    /// Local `google_calendars.id`.
    pub calendar_id: String,
    pub state: CalendarReplicaState,
    pub initial_sync_complete: bool,
    pub last_success_at: Option<String>,
    pub last_attempt_at: Option<String>,
    pub stale: bool,
    /// Category string when non-empty; never a raw Google body / sync token.
    pub error_code: Option<String>,
    pub retry_after_seconds: Option<i64>,
    pub projection: String,
    pub cache_revision: i64,
}

/// SHA-256 hex (64 lowercase chars) of the canonical replica query string
/// `singleEvents=false&timeMin=&eventTypes=`. Deterministic; does not include
/// a live sync token or any per-user secret.
pub fn replica_query_fingerprint() -> String {
    let digest = Sha256::digest(REPLICA_QUERY_CANONICAL.as_bytes());
    hex_encode(&digest)
}

/// Map a calendar service error to a sanitized [`SyncErrorCode`].
///
/// Never returns the raw Google body or a sync token as the code.
pub fn classify_sync_error(err: &CalendarError) -> SyncErrorCode {
    match err {
        CalendarError::GoogleNotFound => SyncErrorCode::NotFound,
        CalendarError::Repo(_) => SyncErrorCode::StorageTransient,
        CalendarError::InvalidResponse(_) => SyncErrorCode::MappingPoison,
        CalendarError::Http(_) => SyncErrorCode::GoogleTransient,
        CalendarError::GoogleApi(msg) => classify_google_api_message(msg),
        CalendarError::InvalidRange(_)
        | CalendarError::Invalid(_)
        | CalendarError::NotFound
        | CalendarError::Conflict => SyncErrorCode::Unknown,
    }
}

fn classify_google_api_message(msg: &str) -> SyncErrorCode {
    if msg.contains("invalid_grant") {
        return SyncErrorCode::AuthRevoked;
    }
    match status_from_returned_message(msg) {
        Some(401) => SyncErrorCode::AuthRevoked,
        Some(410) => SyncErrorCode::Gone,
        Some(429) | Some(403) => SyncErrorCode::RateLimited,
        Some(s) if (500..600).contains(&s) => SyncErrorCode::GoogleTransient,
        _ => SyncErrorCode::Unknown,
    }
}

/// Parse `returned {n}` from messages like `"google events.list returned 410"`.
fn status_from_returned_message(msg: &str) -> Option<u16> {
    const MARKER: &str = "returned ";
    let idx = msg.find(MARKER)?;
    let rest = &msg[idx + MARKER.len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// Replica state to persist after a classified failure.
///
/// Does **not** auto-flip to `rebuilding` and does not reset the sync token.
pub fn replica_state_for_error(code: SyncErrorCode) -> CalendarReplicaState {
    match code {
        SyncErrorCode::AuthRevoked => CalendarReplicaState::AuthorizationRequired,
        _ => CalendarReplicaState::Retrying,
    }
}

/// Next retry instant (unix seconds) with exponential backoff + deterministic
/// ±20% jitter derived from `(now_unix, failure_streak)`.
///
/// `delay = min(30 * 2^min(max(streak - 1, 0), 10), 3600)`, then jittered.
/// Always returns a value strictly greater than `now_unix`.
pub fn next_retry_unix(now_unix: i64, failure_streak: i64) -> i64 {
    let exp = failure_streak.saturating_sub(1).max(0).min(10) as u32;
    let base = BACKOFF_BASE_SECS
        .saturating_mul(1i64 << exp.min(30))
        .min(BACKOFF_CAP_SECS);
    let jittered = apply_deterministic_jitter(base, now_unix, failure_streak);
    now_unix.saturating_add(jittered.max(1))
}

/// RFC 3339 form of [`next_retry_unix`].
pub fn next_retry_rfc3339(now_unix: i64, failure_streak: i64) -> String {
    unix_secs_to_rfc3339(next_retry_unix(now_unix, failure_streak))
}

/// ±20% jitter on `base`, deterministic from `(now_unix, streak)`.
fn apply_deterministic_jitter(base: i64, now_unix: i64, streak: i64) -> i64 {
    let mut hasher = Sha256::new();
    hasher.update(now_unix.to_le_bytes());
    hasher.update(streak.to_le_bytes());
    let digest = hasher.finalize();
    let n = u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]);
    // Map u32 → [-0.2, 0.2].
    let unit = f64::from(n) / f64::from(u32::MAX);
    let ratio = (unit * 0.4) - 0.2;
    let adjusted = (base as f64) * (1.0 + ratio);
    adjusted.round().max(1.0) as i64
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

/// Build the full `sync` envelope for a list of calendars.
pub fn events_sync_envelope(calendars: &[GoogleCalendar], now_unix: i64) -> EventsSyncEnvelope {
    let views: Vec<CalendarSyncView> = calendars
        .iter()
        .map(|cal| calendar_sync_view(cal, now_unix))
        .collect();
    let status = aggregate_sync_status(&views);
    EventsSyncEnvelope {
        status,
        calendars: views,
    }
}

/// Aggregate across **enabled** calendars only (`state != disabled`).
///
/// 1. any `authorization_required` → `authorization_required`
/// 2. else any `retrying` | `rebuilding` | `never_initialized` OR `stale` → `degraded`
/// 3. else `ready`
///
/// Empty list → `ready`.
pub fn aggregate_sync_status(views: &[CalendarSyncView]) -> SyncAggregateStatus {
    let enabled: Vec<&CalendarSyncView> = views
        .iter()
        .filter(|v| v.state != CalendarReplicaState::Disabled)
        .collect();
    if enabled.is_empty() {
        return SyncAggregateStatus::Ready;
    }
    if enabled
        .iter()
        .any(|v| v.state == CalendarReplicaState::AuthorizationRequired)
    {
        return SyncAggregateStatus::AuthorizationRequired;
    }
    if enabled.iter().any(|v| {
        matches!(
            v.state,
            CalendarReplicaState::Retrying
                | CalendarReplicaState::Rebuilding
                | CalendarReplicaState::NeverInitialized
        ) || v.stale
    }) {
        return SyncAggregateStatus::Degraded;
    }
    SyncAggregateStatus::Ready
}

/// Build a sanitized per-calendar health view from a stored row.
pub fn calendar_sync_view(cal: &GoogleCalendar, now_unix: i64) -> CalendarSyncView {
    let last_synced_parseable = cal
        .last_synced_at
        .as_deref()
        .and_then(rfc3339_to_unix_secs)
        .is_some();

    let state = if !cal.sync_enabled {
        CalendarReplicaState::Disabled
    } else if let Some(known) = CalendarReplicaState::from_stored(&cal.sync_status) {
        // Stored status wins when it is a known replica state — but `disabled`
        // in the column while sync is still enabled is treated as the stored
        // value; the `!sync_enabled` branch above already handled true disable.
        known
    } else if cal.initial_sync_complete || last_synced_parseable {
        CalendarReplicaState::Ready
    } else {
        CalendarReplicaState::NeverInitialized
    };

    let success_unix = valid_success_unix(cal.last_success_at.as_deref(), now_unix)
        .or_else(|| valid_success_unix(cal.last_synced_at.as_deref(), now_unix));

    // Prefer the original stored string when valid so we don't reformat.
    // Future / unparseable timestamps are omitted (never shown as success).
    let last_success_at = match (
        parseable_not_future(cal.last_success_at.as_deref(), now_unix),
        parseable_not_future(cal.last_synced_at.as_deref(), now_unix),
    ) {
        (Some(s), _) => Some(s.to_string()),
        (None, Some(s)) => Some(s.to_string()),
        (None, None) => None,
    };

    let stale = if state == CalendarReplicaState::Disabled {
        false
    } else {
        match success_unix {
            None => true,
            Some(ts) => now_unix.saturating_sub(ts) >= SYNC_HEALTH_STALE_SECS,
        }
    };

    let error_code = if cal.last_error_code.is_empty() {
        None
    } else {
        Some(cal.last_error_code.clone())
    };

    let retry_after_seconds = cal
        .next_retry_at
        .as_deref()
        .and_then(rfc3339_to_unix_secs)
        .and_then(|next| {
            if next > now_unix {
                Some(next - now_unix)
            } else {
                None
            }
        });

    let projection = if cal.projection.is_empty() {
        REPLICA_PROJECTION.to_string()
    } else {
        cal.projection.clone()
    };

    let initial_sync_complete = cal.initial_sync_complete || last_synced_parseable;

    CalendarSyncView {
        calendar_id: cal.id.clone(),
        state,
        initial_sync_complete,
        last_success_at,
        last_attempt_at: cal.last_attempt_at.clone(),
        stale,
        error_code,
        retry_after_seconds,
        projection,
        cache_revision: cal.cache_revision,
    }
}

/// Parseable RFC 3339 and not unreasonably in the future.
fn valid_success_unix(ts: Option<&str>, now_unix: i64) -> Option<i64> {
    let u = ts.and_then(rfc3339_to_unix_secs)?;
    if u > now_unix.saturating_add(FUTURE_SKEW_SECS) {
        None
    } else {
        Some(u)
    }
}

fn parseable_not_future<'a>(ts: Option<&'a str>, now_unix: i64) -> Option<&'a str> {
    let s = ts?;
    let u = rfc3339_to_unix_secs(s)?;
    if u > now_unix.saturating_add(FUTURE_SKEW_SECS) {
        None
    } else {
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::HttpError;
    use crate::repo::RepoError;

    const NOW: i64 = 1_700_000_000; // 2023-11-14T22:13:20Z

    fn base_cal(id: &str) -> GoogleCalendar {
        GoogleCalendar {
            id: id.to_string(),
            user_id: "u-1".to_string(),
            google_calendar_id: "primary@example.com".to_string(),
            summary: "Work".to_string(),
            time_zone: "UTC".to_string(),
            is_primary: true,
            access_role: "owner".to_string(),
            sync_enabled: true,
            sync_token: String::new(),
            last_synced_at: None,
            event_labels: "[]".to_string(),
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
            projection: REPLICA_PROJECTION.to_string(),
            created_at: "2023-01-01T00:00:00Z".to_string(),
            updated_at: "2023-01-01T00:00:00Z".to_string(),
            deleted_at: None,
        }
    }

    fn view_with(state: CalendarReplicaState, stale: bool) -> CalendarSyncView {
        CalendarSyncView {
            calendar_id: "c".into(),
            state,
            initial_sync_complete: true,
            last_success_at: None,
            last_attempt_at: None,
            stale,
            error_code: None,
            retry_after_seconds: None,
            projection: REPLICA_PROJECTION.into(),
            cache_revision: 0,
        }
    }

    // ── aggregate ──────────────────────────────────────────────

    #[test]
    fn aggregate_all_ready_is_ready() {
        let views = vec![
            view_with(CalendarReplicaState::Ready, false),
            view_with(CalendarReplicaState::Ready, false),
        ];
        assert_eq!(aggregate_sync_status(&views), SyncAggregateStatus::Ready);
    }

    #[test]
    fn aggregate_one_retrying_is_degraded() {
        let views = vec![
            view_with(CalendarReplicaState::Ready, false),
            view_with(CalendarReplicaState::Retrying, false),
        ];
        assert_eq!(aggregate_sync_status(&views), SyncAggregateStatus::Degraded);
    }

    #[test]
    fn aggregate_authorization_required_beats_stale_and_retrying() {
        let views = vec![
            view_with(CalendarReplicaState::Retrying, true),
            view_with(CalendarReplicaState::AuthorizationRequired, false),
            view_with(CalendarReplicaState::Ready, true),
        ];
        assert_eq!(
            aggregate_sync_status(&views),
            SyncAggregateStatus::AuthorizationRequired
        );
    }

    #[test]
    fn aggregate_ignores_disabled_calendars() {
        let views = vec![
            view_with(CalendarReplicaState::Disabled, false),
            view_with(CalendarReplicaState::Ready, false),
        ];
        assert_eq!(aggregate_sync_status(&views), SyncAggregateStatus::Ready);

        // Only disabled → ready (empty enabled set).
        let only_disabled = vec![view_with(CalendarReplicaState::Disabled, true)];
        assert_eq!(
            aggregate_sync_status(&only_disabled),
            SyncAggregateStatus::Ready
        );
    }

    #[test]
    fn aggregate_empty_list_is_ready() {
        assert_eq!(aggregate_sync_status(&[]), SyncAggregateStatus::Ready);
        let env = events_sync_envelope(&[], NOW);
        assert_eq!(env.status, SyncAggregateStatus::Ready);
        assert!(env.calendars.is_empty());
    }

    #[test]
    fn aggregate_stale_ready_is_degraded() {
        let views = vec![view_with(CalendarReplicaState::Ready, true)];
        assert_eq!(aggregate_sync_status(&views), SyncAggregateStatus::Degraded);
    }

    #[test]
    fn aggregate_never_initialized_is_degraded() {
        let views = vec![view_with(CalendarReplicaState::NeverInitialized, true)];
        assert_eq!(aggregate_sync_status(&views), SyncAggregateStatus::Degraded);
    }

    // ── stale ──────────────────────────────────────────────────

    #[test]
    fn stale_success_30_min_ago_is_not_stale() {
        let mut cal = base_cal("cal-1");
        cal.sync_status = "ready".into();
        cal.initial_sync_complete = true;
        cal.last_success_at = Some(unix_secs_to_rfc3339(NOW - 30 * 60));
        let view = calendar_sync_view(&cal, NOW);
        assert!(!view.stale);
        assert_eq!(view.state, CalendarReplicaState::Ready);
    }

    #[test]
    fn stale_success_2_hours_ago_is_stale() {
        let mut cal = base_cal("cal-1");
        cal.sync_status = "ready".into();
        cal.initial_sync_complete = true;
        cal.last_success_at = Some(unix_secs_to_rfc3339(NOW - 2 * 60 * 60));
        let view = calendar_sync_view(&cal, NOW);
        assert!(view.stale);
    }

    #[test]
    fn stale_missing_success_is_stale_when_enabled() {
        let mut cal = base_cal("cal-1");
        cal.sync_status = "never_initialized".into();
        let view = calendar_sync_view(&cal, NOW);
        assert!(view.stale);
        assert_eq!(view.state, CalendarReplicaState::NeverInitialized);
    }

    #[test]
    fn stale_future_timestamp_is_stale() {
        let mut cal = base_cal("cal-1");
        cal.sync_status = "ready".into();
        cal.initial_sync_complete = true;
        // Far in the future — invalid success.
        cal.last_success_at = Some(unix_secs_to_rfc3339(NOW + 3600));
        let view = calendar_sync_view(&cal, NOW);
        assert!(view.stale);
        assert!(view.last_success_at.is_none());
    }

    #[test]
    fn stale_disabled_is_not_stale() {
        let mut cal = base_cal("cal-1");
        cal.sync_enabled = false;
        cal.sync_status = "retrying".into();
        // No success timestamp.
        let view = calendar_sync_view(&cal, NOW);
        assert_eq!(view.state, CalendarReplicaState::Disabled);
        assert!(!view.stale);
    }

    // ── compat ─────────────────────────────────────────────────

    #[test]
    fn compat_last_synced_at_implies_ready_and_initialized() {
        let mut cal = base_cal("cal-1");
        // Empty sync_status / last_success_at — production rows before health.
        cal.sync_status = String::new();
        cal.last_success_at = None;
        cal.initial_sync_complete = false;
        cal.last_synced_at = Some(unix_secs_to_rfc3339(NOW - 10 * 60));
        let view = calendar_sync_view(&cal, NOW);
        assert_eq!(view.state, CalendarReplicaState::Ready);
        assert!(view.initial_sync_complete);
        assert!(!view.stale);
        assert_eq!(
            view.last_success_at.as_deref(),
            Some(unix_secs_to_rfc3339(NOW - 10 * 60).as_str())
        );
    }

    // ── sanitized JSON ─────────────────────────────────────────

    #[test]
    fn sanitized_json_omits_secrets_and_keeps_error_code() {
        let mut cal = base_cal("cal-1");
        cal.sync_token = "secret-sync-token-xyz".into();
        cal.lease_owner = "lease-secret-abc".into();
        cal.last_error_code = "storage_transient".into();
        cal.sync_status = "retrying".into();
        cal.initial_sync_complete = true;
        cal.last_success_at = Some(unix_secs_to_rfc3339(NOW - 2 * 60 * 60));
        cal.last_attempt_at = Some(unix_secs_to_rfc3339(NOW - 60));

        let view = calendar_sync_view(&cal, NOW);
        let json = serde_json::to_string(&view).unwrap();

        assert!(json.contains("storage_transient"), "{json}");
        assert!(!json.contains("secret-sync-token-xyz"), "{json}");
        assert!(!json.contains("lease-secret-abc"), "{json}");
        assert!(!json.contains("sync_token"), "{json}");
        assert!(!json.contains("lease_owner"), "{json}");
        assert!(!json.contains("raw_json"), "{json}");
        assert!(!json.contains("access_token"), "{json}");
    }

    // ── classify ───────────────────────────────────────────────

    #[test]
    fn classify_sync_error_mapping_table() {
        assert_eq!(
            classify_sync_error(&CalendarError::GoogleNotFound),
            SyncErrorCode::NotFound
        );
        assert_eq!(
            classify_sync_error(&CalendarError::Repo(RepoError::Backend("db".into()))),
            SyncErrorCode::StorageTransient
        );
        assert_eq!(
            classify_sync_error(&CalendarError::InvalidResponse("bad".into())),
            SyncErrorCode::MappingPoison
        );
        assert_eq!(
            classify_sync_error(&CalendarError::GoogleApi(
                "google events.list returned 401".into()
            )),
            SyncErrorCode::AuthRevoked
        );
        assert_eq!(
            classify_sync_error(&CalendarError::GoogleApi(
                "token refresh failed: invalid_grant".into()
            )),
            SyncErrorCode::AuthRevoked
        );
        assert_eq!(
            classify_sync_error(&CalendarError::GoogleApi(
                "google events.list returned 410".into()
            )),
            SyncErrorCode::Gone
        );
        assert_eq!(
            classify_sync_error(&CalendarError::GoogleApi(
                "google events.list returned 429".into()
            )),
            SyncErrorCode::RateLimited
        );
        assert_eq!(
            classify_sync_error(&CalendarError::GoogleApi(
                "google events.list returned 403".into()
            )),
            SyncErrorCode::RateLimited
        );
        assert_eq!(
            classify_sync_error(&CalendarError::GoogleApi(
                "google events.list returned 500".into()
            )),
            SyncErrorCode::GoogleTransient
        );
        assert_eq!(
            classify_sync_error(&CalendarError::GoogleApi(
                "google events.list returned 503".into()
            )),
            SyncErrorCode::GoogleTransient
        );
        assert_eq!(
            classify_sync_error(&CalendarError::Http(HttpError::Message("boom".into()))),
            SyncErrorCode::GoogleTransient
        );
        assert_eq!(
            classify_sync_error(&CalendarError::GoogleApi(
                "google events.list returned 418".into()
            )),
            SyncErrorCode::Unknown
        );
        assert_eq!(
            classify_sync_error(&CalendarError::NotFound),
            SyncErrorCode::Unknown
        );

        // Codes are categories — never the raw message.
        let code = classify_sync_error(&CalendarError::GoogleApi(
            "google events.list returned 401 body={\"error\":\"invalid_grant\"}".into(),
        ));
        assert_eq!(code.as_str(), "auth_revoked");
        assert!(!code.as_str().contains("invalid_grant") || code.as_str() == "auth_revoked");
    }

    #[test]
    fn replica_state_for_error_maps_auth_and_retrying() {
        assert_eq!(
            replica_state_for_error(SyncErrorCode::AuthRevoked),
            CalendarReplicaState::AuthorizationRequired
        );
        assert_eq!(
            replica_state_for_error(SyncErrorCode::Gone),
            CalendarReplicaState::Retrying
        );
        assert_eq!(
            replica_state_for_error(SyncErrorCode::StorageTransient),
            CalendarReplicaState::Retrying
        );
    }

    // ── fingerprint ────────────────────────────────────────────

    #[test]
    fn fingerprint_is_deterministic_full_sha256_hex() {
        let a = replica_query_fingerprint();
        let b = replica_query_fingerprint();
        assert_eq!(a, b);
        assert_eq!(a.len(), 64, "full SHA-256 hex");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()), "{a}");
        // Does not embed a live token.
        assert!(!a.contains("syncToken"));
        assert!(!a.contains("token"));
        // Stable against known input.
        let expected = {
            let digest = Sha256::digest(b"singleEvents=false&timeMin=&eventTypes=");
            hex_encode(&digest)
        };
        assert_eq!(a, expected);
    }

    // ── backoff ────────────────────────────────────────────────

    #[test]
    fn backoff_streak_one_is_tens_of_seconds() {
        let next = next_retry_unix(NOW, 1);
        let delay = next - NOW;
        assert!(next > NOW);
        // base 30s ±20% → roughly 24–36s
        assert!(
            (20..=50).contains(&delay),
            "streak 1 delay should be tens of seconds, got {delay}"
        );
    }

    #[test]
    fn backoff_high_streak_caps_near_one_hour() {
        let next = next_retry_unix(NOW, 20);
        let delay = next - NOW;
        assert!(next > NOW);
        // cap 3600 ±20% → roughly 2880–4320
        assert!(
            (2500..=4500).contains(&delay),
            "high streak should cap near 1h, got {delay}"
        );
    }

    #[test]
    fn backoff_always_after_now_and_deterministic() {
        assert_eq!(next_retry_unix(NOW, 3), next_retry_unix(NOW, 3));
        for streak in [0, 1, 2, 5, 100] {
            assert!(next_retry_unix(NOW, streak) > NOW, "streak={streak}");
        }
        let rfc = next_retry_rfc3339(NOW, 2);
        assert!(rfc.ends_with('Z'), "{rfc}");
        assert_eq!(rfc3339_to_unix_secs(&rfc), Some(next_retry_unix(NOW, 2)));
    }

    // ── view bits ──────────────────────────────────────────────

    #[test]
    fn retry_after_seconds_when_next_retry_in_future() {
        let mut cal = base_cal("cal-1");
        cal.sync_status = "retrying".into();
        cal.next_retry_at = Some(unix_secs_to_rfc3339(NOW + 90));
        cal.last_success_at = Some(unix_secs_to_rfc3339(NOW - 10));
        let view = calendar_sync_view(&cal, NOW);
        assert_eq!(view.retry_after_seconds, Some(90));
    }

    #[test]
    fn empty_projection_defaults_to_replica_constant() {
        let mut cal = base_cal("cal-1");
        cal.projection = String::new();
        cal.last_synced_at = Some(unix_secs_to_rfc3339(NOW));
        let view = calendar_sync_view(&cal, NOW);
        assert_eq!(view.projection, REPLICA_PROJECTION);
    }

    #[test]
    fn serde_snake_case_for_enums() {
        assert_eq!(
            serde_json::to_string(&SyncAggregateStatus::AuthorizationRequired).unwrap(),
            "\"authorization_required\""
        );
        assert_eq!(
            serde_json::to_string(&CalendarReplicaState::NeverInitialized).unwrap(),
            "\"never_initialized\""
        );
        assert_eq!(
            serde_json::to_string(&SyncErrorCode::AuthRevoked).unwrap(),
            "\"auth_revoked\""
        );
    }
}
