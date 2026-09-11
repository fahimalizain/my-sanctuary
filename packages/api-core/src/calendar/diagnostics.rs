//! Structured replica-walk diagnostics and independent operator warnings.
//!
//! Pure, unit-testable records for one replica walk and for the 1h stale /
//! escalated / authorization operator signal. Never carries cursors, full
//! URLs, credentials, event bodies, summaries, `raw_json`, access tokens,
//! page tokens, or sync tokens (ADR 0005).

use serde::{Deserialize, Serialize};

use super::sync::SYNC_HEALTH_STALE_SECS;
use super::sync::SyncErrorCode;
use crate::models::GoogleCalendar;
use crate::time::rfc3339_to_unix_secs;

/// Allow clocks a little ahead of `now_unix` before treating a success
/// timestamp as future/invalid — same skew as [`super::sync::calendar_sync_view`].
const FUTURE_SKEW_SECS: i64 = 120;

/// What kicked off this replica walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplicaWalkTrigger {
    Cron,
    Webhook,
    Unspecified,
}

/// Phase last reached during the walk (or terminal outcome phase).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplicaWalkPhase {
    LeaseAcquire,
    Fetch,
    Apply,
    Checkpoint,
    Release,
    Done,
}

/// Outcome of the terminal checkpoint / publication step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointResult {
    Published,
    MissingSyncToken,
    LeaseLost,
    LeaseBusy,
    Error,
    #[default]
    NotAttempted,
}

/// Independent operator-facing warning level (not the aggregate `sync.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OperatorWarningLevel {
    #[default]
    None,
    Stale,
    Escalated,
    AuthorizationRequired,
}

/// Tunable thresholds for [`classify_operator_warning`].
///
/// Defaults: 1h stale, 3h / streak-5 escalated. Not an SLA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperatorWarningThresholds {
    pub stale_secs: i64,
    pub escalated_age_secs: i64,
    pub escalated_streak: i64,
}

impl Default for OperatorWarningThresholds {
    fn default() -> Self {
        Self {
            stale_secs: SYNC_HEALTH_STALE_SECS,
            escalated_age_secs: 3 * SYNC_HEALTH_STALE_SECS,
            escalated_streak: 5,
        }
    }
}

/// Caller-supplied identity for one walk (run id, trigger, version, start).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicaWalkMeta {
    pub run_id: String,
    pub trigger: ReplicaWalkTrigger,
    pub deployed_version: String,
    /// Unix epoch milliseconds when the walk started. `0` → duration_ms 0.
    pub started_unix_ms: i64,
}

/// Mutable counters collected while applying pages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicaApplyReport {
    /// Successful 2xx `events.list` pages applied.
    pub pages: u32,
    /// Rows passed to `upsert_batch_if_owner`.
    pub upserts: u32,
    /// Soft-deletes attempted.
    pub deletes: u32,
    /// HTTP GET attempts (includes 410 retries).
    pub attempts: u32,
    pub checkpoint: CheckpointResult,
    pub phase: ReplicaWalkPhase,
}

impl Default for ReplicaApplyReport {
    fn default() -> Self {
        Self {
            pages: 0,
            upserts: 0,
            deletes: 0,
            attempts: 0,
            checkpoint: CheckpointResult::NotAttempted,
            phase: ReplicaWalkPhase::LeaseAcquire,
        }
    }
}

/// Sanitized structured record for one replica walk.
///
/// All fields are `pub` so the Worker (slice 2) can stamp `deployed_version`
/// at log time without a second builder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicaWalkDiagnostic {
    pub run_id: String,
    pub trigger: ReplicaWalkTrigger,
    /// Local `google_calendars.id` only — never Google's calendar id alone.
    pub calendar_id: String,
    /// `APP_VERSION`; caller-supplied (cron leaves empty; Worker stamps).
    pub deployed_version: String,
    pub phase: ReplicaWalkPhase,
    pub attempts: u32,
    pub duration_ms: i64,
    pub pages: u32,
    pub upserts: u32,
    pub deletes: u32,
    pub checkpoint: CheckpointResult,
    /// Sanitized failure category; never a raw Google body.
    pub error_category: Option<SyncErrorCode>,
}

impl ReplicaWalkDiagnostic {
    /// Build a diagnostic from walk meta + apply report.
    ///
    /// `duration_ms = max(0, finished_unix_ms - started_unix_ms)`. When
    /// `started_unix_ms == 0` (tests / thin `sync_calendar` wrapper), duration
    /// is always 0.
    pub fn from_parts(
        meta: &ReplicaWalkMeta,
        calendar_id: impl Into<String>,
        report: &ReplicaApplyReport,
        error_category: Option<SyncErrorCode>,
        finished_unix_ms: i64,
    ) -> Self {
        let duration_ms = if meta.started_unix_ms == 0 {
            0
        } else {
            finished_unix_ms
                .saturating_sub(meta.started_unix_ms)
                .max(0)
        };
        Self {
            run_id: meta.run_id.clone(),
            trigger: meta.trigger,
            calendar_id: calendar_id.into(),
            deployed_version: meta.deployed_version.clone(),
            phase: report.phase,
            attempts: report.attempts,
            duration_ms,
            pages: report.pages,
            upserts: report.upserts,
            deletes: report.deletes,
            checkpoint: report.checkpoint,
            error_category,
        }
    }
}

/// One non-`None` operator warning for a calendar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorWarningRecord {
    pub calendar_id: String,
    pub level: OperatorWarningLevel,
    pub last_success_at: Option<String>,
    pub failure_streak: i64,
    pub age_secs: Option<i64>,
}

/// 16 random bytes as lowercase hex (32 chars) — same entropy as
/// [`super::replica::mint_lease_owner`].
pub fn mint_run_id() -> String {
    let mut bytes = [0u8; 16];
    let _ = getrandom::getrandom(&mut bytes);
    let mut out = String::with_capacity(32);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Classify the independent operator warning for one calendar.
///
/// Rules (tunable thresholds, not an SLA):
/// 1. `!sync_enabled` or stored state Disabled → `None`
/// 2. `sync_status == "authorization_required"` OR `last_error_code == "auth_revoked"`
///    → `AuthorizationRequired` immediately (ignore age)
/// 3. Else age from valid success (`last_success_at` then `last_synced_at`;
///    future/unparseable = missing)
/// 4. `Escalated` when `age >= escalated_age_secs` OR `failure_streak >= escalated_streak`
/// 5. Else `Stale` when success missing OR `age >= stale_secs`
/// 6. Else `None`
pub fn classify_operator_warning(
    cal: &GoogleCalendar,
    now_unix: i64,
    thresholds: &OperatorWarningThresholds,
) -> OperatorWarningLevel {
    if !cal.sync_enabled || cal.sync_status == "disabled" {
        return OperatorWarningLevel::None;
    }
    if cal.sync_status == "authorization_required" || cal.last_error_code == "auth_revoked" {
        return OperatorWarningLevel::AuthorizationRequired;
    }

    let success_unix = valid_success_unix(cal.last_success_at.as_deref(), now_unix)
        .or_else(|| valid_success_unix(cal.last_synced_at.as_deref(), now_unix));
    let age_secs = success_unix.map(|ts| now_unix.saturating_sub(ts));

    if cal.failure_streak >= thresholds.escalated_streak {
        return OperatorWarningLevel::Escalated;
    }
    if let Some(age) = age_secs {
        if age >= thresholds.escalated_age_secs {
            return OperatorWarningLevel::Escalated;
        }
        if age >= thresholds.stale_secs {
            return OperatorWarningLevel::Stale;
        }
        return OperatorWarningLevel::None;
    }

    // Missing / unparseable / future success → Stale.
    OperatorWarningLevel::Stale
}

/// Build an [`OperatorWarningRecord`] from a calendar + already-classified level.
pub fn operator_warning_record(
    cal: &GoogleCalendar,
    level: OperatorWarningLevel,
    now_unix: i64,
) -> OperatorWarningRecord {
    let success_unix = valid_success_unix(cal.last_success_at.as_deref(), now_unix)
        .or_else(|| valid_success_unix(cal.last_synced_at.as_deref(), now_unix));
    let age_secs = success_unix.map(|ts| now_unix.saturating_sub(ts));

    // Prefer the original stored string when valid (same preference as the
    // health view) so we do not reformat.
    let last_success_at = match (
        parseable_not_future(cal.last_success_at.as_deref(), now_unix),
        parseable_not_future(cal.last_synced_at.as_deref(), now_unix),
    ) {
        (Some(s), _) => Some(s.to_string()),
        (None, Some(s)) => Some(s.to_string()),
        (None, None) => None,
    };

    OperatorWarningRecord {
        calendar_id: cal.id.clone(),
        level,
        last_success_at,
        failure_streak: cal.failure_streak,
        age_secs,
    }
}

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
    use crate::time::unix_secs_to_rfc3339;

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
            event_labels_updated_at: Some("2026-08-17T00:00:00Z".to_string()),
            sync_query_fingerprint: String::new(),
            sync_status: "ready".to_string(),
            initial_sync_complete: true,
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
            watch_coverage: String::new(),
            event_coverage: String::new(),
            created_at: "2023-01-01T00:00:00Z".to_string(),
            updated_at: "2023-01-01T00:00:00Z".to_string(),
            deleted_at: None,
        }
    }

    fn thresholds() -> OperatorWarningThresholds {
        OperatorWarningThresholds::default()
    }

    #[test]
    fn warning_enabled_success_30_min_ago_is_none() {
        let mut cal = base_cal("cal-1");
        cal.last_success_at = Some(unix_secs_to_rfc3339(NOW - 30 * 60));
        assert_eq!(
            classify_operator_warning(&cal, NOW, &thresholds()),
            OperatorWarningLevel::None
        );
    }

    #[test]
    fn warning_enabled_success_2h_ago_is_stale() {
        let mut cal = base_cal("cal-1");
        cal.last_success_at = Some(unix_secs_to_rfc3339(NOW - 2 * 60 * 60));
        assert_eq!(
            classify_operator_warning(&cal, NOW, &thresholds()),
            OperatorWarningLevel::Stale
        );
    }

    #[test]
    fn warning_enabled_success_4h_ago_is_escalated_by_age() {
        let mut cal = base_cal("cal-1");
        cal.last_success_at = Some(unix_secs_to_rfc3339(NOW - 4 * 60 * 60));
        assert_eq!(
            classify_operator_warning(&cal, NOW, &thresholds()),
            OperatorWarningLevel::Escalated
        );
    }

    #[test]
    fn warning_enabled_recent_success_streak_5_is_escalated() {
        let mut cal = base_cal("cal-1");
        cal.last_success_at = Some(unix_secs_to_rfc3339(NOW - 10 * 60));
        cal.failure_streak = 5;
        assert_eq!(
            classify_operator_warning(&cal, NOW, &thresholds()),
            OperatorWarningLevel::Escalated
        );
    }

    #[test]
    fn warning_authorization_required_ignores_recent_success() {
        let mut cal = base_cal("cal-1");
        cal.sync_status = "authorization_required".into();
        cal.last_success_at = Some(unix_secs_to_rfc3339(NOW - 60));
        assert_eq!(
            classify_operator_warning(&cal, NOW, &thresholds()),
            OperatorWarningLevel::AuthorizationRequired
        );

        let mut cal2 = base_cal("cal-2");
        cal2.last_error_code = "auth_revoked".into();
        cal2.last_success_at = Some(unix_secs_to_rfc3339(NOW - 60));
        assert_eq!(
            classify_operator_warning(&cal2, NOW, &thresholds()),
            OperatorWarningLevel::AuthorizationRequired
        );
    }

    #[test]
    fn warning_disabled_missing_success_is_none() {
        let mut cal = base_cal("cal-1");
        cal.sync_enabled = false;
        cal.last_success_at = None;
        assert_eq!(
            classify_operator_warning(&cal, NOW, &thresholds()),
            OperatorWarningLevel::None
        );

        let mut cal2 = base_cal("cal-2");
        cal2.sync_status = "disabled".into();
        cal2.last_success_at = None;
        assert_eq!(
            classify_operator_warning(&cal2, NOW, &thresholds()),
            OperatorWarningLevel::None
        );
    }

    #[test]
    fn warning_missing_unparseable_future_success_is_stale() {
        let mut cal = base_cal("cal-1");
        cal.last_success_at = None;
        cal.last_synced_at = None;
        assert_eq!(
            classify_operator_warning(&cal, NOW, &thresholds()),
            OperatorWarningLevel::Stale
        );

        cal.last_success_at = Some("not-a-timestamp".into());
        cal.last_synced_at = Some("also-bad".into());
        assert_eq!(
            classify_operator_warning(&cal, NOW, &thresholds()),
            OperatorWarningLevel::Stale
        );

        cal.last_success_at = Some(unix_secs_to_rfc3339(NOW + 3600));
        cal.last_synced_at = Some(unix_secs_to_rfc3339(NOW + 7200));
        assert_eq!(
            classify_operator_warning(&cal, NOW, &thresholds()),
            OperatorWarningLevel::Stale
        );
    }

    #[test]
    fn warning_custom_thresholds_move_boundary() {
        let mut cal = base_cal("cal-1");
        // 90 minutes ago: default stale (1h) but custom stale of 2h → None.
        cal.last_success_at = Some(unix_secs_to_rfc3339(NOW - 90 * 60));
        let custom = OperatorWarningThresholds {
            stale_secs: 2 * 60 * 60,
            escalated_age_secs: 6 * 60 * 60,
            escalated_streak: 10,
        };
        assert_eq!(
            classify_operator_warning(&cal, NOW, &custom),
            OperatorWarningLevel::None
        );
        assert_eq!(
            classify_operator_warning(&cal, NOW, &thresholds()),
            OperatorWarningLevel::Stale
        );

        // streak 3 escalates only when threshold is 3.
        cal.last_success_at = Some(unix_secs_to_rfc3339(NOW - 10 * 60));
        cal.failure_streak = 3;
        let streak_custom = OperatorWarningThresholds {
            stale_secs: SYNC_HEALTH_STALE_SECS,
            escalated_age_secs: 3 * SYNC_HEALTH_STALE_SECS,
            escalated_streak: 3,
        };
        assert_eq!(
            classify_operator_warning(&cal, NOW, &streak_custom),
            OperatorWarningLevel::Escalated
        );
        assert_eq!(
            classify_operator_warning(&cal, NOW, &thresholds()),
            OperatorWarningLevel::None
        );
    }

    #[test]
    fn diagnostic_from_parts_zero_start_yields_zero_duration() {
        let meta = ReplicaWalkMeta {
            run_id: "abcd".into(),
            trigger: ReplicaWalkTrigger::Unspecified,
            deployed_version: "1.2.3".into(),
            started_unix_ms: 0,
        };
        let report = ReplicaApplyReport {
            pages: 1,
            upserts: 0,
            deletes: 0,
            attempts: 1,
            checkpoint: CheckpointResult::Published,
            phase: ReplicaWalkPhase::Done,
        };
        let d = ReplicaWalkDiagnostic::from_parts(
            &meta,
            "cal-1",
            &report,
            None,
            1_700_000_000_999,
        );
        assert_eq!(d.duration_ms, 0);
        assert_eq!(d.calendar_id, "cal-1");
        assert_eq!(d.checkpoint, CheckpointResult::Published);
    }

    #[test]
    fn diagnostic_json_is_sanitized() {
        let meta = ReplicaWalkMeta {
            run_id: "run-deadbeef".into(),
            trigger: ReplicaWalkTrigger::Cron,
            deployed_version: "0.1.0".into(),
            started_unix_ms: 1_700_000_000_000,
        };
        let report = ReplicaApplyReport {
            pages: 2,
            upserts: 3,
            deletes: 1,
            attempts: 3,
            checkpoint: CheckpointResult::Published,
            phase: ReplicaWalkPhase::Done,
        };
        let d = ReplicaWalkDiagnostic::from_parts(
            &meta,
            "cal-1",
            &report,
            Some(SyncErrorCode::MissingSyncToken),
            1_700_000_000_500,
        );
        let json = serde_json::to_string(&d).unwrap();

        for needle in [
            "run-deadbeef",
            "cron",
            "cal-1",
            "0.1.0",
            "done",
            "attempts",
            "duration_ms",
            "pages",
            "upserts",
            "deletes",
            "published",
            "missing_sync_token",
        ] {
            assert!(json.contains(needle), "missing {needle} in {json}");
        }

        for secret in [
            "syncToken",
            "pageToken",
            "access_token",
            "raw_json",
            "https://www.googleapis.com",
            "Standup",
            "lease-secret",
            "channel-token",
            "old-tok",
            "st-final",
        ] {
            assert!(!json.contains(secret), "leaked {secret} in {json}");
        }
        assert_eq!(d.duration_ms, 500);
    }

    #[test]
    fn mint_run_id_is_32_hex_chars() {
        let a = mint_run_id();
        let b = mint_run_id();
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()), "{a}");
        assert_ne!(a, b);
    }

    #[test]
    fn default_thresholds_use_sync_health_stale_secs() {
        let t = OperatorWarningThresholds::default();
        assert_eq!(t.stale_secs, SYNC_HEALTH_STALE_SECS);
        assert_eq!(t.escalated_age_secs, 3 * SYNC_HEALTH_STALE_SECS);
        assert_eq!(t.escalated_streak, 5);
    }
}
