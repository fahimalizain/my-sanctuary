//! Session-authorized, per-calendar repair action (issue #59).
//!
//! Persists a tracked reseed (`full_sync_requested` / `rebuilding`) and returns
//! queued / in-progress / cooldown immediately. Never walks the replica, never
//! touches `sync_token`, never awaits Google. The fallback cron is the walker.

use super::CalendarError;
use crate::repo::CalendarRepo;
use crate::time::{rfc3339_to_unix_secs, unix_secs_to_rfc3339};

/// Minimum seconds between a successful publish and a *new* user-triggered
/// reseed. Coalesce / in-flight paths skip this gate.
pub const CALENDAR_REPAIR_COOLDOWN_SECS: i64 = 60;

/// Outcome of [`request_calendar_repair`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CalendarRepairStatus {
    Queued,
    InProgress,
    Cooldown,
}

/// JSON body for `POST /api/calendar/calendars/:id/repair`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CalendarRepairResponse {
    pub status: CalendarRepairStatus,
    pub retry_after_seconds: Option<i64>,
}

/// Request a tracked reseed for one calendar owned by `user_id`.
///
/// Decision order (strict):
/// 1. Missing / wrong owner → [`CalendarError::NotFound`] (no existence leak).
/// 2. Capability refusals → [`CalendarError::Invalid`] with a stable message.
/// 3. Unexpired lease → coalesce in-progress (ensure flag, no walk).
/// 4. Already flagged → queued (no second write).
/// 5. Recent success within cooldown → cooldown (no persist).
/// 6. Otherwise → [`CalendarRepo::begin_replica_reseed`] → queued.
///
/// `now_unix` is caller-supplied — never `SystemTime`.
pub async fn request_calendar_repair(
    calendars: &dyn CalendarRepo,
    user_id: &str,
    calendar_id: &str,
    now_unix: i64,
) -> Result<CalendarRepairResponse, CalendarError> {
    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);

    let Some(cal) = calendars.get_by_id(calendar_id).await? else {
        return Err(CalendarError::NotFound);
    };
    if cal.user_id != user_id {
        return Err(CalendarError::NotFound);
    }

    if cal.access_role == "freeBusyReader" {
        return Err(CalendarError::Invalid(
            "calendar does not support replica repair".into(),
        ));
    }
    if !cal.sync_enabled || cal.sync_status == "disabled" {
        return Err(CalendarError::Invalid("calendar sync is disabled".into()));
    }
    if cal.sync_status == "authorization_required" {
        return Err(CalendarError::Invalid(
            "calendar requires reauthorization".into(),
        ));
    }

    // Unexpired lease = non-empty owner and (no expiry or expiry >= now).
    // Same string compare as `ensure_lease_held` (`exp < now` ⇒ expired).
    let lease_in_flight = !cal.lease_owner.is_empty()
        && cal
            .lease_expires_at
            .as_deref()
            .map(|exp| exp >= now_rfc3339.as_str())
            .unwrap_or(true);

    if lease_in_flight {
        if !cal.full_sync_requested {
            calendars
                .begin_replica_reseed(&cal.id, &now_rfc3339)
                .await?;
        }
        return Ok(CalendarRepairResponse {
            status: CalendarRepairStatus::InProgress,
            retry_after_seconds: None,
        });
    }

    if cal.full_sync_requested {
        return Ok(CalendarRepairResponse {
            status: CalendarRepairStatus::Queued,
            retry_after_seconds: None,
        });
    }

    if let Some(s) = cal
        .last_success_at
        .as_deref()
        .and_then(rfc3339_to_unix_secs)
    {
        if s <= now_unix {
            let age = now_unix - s;
            if age < CALENDAR_REPAIR_COOLDOWN_SECS {
                return Ok(CalendarRepairResponse {
                    status: CalendarRepairStatus::Cooldown,
                    retry_after_seconds: Some(CALENDAR_REPAIR_COOLDOWN_SECS - age),
                });
            }
        }
    }

    calendars
        .begin_replica_reseed(&cal.id, &now_rfc3339)
        .await?;

    Ok(CalendarRepairResponse {
        status: CalendarRepairStatus::Queued,
        retry_after_seconds: None,
    })
}
