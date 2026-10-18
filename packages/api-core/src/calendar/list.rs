use super::apply::EventClassifyCtx;
use super::catalog::refresh_calendar_list;
use super::cron::stamp_auth_revoked_for_user;
use super::sync::{classify_watch_coverage, events_sync_envelope, EventsSyncEnvelope};
use super::watch::{
    ensure_watch_with_channels, is_public_https_callback, stop_watches_for_calendar,
};
use super::window::fetch_and_apply_window;
use super::CalendarError;
use crate::calendar_color::PaintTaxonomy;
use crate::models::{CalendarEvent, GoogleCalendar, WatchChannel};
use crate::oauth::HttpClient;
use crate::repo::{CalendarEventRepo, CalendarRepo, WatchChannelRepo};
use crate::time::{add_months_unix, rfc3339_to_unix_secs, unix_secs_to_rfc3339};
use crate::token::{is_refresh_auth_revoked, GoogleAccess, TokenError};
use std::collections::{HashMap, HashSet};

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
    /// The user's calendars as of the end of the request (re-read only when
    /// the request mutated them), so paint can map local → Google calendar
    /// ids without another read. `watch_coverage` is stamped in memory.
    pub calendars: Vec<GoogleCalendar>,
}

/// The reads every `GET /events` starts with, issued concurrently: the
/// user's calendars and all of their watch channels (one query, not one per
/// calendar). The worker runs this alongside the token read.
///
/// A channel-list failure is not fatal (listing must keep working): it is
/// carried as `Err` so [`list_events_with_snapshot`] skips ensure-watch and
/// coverage stamping and reports it in `sync_errors`.
#[derive(Debug, Clone)]
pub struct ListSnapshot {
    pub calendars: Vec<GoogleCalendar>,
    pub channels: Result<Vec<WatchChannel>, String>,
}

/// Loads a [`ListSnapshot`]; a calendar-list failure fails the request, a
/// channel-list failure is carried inside the snapshot.
pub async fn load_list_snapshot(
    calendars: &dyn CalendarRepo,
    watches: &dyn WatchChannelRepo,
    user_id: &str,
) -> Result<ListSnapshot, CalendarError> {
    let (cals, channels) = futures::join!(
        calendars.list_by_user_id(user_id),
        watches.list_by_user_id(user_id)
    );
    Ok(ListSnapshot {
        calendars: cals?,
        channels: channels.map_err(|err| err.to_string()),
    })
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
    let snapshot = load_list_snapshot(calendars, watches, user_id).await?;
    // No taxonomy in hand on this wrapper: the window write-through stamps no
    // category (`EventClassifyCtx::none`). The worker route loads the paint
    // taxonomy itself and calls [`list_events_with_snapshot`] with it.
    list_events_with_snapshot(
        http,
        calendars,
        events,
        watches,
        access,
        user_id,
        start_rfc3339,
        end_rfc3339,
        now_unix,
        watch_callback_url,
        snapshot,
        None,
    )
    .await
}

/// [`list_events`] over a pre-loaded [`ListSnapshot`] (the worker loads it
/// concurrently with the token read). On a pure cache hit this issues exactly
/// one query — the range read — and no writes:
///
/// - calendars and channels come from `snapshot`; ensure-watch and the
///   coverage classification share the one channel list (re-listed for a
///   calendar only if this request created/stopped its channels);
/// - calendars are re-read only when the request mutated them (first-contact
///   import, dirty bump, sync disable, window fetch);
/// - `watch_coverage` is stamped in memory on every GET and persisted only
///   when it differs from the stored value (cron/webhook paths keep it fresh
///   otherwise), so steady-state GETs do not write.
///
/// `taxonomy` is the caller's already-loaded paint taxonomy (the worker loads
/// it in the same `join!` as the snapshot); the window write-through stamps
/// each row's leaf `category_id` from it. `None` — a failed taxonomy load —
/// only degrades the stamp (`EventClassifyCtx::none`); listing never 500s
/// because paint data is missing.
pub async fn list_events_with_snapshot(
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
    snapshot: ListSnapshot,
    taxonomy: Option<&PaintTaxonomy>,
) -> Result<CalendarListOutput, CalendarError> {
    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);
    let ListSnapshot {
        calendars: mut cals,
        channels: mut channels_result,
    } = snapshot;
    // Set whenever this request may have changed calendar rows or channels,
    // so the envelope is built from a re-read instead of the stale snapshot.
    let mut calendars_mutated = false;
    if cals.is_empty() {
        // First contact with Google: import the calendar list, then re-read.
        refresh_calendar_list(http, calendars, Some(watches), access, user_id, &now_rfc3339)
            .await?;
        let reloaded = load_list_snapshot(calendars, watches, user_id).await?;
        cals = reloaded.calendars;
        channels_result = reloaded.channels;
    }

    let mut sync_errors: Vec<String> = Vec::new();
    let mut channels_by_calendar: HashMap<String, Vec<WatchChannel>> = HashMap::new();
    let mut channels_available = true;
    match channels_result {
        Ok(rows) => {
            for channel in rows {
                channels_by_calendar
                    .entry(channel.calendar_id.clone())
                    .or_default()
                    .push(channel);
            }
        }
        Err(err) => {
            channels_available = false;
            sync_errors.push(format!("failed to list watch channels for user {user_id}: {err}"));
        }
    }
    // Calendars whose channel rows changed during this request (created or
    // stopped): their snapshot entry is stale, coverage re-lists them.
    let mut channels_stale: HashSet<String> = HashSet::new();

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
        // entirely when WATCH_CALLBACK_URL is unset or not public HTTPS (or
        // when the channel list failed — never create blind).
        if let Some(callback_url) = watch_callback_url.filter(|_| channels_available) {
            let existing: Vec<&WatchChannel> = channels_by_calendar
                .get(&cal.id)
                .map(|rows| rows.iter().collect())
                .unwrap_or_default();
            match ensure_watch_with_channels(
                http,
                watches,
                access,
                cal,
                callback_url,
                now_unix,
                &existing,
            )
            .await
            {
                Ok(created) => {
                    if created {
                        channels_stale.insert(cal.id.clone());
                    }
                }
                Err(CalendarError::GoogleNotFound) => {
                    sync_errors.push(format!(
                        "calendar {} ({}) returned 404 for events.watch — disabling sync",
                        cal.id, cal.google_calendar_id
                    ));
                    calendars_mutated = true;
                    if let Err(err) = calendars.set_sync_enabled(&cal.id, false, &now_rfc3339).await {
                        sync_errors.push(format!(
                            "failed to disable sync for calendar {}: {err}",
                            cal.id
                        ));
                    }
                    // A prior channel may exist for this calendar (e.g. the
                    // calendar became unavailable): stop + hard-delete them.
                    channels_stale.insert(cal.id.clone());
                    if let Err(err) =
                        stop_watches_for_calendar(http, watches, access, &cal.id).await
                    {
                        sync_errors.push(format!(
                            "failed to stop watch channels for calendar {}: {err}",
                            cal.id
                        ));
                    }
                    // Missing / leftover coverage after the stop is stamped by
                    // the coverage pass below (this calendar is marked stale).
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
        calendars_mutated = true;
        if let Err(err) = calendars.bump_dirty_requested(&cal.id, &now_rfc3339).await {
            sync_errors.push(format!(
                "failed to bump dirty for calendar {} ({}): {err}",
                cal.id, cal.google_calendar_id
            ));
        }

        window_fetched = true;
        // Per-calendar classifier context: the Google id is the scope key for
        // patterns; a missing taxonomy only degrades the stamp.
        let ctx = match taxonomy {
            Some(taxonomy) => taxonomy.event_ctx(&cal.google_calendar_id),
            None => EventClassifyCtx::none(),
        };
        match fetch_and_apply_window(
            http,
            calendars,
            events,
            access,
            cal,
            start_rfc3339,
            end_rfc3339,
            &now_rfc3339,
            &ctx,
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
                channels_stale.insert(cal.id.clone());
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

    // Health envelope: reuse the start-of-request calendars unless this
    // request mutated them (dirty bump / disable / window fetch / import), in
    // which case re-read so it reflects those writes. On re-read failure, fall
    // back to the in-memory snapshot. Window path must not flip
    // ready / initial_sync_complete / sync_token.
    let mut fresh = if calendars_mutated {
        match calendars.list_by_user_id(user_id).await {
            Ok(rows) => rows,
            Err(_) => cals,
        }
    } else {
        cals
    };
    // Stamp sanitized watch coverage from live channel rows so GET is accurate
    // after deploy (column defaults to `missing`). The in-memory row is always
    // stamped so the envelope is correct; D1 is written only when the value
    // changed from what is stored (best-effort), so a steady-state GET does no
    // writes. Cron / webhook paths persist coverage on their own schedule.
    if channels_available {
        let mut changed: Vec<(usize, &'static str)> = Vec::new();
        for (index, cal) in fresh.iter_mut().enumerate() {
            let relisted;
            let channels: &[WatchChannel] = if channels_stale.contains(&cal.id) {
                match watches.list_by_calendar_id(&cal.id).await {
                    Ok(rows) => {
                        relisted = rows;
                        &relisted
                    }
                    Err(err) => {
                        sync_errors.push(format!(
                            "failed to list watch channels for calendar {}: {err}",
                            cal.id
                        ));
                        continue;
                    }
                }
            } else {
                channels_by_calendar
                    .get(&cal.id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
            };
            let coverage = classify_watch_coverage(channels, now_unix).as_str();
            if cal.watch_coverage != coverage {
                cal.watch_coverage = coverage.to_string();
                changed.push((index, coverage));
            }
        }
        let persisted = futures::future::join_all(changed.iter().map(|(index, coverage)| {
            calendars.set_watch_coverage(&fresh[*index].id, coverage, &now_rfc3339)
        }))
        .await;
        for ((index, _), result) in changed.iter().zip(persisted) {
            if let Err(err) = result {
                sync_errors.push(format!(
                    "failed to persist watch coverage for calendar {}: {err}",
                    fresh[*index].id
                ));
            }
        }
    }
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
        calendars: fresh,
    })
}

/// Serve cached events after a Google token refresh failure.
///
/// No Google HTTP, no access token. On a revoked grant
/// ([`is_refresh_auth_revoked`]) stamps `authorization_required` on the
/// user's sync-enabled calendars; on [`TokenError::NoToken`] /
/// [`TokenError::NoRefreshToken`] serves cache without flipping health.
/// Stamp failures are collected in `sync_errors` and never fail the listing.
/// `source` is always `"cache"`.
pub async fn list_events_after_refresh_failure(
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    user_id: &str,
    start_rfc3339: &str,
    end_rfc3339: &str,
    now_unix: i64,
    refresh_err: &TokenError,
) -> Result<CalendarListOutput, CalendarError> {
    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);
    let mut sync_errors: Vec<String> = Vec::new();

    if is_refresh_auth_revoked(refresh_err) {
        sync_errors.extend(
            stamp_auth_revoked_for_user(calendars, user_id, now_unix, &now_rfc3339).await,
        );
    }

    // Post-stamp read: nothing mutates calendars after this, so it serves
    // the envelope as well (no second read).
    let fresh = calendars.list_by_user_id(user_id).await?;

    let cached = events
        .list_by_user_id_and_time_range(user_id, start_rfc3339, end_rfc3339)
        .await?;

    let sync = events_sync_envelope(&fresh, now_unix);

    Ok(CalendarListOutput {
        events: cached,
        sync_errors,
        sync,
        source: "cache".to_string(),
        calendars: fresh,
    })
}

/// Merge lease-miss ephemeral window rows into the D1 cache query result.
///
/// Applies the same GET projection filters as the range list SQL (cancelled +
/// overlap; all-day included). Prefers existing D1 rows on natural-key
/// collision.
fn merge_ephemeral_window_events(
    cached: &mut Vec<CalendarEvent>,
    ephemeral: Vec<CalendarEvent>,
    start_rfc3339: &str,
    end_rfc3339: &str,
) {
    for event in ephemeral {
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
