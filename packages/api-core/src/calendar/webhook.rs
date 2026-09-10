use crate::models::{GoogleCalendar, WatchChannel};
use crate::repo::CalendarRepo;

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
