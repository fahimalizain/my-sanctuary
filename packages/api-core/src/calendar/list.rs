use super::catalog::refresh_calendar_list;
use super::sync::{events_sync_envelope, EventsSyncEnvelope};
use super::watch::{ensure_watch, is_public_https_callback, stop_watches_for_calendar};
use super::window::fetch_and_apply_window;
use super::CalendarError;
use crate::models::CalendarEvent;
use crate::oauth::HttpClient;
use crate::repo::{CalendarEventRepo, CalendarRepo, WatchChannelRepo};
use crate::time::{add_months_unix, rfc3339_to_unix_secs, unix_secs_to_rfc3339};
use crate::token::GoogleAccess;

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
