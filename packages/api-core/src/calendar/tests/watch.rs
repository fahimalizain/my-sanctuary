use super::support::*;
use crate::calendar::sync::{refresh_watch_coverage, WatchCoverage};
use crate::calendar::watch::WatchChannelResponse;
use crate::calendar::{
    is_public_https_callback, list_events, renew_watch_if_needed, stop_watches_for_calendar,
};
use crate::models::WatchChannel;
use crate::repo::CalendarRepo;
use crate::time::unix_secs_to_rfc3339;
use crate::WATCH_RENEW_HORIZON_SECS;

// ──────────────────────────────────────────
// is_public_https_callback
// ──────────────────────────────────────────

#[test]
fn is_public_https_callback_accepts_only_public_https_urls() {
    assert!(is_public_https_callback(CALLBACK_URL));
    assert!(is_public_https_callback("https://sanctuary.example.com/notify"));
    assert!(is_public_https_callback("https://SANCTUARY.EXAMPLE.COM/notify"));

    assert!(!is_public_https_callback(""), "empty is false");
    assert!(!is_public_https_callback("not a url"), "unparseable is false");
    assert!(
        !is_public_https_callback("http://my-sanctuary.fahimalizain.com/api/calendar/notifications"),
        "http scheme is false"
    );
    assert!(!is_public_https_callback("https://localhost/api/calendar/notifications"));
    assert!(!is_public_https_callback("https://LOCALHOST:8443/x"), "host case-insensitive");
    assert!(!is_public_https_callback("https://127.0.0.1:8787/api/calendar/notifications"));
    assert!(!is_public_https_callback("https://[::1]/api/calendar/notifications"));
}


// ──────────────────────────────────────────
// WatchChannelResponse deserialization
// ──────────────────────────────────────────

#[test]
fn watch_channel_response_accepts_numeric_expiration() {
    let channel: WatchChannelResponse =
        serde_json::from_str(r#"{"resourceId":"r","expiration":1710000000000}"#).unwrap();
    assert_eq!(channel.expiration_millis, Some(1710000000000));
}

#[test]
fn watch_channel_response_accepts_string_expiration() {
    let channel: WatchChannelResponse =
        serde_json::from_str(r#"{"resourceId":"r","expiration":"1787628641000"}"#).unwrap();
    assert_eq!(channel.expiration_millis, Some(1787628641000));
}

#[test]
fn watch_channel_response_defaults_missing_expiration_to_none() {
    let channel: WatchChannelResponse =
        serde_json::from_str(r#"{"resourceId":"r"}"#).unwrap();
    assert_eq!(channel.expiration_millis, None);
}

#[test]
fn watch_channel_response_maps_null_expiration_to_none() {
    let channel: WatchChannelResponse =
        serde_json::from_str(r#"{"resourceId":"r","expiration":null}"#).unwrap();
    assert_eq!(channel.expiration_millis, None);
}

#[test]
fn watch_channel_response_rejects_unparseable_expiration_string() {
    let err = serde_json::from_str::<WatchChannelResponse>(
        r#"{"resourceId":"r","expiration":"not-a-number"}"#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("not-a-number"), "{err}");
}

#[test]
fn list_events_skips_watch_when_callback_is_none() {
    // FakeHttp has no `/watch` route: if list_events tried to watch, the
    // fake would panic with "no route for …/events/watch".
    let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let watches = FakeWatchChannelRepo::new();

    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
    ))
    .unwrap();

    assert!(http.posts.lock().unwrap().is_empty(), "no watch POST without a callback");
    assert!(watches.inserted.lock().unwrap().is_empty());
    // The first-paint window still runs with no callback configured.
    let gets = http.gets.lock().unwrap();
    assert_eq!(gets.len(), 1);
    assert!(gets[0].contains("singleEvents=true"), "{gets:?}");
    assert_eq!(output.events.len(), 2);
    assert!(output.sync_errors.is_empty());
    assert_eq!(output.source, "window");
}

#[test]
fn list_events_skips_watch_when_callback_is_localhost() {
    let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let watches = FakeWatchChannelRepo::new();

    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX,
        Some("http://localhost:8787/api/calendar/notifications"),
    ))
    .unwrap();

    assert!(http.posts.lock().unwrap().is_empty(), "no watch POST for a localhost callback");
    assert!(watches.inserted.lock().unwrap().is_empty());
    assert_eq!(output.events.len(), 2, "first-paint window unaffected");
    assert!(output.sync_errors.is_empty());
    assert_eq!(output.source, "window");
}

#[test]
fn never_synced_calendar_is_watched_then_window_fetched() {
    // `/events/watch` must precede `/events`: the substring matcher would
    // otherwise swallow the watch POST URL.
    let http = FakeHttp::new(vec![
        ("/events/watch", 200, WATCH_JSON),
        ("/events", 200, EVENTS_JSON),
    ]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let watches = FakeWatchChannelRepo::new();

    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX,
        Some(CALLBACK_URL),
    ))
    .unwrap();

    // Watch POST: web_hook with the configured callback address.
    let posts = http.posts.lock().unwrap();
    assert_eq!(posts.len(), 1, "one watch POST");
    let (url, body) = posts.first().unwrap().clone();
    assert!(url.contains("/calendars/primary%40example.com/events/watch"), "{url}");
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["type"], "web_hook");
    assert_eq!(body["address"], CALLBACK_URL);
    assert_eq!(body["id"].as_str().unwrap().len(), 36, "uuid-shaped channel id");
    assert_eq!(body["token"].as_str().unwrap().len(), 64, "64 hex chars");

    // Channel row inserted with Google's resourceId + converted expiration
    // (1710000000000 ms == 1710000000 s == 2024-03-09T16:00:00Z).
    let inserted = watches.inserted.lock().unwrap();
    assert_eq!(inserted.len(), 1);
    assert_eq!(inserted[0].calendar_id, "cal-1");
    assert_eq!(inserted[0].resource_id, "resource-123");
    assert_eq!(inserted[0].expiration, "2024-03-09T16:00:00Z");

    // First-paint window (not replica) populated the cache; token not published.
    let gets = http.gets.lock().unwrap();
    assert_eq!(gets.len(), 1);
    assert!(gets[0].contains("singleEvents=true"), "{gets:?}");
    assert!(!gets[0].contains("syncToken"), "{gets:?}");
    assert_eq!(events.upserted_batch.lock().unwrap().len(), 2);
    assert_eq!(output.events.len(), 2);
    assert!(output.sync_errors.is_empty());
    assert!(calendars.stored.lock().unwrap()[0].sync_token.is_empty());
    assert_eq!(output.source, "window");
}

#[test]
fn never_synced_calendar_is_watched_then_window_fetched_with_string_expiration() {
    // Production shape: Google sends `expiration` as a JSON string
    // (discovery type string/int64). This is the path that used to fail
    // with "invalid type: string ..., expected i64" and orphan every
    // channel; the row must be inserted exactly like the numeric case.
    let http = FakeHttp::new(vec![
        ("/events/watch", 200, WATCH_JSON_STRING_EXPIRATION),
        ("/events", 200, EVENTS_JSON),
    ]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let watches = FakeWatchChannelRepo::new();

    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX,
        Some(CALLBACK_URL),
    ))
    .unwrap();

    // Channel row inserted with Google's resourceId + converted expiration
    // ("1710000000000" ms == 1710000000 s == 2024-03-09T16:00:00Z — same as
    // the numeric fixture).
    let inserted = watches.inserted.lock().unwrap();
    assert_eq!(inserted.len(), 1);
    assert_eq!(inserted[0].calendar_id, "cal-1");
    assert_eq!(inserted[0].resource_id, "resource-123");
    assert_eq!(inserted[0].expiration, "2024-03-09T16:00:00Z");

    // First-paint window still ran and populated the cache; token not published.
    assert_eq!(events.upserted_batch.lock().unwrap().len(), 2);
    assert_eq!(output.events.len(), 2);
    assert!(output.sync_errors.is_empty());
    assert!(calendars.stored.lock().unwrap()[0].sync_token.is_empty());
    assert_eq!(output.source, "window");
}

#[test]
fn already_unexpired_channel_does_not_rewatch() {
    let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    // Future expiration: 2023-11-21T22:13:20Z > NOW_UNIX instant.
    let watches =
        FakeWatchChannelRepo::with(vec![watch_channel("cal-1", "2023-11-21T22:13:20Z")]);

    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX,
        Some(CALLBACK_URL),
    ))
    .unwrap();

    assert!(http.posts.lock().unwrap().is_empty(), "unexpired channel must not be rewatched");
    assert!(watches.inserted.lock().unwrap().is_empty());
    let gets = http.gets.lock().unwrap();
    assert_eq!(gets.len(), 1, "window still runs");
    assert!(gets[0].contains("singleEvents=true"), "{gets:?}");
    assert!(output.sync_errors.is_empty());
    assert_eq!(output.source, "window");
}

#[test]
fn watch_404_disables_sync_and_does_not_list_events() {
    let http = FakeHttp::new(vec![("/events/watch", 404, r#"{"error":"not found"}"#)]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let watches = FakeWatchChannelRepo::new();

    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX,
        Some(CALLBACK_URL),
    ))
    .unwrap();

    assert_eq!(
        *calendars.disabled.lock().unwrap(),
        vec![("cal-1".to_string(), false)]
    );
    assert!(http.gets.lock().unwrap().is_empty(), "no events.list after watch 404");
    assert!(events.upserted_batch.lock().unwrap().is_empty());
    assert_eq!(output.sync_errors.len(), 1);
    assert!(output.sync_errors[0].contains("404"), "{}", output.sync_errors[0]);
}

#[test]
fn events_list_404_stops_existing_watches() {
    let http = FakeHttp::new(vec![
        ("/events", 404, r#"{"error":"not found"}"#),
        ("/channels/stop", 200, "{}"),
    ]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    // Channel preloaded unexpired: ensure_watch short-circuits, so the
    // only POST is channels.stop from the events.list 404 path.
    let watches =
        FakeWatchChannelRepo::with(vec![watch_channel("cal-1", "2023-11-21T22:13:20Z")]);

    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX,
        Some(CALLBACK_URL),
    ))
    .unwrap();

    // events.list 404 → sync disabled (as before)…
    assert_eq!(
        *calendars.disabled.lock().unwrap(),
        vec![("cal-1".to_string(), false)]
    );
    // …and the stored channel is stopped and hard-deleted.
    let posts = http.posts.lock().unwrap();
    assert_eq!(posts.len(), 1);
    let (url, body) = posts.first().unwrap().clone();
    assert!(url.contains("/channels/stop"), "{url}");
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["id"], "minted-id");
    assert_eq!(body["resourceId"], "resource-1");
    assert_eq!(
        *watches.deleted_by_calendar_id.lock().unwrap(),
        vec!["cal-1".to_string()]
    );
    assert!(watches.stored.lock().unwrap().is_empty(), "rows hard-deleted");
    assert_eq!(output.sync_errors.len(), 1);
    assert!(output.sync_errors[0].contains("404"), "{}", output.sync_errors[0]);
}


// ──────────────────────────────────────────
// renew_watch_if_needed / run_fallback_cron
// ──────────────────────────────────────────

#[test]
fn renew_skips_when_channel_expires_after_horizon() {
    // No /events/watch route: a watch POST would panic "no route".
    let http = FakeHttp::new(vec![]);
    let cal = calendar("cal-1", "primary@example.com", true);
    // 2023-11-16T00:00:00Z > horizon (now + 24h == 2023-11-15T22:13:20Z).
    let watches =
        FakeWatchChannelRepo::with(vec![watch_channel("cal-1", "2023-11-16T00:00:00Z")]);

    let renewed = pollster::block_on(renew_watch_if_needed(
        &http, &watches, &access(), &cal, CALLBACK_URL, NOW_UNIX,
    ))
    .unwrap();

    assert!(!renewed, "existing coverage spans the horizon");
    assert!(http.posts.lock().unwrap().is_empty(), "no watch POST");
    assert!(watches.inserted.lock().unwrap().is_empty());
    assert!(watches.deleted_by_id.lock().unwrap().is_empty());
}

#[test]
fn renew_creates_watch_and_stops_old_when_expiring_within_horizon() {
    let http = FakeHttp::new(vec![
        ("/events/watch", 200, WATCH_JSON),
        ("/channels/stop", 200, "{}"),
    ]);
    let cal = calendar("cal-1", "primary@example.com", true);
    // 1 hour out: unexpired (ensure_watch would short-circuit) but inside
    // the 24-hour horizon — the cron must renew.
    let watches =
        FakeWatchChannelRepo::with(vec![watch_channel("cal-1", "2023-11-14T23:13:20Z")]);

    let renewed = pollster::block_on(renew_watch_if_needed(
        &http, &watches, &access(), &cal, CALLBACK_URL, NOW_UNIX,
    ))
    .unwrap();

    assert!(renewed, "new channel minted");
    // New channel inserted with the same body/minting contract as
    // ensure_watch: Google's resourceId + converted expiration.
    let inserted = watches.inserted.lock().unwrap();
    assert_eq!(inserted.len(), 1);
    assert_eq!(inserted[0].calendar_id, "cal-1");
    assert_eq!(inserted[0].resource_id, "resource-123");
    assert_eq!(inserted[0].expiration, "2024-03-09T16:00:00Z");

    // watch POST, then a channels.stop POST with the OLD channel's id.
    let posts = http.posts.lock().unwrap();
    assert_eq!(posts.len(), 2);
    assert!(posts[0].0.contains("/events/watch"), "{}", posts[0].0);
    assert!(posts[1].0.contains("/channels/stop"), "{}", posts[1].0);
    let stop_body: serde_json::Value = serde_json::from_str(&posts[1].1).unwrap();
    assert_eq!(stop_body["id"], "minted-id");
    assert_eq!(stop_body["resourceId"], "resource-1");

    // Old row hard-deleted by id only — never delete_by_calendar_id
    // (that would kill the new row). The new row remains stored.
    assert_eq!(
        *watches.deleted_by_id.lock().unwrap(),
        vec!["wc-1".to_string()]
    );
    assert!(watches.deleted_by_calendar_id.lock().unwrap().is_empty());
    let stored = watches.stored.lock().unwrap();
    assert_eq!(stored.len(), 1, "new row only");
    assert_eq!(stored[0].channel_id, inserted[0].channel_id);
}

// ──────────────────────────────────────────
// watch_coverage persist (issue #55 slice 2)
// ──────────────────────────────────────────

#[test]
fn list_events_stamps_covered_when_horizon_channel_present() {
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.initial_sync_complete = true;
    cal.last_synced_at = Some(unix_secs_to_rfc3339(NOW_UNIX - 60));
    cal.last_success_at = Some(unix_secs_to_rfc3339(NOW_UNIX - 60));
    cal.sync_status = "ready".to_string();

    let covered_exp = unix_secs_to_rfc3339(NOW_UNIX + WATCH_RENEW_HORIZON_SECS + 60);
    let mut channel = watch_channel("cal-1", &covered_exp);
    channel.token = "secret-channel-token".to_string();
    channel.resource_id = "res-secret".to_string();

    let http = FakeHttp::new(vec![]);
    let calendars = FakeCalendarRepo::with(vec![cal]);
    let events = FakeEventRepo::new();
    let watches = FakeWatchChannelRepo::with(vec![channel]);

    let output = pollster::block_on(list_events(
        &http,
        &calendars,
        &events,
        &watches,
        &access(),
        "u-1",
        "2026-08-01T00:00:00Z",
        "2026-09-01T00:00:00Z",
        NOW_UNIX,
        None, // no watch I/O — coverage comes from stored channels
    ))
    .unwrap();

    assert_eq!(output.sync.calendars.len(), 1);
    assert_eq!(
        output.sync.calendars[0].watch_coverage,
        WatchCoverage::Covered
    );
    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].watch_coverage, "covered");

    let json = serde_json::to_string(&output.sync).unwrap();
    assert!(json.contains("\"watch_coverage\""), "{json}");
    assert!(json.contains("covered"), "{json}");
    assert!(!json.contains("secret-channel-token"), "{json}");
    assert!(!json.contains("res-secret"), "{json}");
    assert!(!json.contains("resource_id"), "{json}");
    assert!(!json.contains("channel_id"), "{json}");
}

#[test]
fn list_events_stamps_missing_when_no_channels() {
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.initial_sync_complete = true;
    cal.last_synced_at = Some(unix_secs_to_rfc3339(NOW_UNIX - 60));
    cal.last_success_at = Some(unix_secs_to_rfc3339(NOW_UNIX - 60));
    cal.sync_status = "ready".to_string();

    let http = FakeHttp::new(vec![]);
    let calendars = FakeCalendarRepo::with(vec![cal]);
    let events = FakeEventRepo::new();
    let watches = FakeWatchChannelRepo::new();

    let output = pollster::block_on(list_events(
        &http,
        &calendars,
        &events,
        &watches,
        &access(),
        "u-1",
        "2026-08-01T00:00:00Z",
        "2026-09-01T00:00:00Z",
        NOW_UNIX,
        None,
    ))
    .unwrap();

    assert_eq!(
        output.sync.calendars[0].watch_coverage,
        WatchCoverage::Missing
    );
    assert_eq!(
        calendars.stored.lock().unwrap()[0].watch_coverage,
        "missing"
    );
    // Missing watches must not degrade a ready replica.
    assert_eq!(
        output.sync.status.as_str(),
        "ready",
        "aggregate must ignore watch_coverage"
    );
}

#[test]
fn renew_noop_persists_covered_via_refresh_helper() {
    // renew returns Ok(false) when horizon already covered; cron still refreshes.
    let http = FakeHttp::new(vec![]);
    let cal = calendar("cal-1", "primary@example.com", true);
    let covered_exp = unix_secs_to_rfc3339(NOW_UNIX + WATCH_RENEW_HORIZON_SECS + 120);
    let watches =
        FakeWatchChannelRepo::with(vec![watch_channel("cal-1", &covered_exp)]);
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);

    let renewed = pollster::block_on(renew_watch_if_needed(
        &http,
        &watches,
        &access(),
        &cal,
        CALLBACK_URL,
        NOW_UNIX,
    ))
    .unwrap();
    assert!(!renewed);

    let coverage = pollster::block_on(refresh_watch_coverage(
        &calendars,
        &watches,
        "cal-1",
        NOW_UNIX,
        &unix_secs_to_rfc3339(NOW_UNIX),
    ))
    .unwrap();
    assert_eq!(coverage, WatchCoverage::Covered);
    assert_eq!(
        calendars.stored.lock().unwrap()[0].watch_coverage,
        "covered"
    );
}

#[test]
fn stop_watches_then_refresh_persists_missing() {
    let http = FakeHttp::new(vec![("/channels/stop", 200, "{}")]);
    let cal = calendar("cal-1", "primary@example.com", true);
    let covered_exp = unix_secs_to_rfc3339(NOW_UNIX + WATCH_RENEW_HORIZON_SECS + 60);
    let mut channel = watch_channel("cal-1", &covered_exp);
    channel.token = "secret-channel-token".to_string();
    channel.resource_id = "res-secret".to_string();
    let watches = FakeWatchChannelRepo::with(vec![channel]);
    let calendars = FakeCalendarRepo::with(vec![cal]);

    // Pre-stamp covered so we can observe the flip to missing.
    pollster::block_on(calendars.set_watch_coverage(
        "cal-1",
        "covered",
        &unix_secs_to_rfc3339(NOW_UNIX),
    ))
    .unwrap();

    pollster::block_on(stop_watches_for_calendar(
        &http,
        &watches,
        &access(),
        "cal-1",
    ))
    .unwrap();
    assert!(watches.stored.lock().unwrap().is_empty());

    let coverage = pollster::block_on(refresh_watch_coverage(
        &calendars,
        &watches,
        "cal-1",
        NOW_UNIX,
        &unix_secs_to_rfc3339(NOW_UNIX),
    ))
    .unwrap();
    assert_eq!(coverage, WatchCoverage::Missing);
    assert_eq!(
        calendars.stored.lock().unwrap()[0].watch_coverage,
        "missing"
    );
}

#[test]
fn list_events_envelope_json_never_leaks_channel_secrets() {
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.initial_sync_complete = true;
    cal.last_synced_at = Some(unix_secs_to_rfc3339(NOW_UNIX - 60));
    cal.last_success_at = Some(unix_secs_to_rfc3339(NOW_UNIX - 60));
    cal.sync_status = "ready".to_string();
    cal.sync_token = "secret-sync-token".to_string();
    cal.lease_owner = "lease-secret".to_string();

    let covered_exp = unix_secs_to_rfc3339(NOW_UNIX + WATCH_RENEW_HORIZON_SECS + 60);
    let channel = WatchChannel {
        id: "wc-1".to_string(),
        calendar_id: "cal-1".to_string(),
        channel_id: "ch-secret-id".to_string(),
        resource_id: "res-secret".to_string(),
        token: "secret-channel-token".to_string(),
        expiration: covered_exp,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
    };

    let http = FakeHttp::new(vec![]);
    let calendars = FakeCalendarRepo::with(vec![cal]);
    let events = FakeEventRepo::new();
    let watches = FakeWatchChannelRepo::with(vec![channel]);

    let output = pollster::block_on(list_events(
        &http,
        &calendars,
        &events,
        &watches,
        &access(),
        "u-1",
        "2026-08-01T00:00:00Z",
        "2026-09-01T00:00:00Z",
        NOW_UNIX,
        None,
    ))
    .unwrap();

    let json = serde_json::to_string(&output.sync).unwrap();
    for secret in [
        "secret-channel-token",
        "res-secret",
        "ch-secret-id",
        "secret-sync-token",
        "lease-secret",
        "access_token",
        "raw_json",
    ] {
        assert!(!json.contains(secret), "leaked {secret}: {json}");
    }
    assert!(!json.contains("\"token\""), "{json}");
    assert!(!json.contains("resource_id"), "{json}");
    assert!(!json.contains("channel_id"), "{json}");
    assert!(!json.contains("sync_token"), "{json}");
    assert!(!json.contains("lease_owner"), "{json}");
    assert!(json.contains("\"watch_coverage\""), "{json}");
}
