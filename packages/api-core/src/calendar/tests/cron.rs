use super::support::*;
use crate::calendar::{replica_due, run_fallback_cron};
use crate::repo::CalendarRepo;
use crate::time::unix_secs_to_rfc3339;

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
        &http, &calendars, &events, &FakeOperationRepo::new(), &watches, &tokens, &oauth, None, NOW_UNIX,
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
        &http, &calendars, &events, &FakeOperationRepo::new(), &watches, &tokens, &oauth, None, NOW_UNIX,
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
        &http, &calendars, &events, &FakeOperationRepo::new(), &watches, &tokens, &oauth, None, NOW_UNIX,
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
        &FakeOperationRepo::new(), &watches,
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
        &http, &calendars, &events, &FakeOperationRepo::new(), &watches, &tokens, &oauth, None, NOW_UNIX,
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
        &http, &calendars, &events, &FakeOperationRepo::new(), &watches, &tokens, &oauth, None, NOW_UNIX,
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
        &http, &calendars, &events, &FakeOperationRepo::new(), &watches, &tokens, &oauth, Some(CALLBACK_URL), NOW_UNIX,
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
        &http, &calendars, &events, &FakeOperationRepo::new(), &watches, &tokens, &oauth,
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
        &http, &calendars, &events, &FakeOperationRepo::new(), &watches, &tokens, &oauth, None, NOW_UNIX,
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
    cal.next_retry_at = Some(future_retry.clone());
    assert!(!replica_due(&cal, now), "future backoff blocks even stale");

    // next_retry_at in the past + last_success 1m ago + not dirty → due
    cal.next_retry_at = Some(past_retry);
    cal.last_success_at = Some(one_min_ago.clone());
    assert!(replica_due(&cal, now), "expired backoff retries");

    // full_sync_requested + last_success 1m ago + not dirty → due
    cal.next_retry_at = None;
    cal.full_sync_requested = true;
    cal.last_success_at = Some(one_min_ago.clone());
    assert!(replica_due(&cal, now), "full_sync_requested forces due");

    // full_sync_requested + future next_retry_at + fresh last_success + not dirty
    // → due (reseed wins over backoff; isolate death mid-410 recovery)
    cal.next_retry_at = Some(future_retry.clone());
    cal.last_success_at = Some(one_min_ago.clone());
    cal.dirty_requested_generation = 0;
    cal.dirty_applied_generation = 0;
    cal.full_sync_requested = true;
    assert!(
        replica_due(&cal, now),
        "full_sync_requested wins over future backoff"
    );

    // full_sync_requested + authorization_required → still not due
    cal.sync_status = "authorization_required".to_string();
    assert!(
        !replica_due(&cal, now),
        "auth_required still hard-skips even with full_sync_requested"
    );

    // full_sync_requested + freeBusyReader → still not due
    cal.sync_status = String::new();
    cal.access_role = "freeBusyReader".to_string();
    assert!(
        !replica_due(&cal, now),
        "freeBusyReader still hard-skips even with full_sync_requested"
    );

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
        &http, &calendars, &events, &FakeOperationRepo::new(), &watches, &tokens, &oauth, None, NOW_UNIX,
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
        &http, &calendars, &events, &FakeOperationRepo::new(), &watches, &tokens, &oauth, None, NOW_UNIX,
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
        &http, &calendars, &events, &FakeOperationRepo::new(), &watches, &tokens, &oauth, None, NOW_UNIX,
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

#[test]
fn cron_poison_on_one_calendar_siblings_still_sync() {
    // cal-a: due, invalid page JSON → mapping_poison + quarantine + degraded
    // cal-b: due, valid EVENTS_JSON → publishes; complete coverage, no quarantine
    let http = FakeHttp::new(vec![
        ("primary%40example.com/events", 200, "not-json{{{"),
        ("secondary%40example.com/events", 200, EVENTS_JSON),
    ]);
    let mut cal_a = calendar("cal-a", "primary@example.com", true);
    cal_a.last_success_at = Some("2023-11-14T22:12:20Z".to_string());
    cal_a.dirty_requested_generation = 2;
    cal_a.dirty_applied_generation = 0;
    let mut cal_b = calendar("cal-b", "secondary@example.com", true);
    cal_b.last_success_at = Some("2023-11-14T22:12:20Z".to_string());
    cal_b.dirty_requested_generation = 1;
    cal_b.dirty_applied_generation = 0;
    let calendars = FakeCalendarRepo::with(vec![cal_a, cal_b]);
    let events = FakeEventRepo::new();
    let watches = FakeWatchChannelRepo::new();
    let tokens = FakeTokenRepo::with(vec![fresh_token("u-1", "at-1")]);
    let oauth = oauth_config();

    let report = pollster::block_on(run_fallback_cron(
        &http,
        &calendars,
        &events,
        &FakeOperationRepo::new(),
        &watches,
        &tokens,
        &oauth,
        None,
        NOW_UNIX,
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
    assert!(a.sync_token.is_empty(), "poison must not advance token");
    assert_eq!(a.last_error_code, "mapping_poison");
    assert_eq!(a.sync_status, "retrying");
    assert_eq!(a.event_coverage, "degraded");
    assert_eq!(a.dirty_applied_generation, 0);

    let b = stored.iter().find(|c| c.id == "cal-b").unwrap();
    assert_eq!(b.sync_token, "st-9");
    assert_eq!(b.event_coverage, "complete");
    assert!(b.last_error_code.is_empty());
    assert_eq!(b.dirty_applied_generation, 1);
    drop(stored);

    let q = events.quarantine.lock().unwrap();
    assert_eq!(q.len(), 1);
    assert_eq!(q[0].calendar_id, "cal-a");
    assert_eq!(q[0].phase, "replica_page");
    assert_eq!(q[0].error_class, "mapping_poison");
    assert!(q[0].replay_payload.contains("not-json{{{"));
    assert!(
        !q.iter().any(|r| r.calendar_id == "cal-b"),
        "sibling must not be quarantined"
    );

    // No hot-loop on the poison calendar: one events.list GET for primary.
    let gets = http.gets.lock().unwrap();
    let primary_gets: Vec<_> = gets
        .iter()
        .filter(|u| u.contains("primary") && u.contains("/events"))
        .collect();
    assert_eq!(
        primary_gets.len(),
        1,
        "poison must not hot-loop: {gets:?}"
    );
}
