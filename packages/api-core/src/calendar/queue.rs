//! Queue-triggered replica sync orchestration (issue #80).
//!
//! A thin latency layer on top of [`sync_calendar_traced`]: the Worker maps
//! [`QueueSyncAction`] to `message.ack()` / `message.retry()` / `queue.send()`
//! then ack. Dirty generation plus the fallback cron remain the correctness
//! contract — this path never puts sync back into `wait_until`.

use super::cron::{sync_calendar_traced, SyncCalendarOutcome};
use super::diagnostics::{mint_run_id, ReplicaWalkDiagnostic, ReplicaWalkMeta, ReplicaWalkTrigger};
use crate::models::GoogleCalendar;
use crate::oauth::HttpClient;
use crate::repo::{CalendarEventOperationRepo, CalendarEventRepo, CalendarRepo};
use crate::time::{rfc3339_to_unix_secs, unix_secs_to_rfc3339, Clock};
use crate::token::GoogleAccess;

/// Queue consumer action. The Worker maps these to
/// `message.ack()` / `message.retry()` / `queue.send()` then ack.
///
/// `Retry` is reserved. Current rules never return it: sync failure Acks
/// so `next_retry_at` backoff is preserved and cron owns retry (issue #80
/// open question, settled as Ack).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueSyncAction {
    Ack,
    Retry,
    Reenqueue,
}

#[derive(Debug, Clone)]
pub struct QueueSyncReport {
    pub action: QueueSyncAction,
    /// True only when this invocation published a replica
    /// (`SyncCalendarOutcome::Published`). Worker notifies browsers from this.
    pub published: bool,
    /// Present when a walk was attempted (Published / LeaseBusy / Err).
    /// None on skip paths (missing / disabled / soft-deleted / backoff /
    /// already-clean / freeBusy / authorization_required).
    pub diagnostic: Option<ReplicaWalkDiagnostic>,
}

/// JSON body for the `calendar-sync` queue. Worker serializes this.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CalendarSyncMessage {
    pub calendar_id: String,
}

fn ack_skip() -> QueueSyncReport {
    QueueSyncReport {
        action: QueueSyncAction::Ack,
        published: false,
        diagnostic: None,
    }
}

/// Queue consumer for one calendar-sync message: re-read living row, honor
/// skip gates, walk via [`sync_calendar_traced`] when still dirty, then decide
/// [`QueueSyncAction`].
///
/// Never throws. Repo / walk failures Ack so cron can recover. Does not bump
/// dirty on re-enqueue.
pub async fn run_queue_sync(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    operations: &dyn CalendarEventOperationRepo,
    access: &GoogleAccess,
    cal: &GoogleCalendar,
    now_unix: i64,
    clock: &dyn Clock,
) -> QueueSyncReport {
    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);

    // Re-read the living row first — the message payload is a hint only.
    let live = match calendars.get_by_id(&cal.id).await {
        Ok(Some(row)) => row,
        // Missing or soft-deleted (get_by_id filters deleted_at).
        Ok(None) => return ack_skip(),
        // Repo error: never throw; cron recovers.
        Err(_) => return ack_skip(),
    };

    // Skip gates: Ack, no Google, no dirty change, diagnostic=None.
    if !live.sync_enabled {
        return ack_skip();
    }
    if live.access_role == "freeBusyReader" {
        return ack_skip();
    }
    if live.sync_status == "authorization_required" {
        return ack_skip();
    }
    // Honor failure backoff; queue must not re-hammer Google; cron owns retry.
    if let Some(retry) = live
        .next_retry_at
        .as_deref()
        .and_then(rfc3339_to_unix_secs)
    {
        if retry > now_unix {
            return ack_skip();
        }
    }
    // Duplicate / already-applied hint — no redundant Google call.
    if live.dirty_requested_generation <= live.dirty_applied_generation {
        return ack_skip();
    }

    let traced = sync_calendar_traced(
        http,
        calendars,
        events,
        operations,
        access,
        &live,
        &now_rfc3339,
        clock,
        ReplicaWalkMeta {
            run_id: mint_run_id(),
            // Webhook latency path — do not add a Queue trigger variant.
            trigger: ReplicaWalkTrigger::Webhook,
            deployed_version: String::new(),
            started_unix_ms: now_unix.saturating_mul(1000),
        },
    )
    .await;

    let diagnostic = Some(traced.diagnostic);

    match traced.outcome {
        Ok(SyncCalendarOutcome::Published) => {
            // Post-publish generation predicate (live requested vs applied
            // after mark_dirty_applied), NOT a pre/post requested comparison.
            // sync_calendar snapshots requested under the lease and
            // mark_dirty_applied advances applied only to that snapshot, so
            // post-publish requested > applied means a bump landed after the
            // snapshot. Multiple mid-walk bumps still produce one Reenqueue
            // (coalescing). A re-enqueue does not bump dirty.
            let action = match calendars.get_by_id(&live.id).await {
                Ok(Some(after))
                    if after.dirty_requested_generation > after.dirty_applied_generation =>
                {
                    QueueSyncAction::Reenqueue
                }
                // Clean, or re-read failed / None → Ack (dirty remains; cron
                // recovers). Never throw.
                _ => QueueSyncAction::Ack,
            };
            QueueSyncReport {
                action,
                published: true,
                diagnostic,
            }
        }
        Ok(SyncCalendarOutcome::LeaseBusy) => {
            // Every bump has its own message; the lease holder performs the
            // follow-up re-enqueue on still-dirty. Do not Retry with delay
            // (bypasses backoff, exhausts max_retries against walks that can
            // exceed 90s).
            QueueSyncReport {
                action: QueueSyncAction::Ack,
                published: false,
                diagnostic,
            }
        }
        Err(_) => {
            // sync_calendar already recorded the failure and next_retry_at;
            // queue retry would bypass backoff and re-hammer Google.
            // Settled open question: Ack, cron owns retry.
            QueueSyncReport {
                action: QueueSyncAction::Ack,
                published: false,
                diagnostic,
            }
        }
    }
}
