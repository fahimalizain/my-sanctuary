use super::catalog::refresh_calendar_list;
use super::labels::ensure_event_labels;
use super::repair::repair_inflight_operations;
use super::replica::{lease_expires_at, mint_lease_owner, sync_replica};
use super::sync::{
    classify_sync_error, next_retry_rfc3339, replica_state_for_error, SyncErrorCode,
};
use super::watch::{
    is_public_https_callback, renew_watch_if_needed, stop_watches_for_calendar,
};
use super::{
    CalendarError, CRON_MAX_REPLICA_CALENDARS, CRON_SYNC_STALE_SECS,
};
use crate::config::OAuthConfig;
use crate::models::GoogleCalendar;
use crate::oauth::HttpClient;
use crate::repo::{
    CalendarEventOperationRepo, CalendarEventRepo, CalendarRepo, TokenRepo, WatchChannelRepo,
};
use crate::time::{rfc3339_to_unix_secs, unix_secs_to_rfc3339};
use crate::token::{is_refresh_auth_revoked, refresh_if_needed, GoogleAccess, TokenError};
use std::collections::{HashMap, HashSet};

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
/// 2. GET-only [`repair_inflight_operations`] for stuck journal rows. Repair
///    errors append to the report; they never abort replica/renew work.
/// 3. [`refresh_calendar_list`] (incremental when a list cursor exists).
///    List errors are logged; existing sync-enabled calendars still get
///    replica/renew work. New local calendar ids are pushed to `published`.
/// 4. For each living sync-enabled calendar of the user:
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
/// 5. Leftover-stop tail: [`WatchChannelRepo::list_all`], resolve each calendar
///    via [`CalendarRepo::get_by_id_unfiltered`], stop channels when the
///    calendar is missing-from-living-path (soft-deleted) or `!sync_enabled`.
///    Does **not** require a public HTTPS callback (stop needs no webhook URL).
///    Does **not** abort prior replica work. Failed stops leave rows + error.
pub async fn run_fallback_cron(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    operations: &dyn CalendarEventOperationRepo,
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
                    report.errors.extend(
                        stamp_auth_revoked_for_user(
                            calendars,
                            user_id,
                            now_unix,
                            &now_rfc3339,
                        )
                        .await,
                    );
                }
                continue;
            }
        };

        // GET-only journal repair before replica so stuck writes finish and
        // inflight skip set shrinks. Failures never abort the rest of the tick.
        let repair_errors = repair_inflight_operations(
            http,
            calendars,
            events,
            operations,
            &access,
            user_id,
            now_unix,
        )
        .await;
        report.errors.extend(repair_errors);

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
                match sync_calendar(
                    http,
                    calendars,
                    events,
                    operations,
                    &access,
                    cal,
                    &now_rfc3339,
                )
                .await
                {
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
    operations: &dyn CalendarEventOperationRepo,
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
        // Refresh event-label cache when empty or stale (TTL).
        ensure_event_labels(http, calendars, access, &fresh, now_rfc3339).await?;
        sync_replica(
            http,
            calendars,
            events,
            operations,
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

/// Stamp `auth_revoked` / `authorization_required` on every living
/// sync-enabled calendar for `user_id`. Keeps events and cursors.
///
/// Returns human-readable failures (never tokens). Used by the fallback cron
/// and by GET handlers when Google rejects the refresh grant.
pub(crate) async fn stamp_auth_revoked_for_user(
    calendars: &dyn CalendarRepo,
    user_id: &str,
    now_unix: i64,
    now_rfc3339: &str,
) -> Vec<String> {
    let mut errors = Vec::new();
    match calendars.list_by_user_id(user_id).await {
        Ok(user_cals) => {
            for cal in user_cals.iter().filter(|c| c.sync_enabled) {
                if let Err(persist_err) = persist_sync_failure(
                    calendars,
                    cal,
                    SyncErrorCode::AuthRevoked,
                    now_unix,
                    now_rfc3339,
                )
                .await
                {
                    errors.push(format!(
                        "failed to stamp auth_revoked for calendar {}: {persist_err}",
                        cal.id
                    ));
                }
            }
        }
        Err(list_err) => errors.push(format!(
            "failed to list calendars for auth_revoked stamp (user {user_id}): {list_err}"
        )),
    }
    errors
}

/// Persist a classified failure without advancing the sync cursor.
///
/// Uses `cal.failure_streak + 1` for backoff (the in-memory snapshot at
/// invocation — attempt does not bump streak). Returns a repo error if the
/// health write itself fails so callers never drop health silently.
pub(crate) async fn persist_sync_failure(
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
