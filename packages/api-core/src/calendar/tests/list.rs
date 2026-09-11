use super::support::*;
use crate::calendar::{
    list_events, list_events_after_refresh_failure, parse_event_time_range, sync_calendar,
};
use crate::calendar::window::fetch_and_apply_window;
use crate::oauth::HttpError;
use crate::repo::CalendarEventRepo;
use crate::token::TokenError;

// ──────────────────────────────────────────
// parse_event_time_range
// ──────────────────────────────────────────

#[test]
fn default_window_is_minus_one_month_to_plus_two_months() {
    let (start, end) = parse_event_time_range(None, None, NOW_UNIX).unwrap();
    // 2023-11-14T22:13:20Z − 1 month / + 2 months.
    assert_eq!(start, "2023-10-14T22:13:20Z");
    assert_eq!(end, "2024-01-14T22:13:20Z");
}

#[test]
fn explicit_bounds_are_normalized() {
    let (start, end) = parse_event_time_range(
        Some("2026-08-01T00:00:00.500Z"),
        Some("2026-09-01T00:00:00+00:00"),
        NOW_UNIX,
    )
    .unwrap();
    assert_eq!(start, "2026-08-01T00:00:00Z", "fraction truncated");
    assert_eq!(end, "2026-09-01T00:00:00Z", "offset normalized to UTC");
}

#[test]
fn empty_bounds_fall_back_to_defaults() {
    let (start, end) = parse_event_time_range(Some(""), Some(""), NOW_UNIX).unwrap();
    assert_eq!(start, "2023-10-14T22:13:20Z");
    assert_eq!(end, "2024-01-14T22:13:20Z");
}

#[test]
fn invalid_bounds_are_rejected() {
    let err = parse_event_time_range(Some("not-a-date"), None, NOW_UNIX).unwrap_err();
    assert!(err.to_string().contains("time_min"), "{err}");

    let err = parse_event_time_range(None, Some("nope"), NOW_UNIX).unwrap_err();
    assert!(err.to_string().contains("time_max"), "{err}");

    let err = parse_event_time_range(
        Some("2026-09-01T00:00:00Z"),
        Some("2026-08-01T00:00:00Z"),
        NOW_UNIX,
    )
    .unwrap_err();
    assert_eq!(err.to_string(), "time_max must be after time_min");

    let err = parse_event_time_range(
        Some("2026-08-01T00:00:00Z"),
        Some("2026-08-01T00:00:00Z"),
        NOW_UNIX,
    )
    .unwrap_err();
    assert_eq!(err.to_string(), "time_max must be after time_min");
}


// ──────────────────────────────────────────
// list_events
// ──────────────────────────────────────────

#[test]
fn empty_calendars_imports_calendar_list_before_serving_cache() {
    let http = FakeHttp::new(vec![
        ("calendarList", 200, CALENDAR_LIST_JSON),
        ("/events", 200, r#"{"items":[],"nextSyncToken":"st-1"}"#),
    ]);
    let calendars = FakeCalendarRepo::with(vec![]);
    let events = FakeEventRepo::new();

    let watches = FakeWatchChannelRepo::new();
    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
    ))
    .unwrap();

    assert!(output.events.is_empty());
    assert!(output.sync_errors.is_empty());

    // calendarList fetched, rows upserted (sync_enabled defaults true),
    // then each imported row's event-label cache backfilled via
    // `calendars.get` (empty `event_labels` = cache miss).
    let gets = http.gets.lock().unwrap();
    assert_eq!(
        gets.len(),
        5,
        "calendarList + 2 calendars.get backfill + 2 events.list"
    );
    assert!(gets[0].contains("calendarList"), "{gets:?}");
    assert!(
        gets[1].contains("/calendars/primary%40example.com") && !gets[1].contains("/events"),
        "{gets:?}"
    );
    assert!(
        gets[2].contains("/calendars/en.usa%23holiday%40group.v.calendar.google.com")
            && !gets[2].contains("/events"),
        "{gets:?}"
    );
    let upserted = calendars.upserted.lock().unwrap();
    assert_eq!(upserted.len(), 2);
    assert!(upserted.iter().all(|cal| cal.sync_enabled));
    assert!(upserted.iter().any(|cal| cal.is_primary));
    assert_eq!(upserted[1].google_calendar_id, "en.usa#holiday@group.v.calendar.google.com");

    // Freshly imported calendars are never-initialized: Path A window
    // fetch (not replica). Encoded `#`/`@` show in the events.list URLs.
    assert!(gets[3].contains("primary%40example.com/events"), "{gets:?}");
    assert!(gets[3].contains("singleEvents=true"), "{gets:?}");
    assert!(!gets[3].contains("syncToken"), "{gets:?}");
    assert!(gets[4].contains("en.usa%23holiday%40group.v.calendar.google.com/events"), "{gets:?}");
    assert!(gets[4].contains("singleEvents=true"), "{gets:?}");
    // Window discards nextSyncToken — no record_sync_success.
    assert!(calendars.sync_states.lock().unwrap().is_empty());
    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored.len(), 2);
    assert!(stored.iter().all(|c| c.sync_token.is_empty()));
    assert!(stored.iter().all(|c| !c.initial_sync_complete));
    assert!(stored.iter().all(|c| c.dirty_requested_generation == 1));
    assert_eq!(output.source, "window");
}

#[test]
fn first_import_backfills_event_label_cache() {
    // Empty store: first contact imports calendarList, then every imported
    // row (empty `event_labels` = cache miss) is fetched via
    // `calendars.get` and the event labels are persisted.
    let http = FakeHttp::new(vec![
        ("calendarList", 200, CALENDAR_LIST_JSON),
        ("/events", 200, r#"{"items":[],"nextSyncToken":"st-1"}"#),
        (
            "/calendars/",
            200,
            r##"{"id":"ignored","labelProperties":{"eventLabels":[
                {"id":"1","backgroundColor":"#AC725E","name":null},
                {"id":"2","backgroundColor":"#d06b64"},
                {"id":"3","backgroundColor":"not-a-hex"}
            ]}}"##,
        ),
    ]);
    let calendars = FakeCalendarRepo::with(vec![]);
    let events = FakeEventRepo::new();

    let watches = FakeWatchChannelRepo::new();
    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
    ))
    .unwrap();
    assert!(output.sync_errors.is_empty(), "{:?}", output.sync_errors);

    // Both imported rows carry the cached labels: background colors are
    // canonicalized to lowercase #rrggbb when they parse, the original is
    // kept otherwise, and null `name` is ignored.
    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored.len(), 2);
    let expected = r##"[{"id":"1","backgroundColor":"#ac725e"},{"id":"2","backgroundColor":"#d06b64"},{"id":"3","backgroundColor":"not-a-hex"}]"##;
    assert!(
        stored.iter().all(|cal| cal.event_labels == expected),
        "label cache filled on every imported row: {stored:?}"
    );
    // The writes went through the dedicated set_event_labels path.
    let updates = calendars.label_updates.lock().unwrap();
    assert_eq!(updates.len(), 2);
    assert!(updates.iter().all(|(_, json)| json == &expected));
}

#[test]
fn calendar_get_without_label_properties_stores_empty_array() {
    // Holiday-style `calendars.get` body: no `labelProperties` at all.
    // The cache must read `"[]"` (fetched, no labels) — never stay empty.
    // `/events` must be listed before `/calendars/` so events.list URLs
    // (which contain both substrings) do not match the calendars.get body.
    let http = FakeHttp::new(vec![
        ("calendarList", 200, CALENDAR_LIST_JSON),
        ("/events", 200, r#"{"items":[],"nextSyncToken":"st-1"}"#),
        (
            "/calendars/",
            200,
            r#"{"id":"en.usa#holiday@group.v.calendar.google.com","summary":"Holidays","timeZone":"UTC"}"#,
        ),
    ]);
    let calendars = FakeCalendarRepo::with(vec![]);
    let events = FakeEventRepo::new();

    let watches = FakeWatchChannelRepo::new();
    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
    ))
    .unwrap();
    assert!(output.sync_errors.is_empty(), "{:?}", output.sync_errors);

    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored.len(), 2);
    assert!(
        stored.iter().all(|cal| cal.event_labels == "[]"),
        "absent labelProperties must cache as an empty array: {stored:?}"
    );
}

#[test]
fn sync_skips_calendars_get_when_label_cache_is_filled() {
    // Both variants of a filled *fresh* cache — `"[]"` (fetched, no labels)
    // and a non-empty JSON array — must skip the `calendars.get` backfill.
    // `calendar()` stamps a fresh `event_labels_updated_at`.
    let mut empty_labels = calendar("cal-1", "primary@example.com", true);
    empty_labels.event_labels = "[]".to_string();
    let mut filled_labels = calendar("cal-2", "work@example.com", true);
    filled_labels.event_labels = r##"[{"id":"1","backgroundColor":"#ac725e"}]"##.to_string();

    let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
    let calendars = FakeCalendarRepo::with(vec![empty_labels, filled_labels]);
    let events = FakeEventRepo::new();

    let rows = calendars.stored.lock().unwrap().clone();
    for cal in &rows {
        pollster::block_on(sync_calendar(
            &http, &calendars, &events, &FakeOperationRepo::new(), &access(), cal, "2023-11-14T22:13:20Z",
        ))
        .unwrap();
    }

    // events.list still ran for both calendars; no URL is the bare
    // calendar resource (which would be the backfill GET).
    let gets = http.gets.lock().unwrap();
    assert_eq!(gets.len(), 2, "one events.list per calendar: {gets:?}");
    assert!(
        gets.iter().all(|url| url.contains("/events")),
        "no bare calendars.get when the cache is filled: {gets:?}"
    );
    assert!(
        calendars.label_updates.lock().unwrap().is_empty(),
        "no label writes"
    );
}

#[test]
fn sync_refreshes_stale_label_cache() {
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.event_labels = r##"[{"id":"old","backgroundColor":"#616161"}]"##.to_string();
    cal.event_labels_updated_at = Some("2020-01-01T00:00:00Z".to_string());

    let labels_body = r##"{"labelProperties":{"eventLabels":[
        {"id":"new","backgroundColor":"#ac725e"}
    ]}}"##;
    let http = FakeHttp::new(vec![
        ("/events", 200, EVENTS_JSON),
        ("/calendars/", 200, labels_body),
    ]);
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();
    let now = "2023-11-14T22:13:20Z";

    pollster::block_on(sync_calendar(
        &http,
        &calendars,
        &events,
        &FakeOperationRepo::new(),
        &access(),
        &cal,
        now,
    ))
    .unwrap();

    let gets = http.gets.lock().unwrap();
    assert!(
        gets.iter()
            .any(|url| url.contains("/calendars/primary%40example.com") && !url.contains("/events")),
        "stale cache must trigger bare calendars.get: {gets:?}"
    );
    assert!(
        gets.iter().any(|url| url.contains("/events")),
        "events.list still runs: {gets:?}"
    );

    let expected = r##"[{"id":"new","backgroundColor":"#ac725e"}]"##;
    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].event_labels, expected);
    assert_eq!(stored[0].event_labels_updated_at.as_deref(), Some(now));
}

#[test]
fn never_synced_calendar_window_fetch_before_cache_query() {
    // `calendar()` defaults to `last_synced_at: None` — first paint uses
    // Path A (window), not Path B (replica).
    let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();

    let watches = FakeWatchChannelRepo::new();
    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
    ))
    .unwrap();

    let gets = http.gets.lock().unwrap();
    assert_eq!(gets.len(), 1);
    assert!(gets[0].contains("/calendars/primary%40example.com/events"), "{gets:?}");
    assert!(gets[0].contains("singleEvents=true"), "{gets:?}");
    assert!(gets[0].contains("orderBy=startTime"), "{gets:?}");
    assert!(gets[0].contains("maxResults=250"), "{gets:?}");
    assert!(gets[0].contains("timeMin="), "{gets:?}");
    assert!(gets[0].contains("timeMax="), "{gets:?}");
    assert!(!gets[0].contains("syncToken"), "{gets:?}");
    assert!(!gets[0].contains("singleEvents=false"), "{gets:?}");

    // Write-through under lease: both items upserted.
    let upserted = events.upserted_batch.lock().unwrap();
    assert_eq!(upserted.len(), 2);
    assert!(upserted.iter().all(|event| event.calendar_id == "cal-1"));
    assert!(upserted.iter().all(|event| event.last_synced_at == "2023-11-14T22:13:20Z"));
    assert_eq!(upserted[0].title, "Standup");
    assert_eq!(upserted[0].start_time, "2026-08-18T09:00:00Z");
    assert_eq!(upserted[0].recurrence, r#"["RRULE:FREQ=DAILY"]"#);

    // Window must not publish nextSyncToken / record_sync_success.
    assert!(calendars.sync_states.lock().unwrap().is_empty());

    let ranged = events.ranged.lock().unwrap();
    assert_eq!(*ranged, vec![("u-1".to_string(), "2026-08-01T00:00:00Z".to_string(), "2026-09-01T00:00:00Z".to_string())]);

    assert_eq!(output.events.len(), 2);
    assert!(output.sync_errors.is_empty());
    assert_eq!(output.source, "window");

    // Health still never_initialized — window does not flip ready.
    assert_eq!(output.sync.calendars.len(), 1);
    let health = &output.sync.calendars[0];
    assert_eq!(health.calendar_id, "cal-1");
    assert_eq!(
        health.state,
        crate::calendar_sync::CalendarReplicaState::NeverInitialized
    );
    assert!(!health.initial_sync_complete);
    assert!(health.error_code.is_none());
    assert_eq!(
        output.sync.status,
        crate::calendar_sync::SyncAggregateStatus::Degraded
    );

    let stored = calendars.stored.lock().unwrap();
    assert!(stored[0].sync_token.is_empty(), "window discards nextSyncToken");
    assert!(!stored[0].initial_sync_complete);
    assert_eq!(stored[0].cache_revision, 0);
    assert_eq!(stored[0].dirty_requested_generation, 1);
    assert!(stored[0].last_error_code.is_empty());
    assert!(stored[0].last_success_at.is_none());
    assert!(stored[0].last_attempt_at.is_none());
}

#[test]
fn window_empty_page_with_next_sync_token_does_not_publish() {
    // Google may return nextSyncToken on a window query; Path A throws it away.
    let http = FakeHttp::new(vec![("/events", 200, r#"{"items":[],"nextSyncToken":"st-window"}"#)]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let watches = FakeWatchChannelRepo::new();

    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
    ))
    .unwrap();

    assert!(output.sync_errors.is_empty(), "{:?}", output.sync_errors);
    assert!(output.events.is_empty());
    assert_eq!(output.source, "window");
    assert!(calendars.sync_states.lock().unwrap().is_empty());

    let stored = calendars.stored.lock().unwrap();
    assert!(stored[0].sync_token.is_empty());
    assert!(!stored[0].initial_sync_complete);
    assert_eq!(stored[0].dirty_requested_generation, 1);
    assert_eq!(
        output.sync.calendars[0].state,
        crate::calendar_sync::CalendarReplicaState::NeverInitialized
    );
}

#[test]
fn previously_synced_calendar_is_not_synced_even_if_old() {
    let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
    let mut cal = calendar("cal-1", "primary@example.com", true);
    // Days old — stale under the old 5-minute rule, but the request path
    // is cache-only once `last_synced_at` is set (ADR 0001).
    cal.last_synced_at = Some("2023-11-10T00:00:00Z".to_string());
    let calendars = FakeCalendarRepo::with(vec![cal]);
    let events = FakeEventRepo::new();

    let watches = FakeWatchChannelRepo::new();
    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
    ))
    .unwrap();

    assert!(http.gets.lock().unwrap().is_empty(), "no Google calls for a previously synced calendar");
    assert!(events.upserted_batch.lock().unwrap().is_empty());
    assert!(calendars.sync_states.lock().unwrap().is_empty(), "sync state untouched");
    assert!(output.sync_errors.is_empty());
    assert_eq!(output.source, "cache");
    assert_eq!(
        calendars.stored.lock().unwrap()[0].dirty_requested_generation,
        0,
        "cache-only must not bump dirty"
    );

    // Health envelope: old last_synced_at → degraded + stale, but compat
    // state is still ready / initial_sync_complete.
    assert_eq!(
        output.sync.status,
        crate::calendar_sync::SyncAggregateStatus::Degraded
    );
    assert_eq!(output.sync.calendars.len(), 1);
    let health = &output.sync.calendars[0];
    assert!(health.stale);
    assert_eq!(health.state, crate::calendar_sync::CalendarReplicaState::Ready);
    assert!(health.initial_sync_complete);
    assert!(health.error_code.is_none());
}

#[test]
fn mixed_initialized_and_never_init_source_is_mixed() {
    let mut ready = calendar("cal-ready", "ready@example.com", true);
    ready.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
    ready.initial_sync_complete = true;
    ready.sync_token = "tok-ready".to_string();
    let never = calendar("cal-new", "new@example.com", true);
    let http = FakeHttp::new(vec![
        (
            "new%40example.com/events",
            200,
            r#"{"items":[{"id":"n1","summary":"New","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}}],"nextSyncToken":"st-discard"}"#,
        ),
    ]);
    let calendars = FakeCalendarRepo::with(vec![ready, never]);
    let events = FakeEventRepo::new();
    let watches = FakeWatchChannelRepo::new();

    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
    ))
    .unwrap();

    assert_eq!(output.source, "mixed");
    let gets = http.gets.lock().unwrap();
    assert_eq!(gets.len(), 1, "window only for never-init: {gets:?}");
    assert!(gets[0].contains("new%40example.com"), "{gets:?}");
    assert!(gets[0].contains("singleEvents=true"), "{gets:?}");
    assert!(!gets[0].contains("syncToken"), "{gets:?}");

    let stored = calendars.stored.lock().unwrap();
    let ready_row = stored.iter().find(|c| c.id == "cal-ready").unwrap();
    let new_row = stored.iter().find(|c| c.id == "cal-new").unwrap();
    assert_eq!(ready_row.sync_token, "tok-ready");
    assert_eq!(ready_row.dirty_requested_generation, 0);
    assert!(new_row.sync_token.is_empty());
    assert_eq!(new_row.dirty_requested_generation, 1);
    assert!(!new_row.initial_sync_complete);
    assert_eq!(output.events.len(), 1);
    assert_eq!(output.events[0].google_event_id, "n1");
}

#[test]
fn list_events_returns_sync_envelope_when_event_window_is_empty() {
    // Already synced, recent last_synced_at, zero events in the window.
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.last_synced_at = Some("2023-11-14T22:00:00Z".to_string());
    let http = FakeHttp::new(vec![]);
    let calendars = FakeCalendarRepo::with(vec![cal]);
    let events = FakeEventRepo::new();

    let watches = FakeWatchChannelRepo::new();
    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
    ))
    .unwrap();

    assert!(output.events.is_empty());
    assert!(http.gets.lock().unwrap().is_empty());
    assert_eq!(output.source, "cache");
    assert_eq!(output.sync.calendars.len(), 1);
    assert_eq!(output.sync.calendars[0].calendar_id, "cal-1");
    assert_eq!(
        output.sync.calendars[0].state,
        crate::calendar_sync::CalendarReplicaState::Ready
    );
    assert!(!output.sync.calendars[0].stale);
    assert_eq!(
        output.sync.status,
        crate::calendar_sync::SyncAggregateStatus::Ready
    );
}

#[test]
fn list_events_sync_envelope_never_leaks_sync_token() {
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.sync_token = "secret-sync-token-xyz".to_string();
    cal.last_error_code = "storage_transient".to_string();
    cal.sync_status = "retrying".to_string();
    cal.last_success_at = Some("2023-11-10T00:00:00Z".to_string());
    cal.last_synced_at = Some("2023-11-10T00:00:00Z".to_string());
    cal.initial_sync_complete = true;
    cal.lease_owner = "lease-secret-should-not-leak".to_string();

    let http = FakeHttp::new(vec![]);
    let calendars = FakeCalendarRepo::with(vec![cal]);
    let events = FakeEventRepo::new();

    let watches = FakeWatchChannelRepo::new();
    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
    ))
    .unwrap();

    assert_eq!(
        output.sync.calendars[0].error_code.as_deref(),
        Some("storage_transient")
    );
    assert_eq!(
        output.sync.calendars[0].state,
        crate::calendar_sync::CalendarReplicaState::Retrying
    );

    let json = serde_json::to_string(&output.sync).unwrap();
    assert!(json.contains("storage_transient"), "{json}");
    assert!(!json.contains("secret-sync-token-xyz"), "{json}");
    assert!(!json.contains("sync_token"), "{json}");
    assert!(!json.contains("lease-secret-should-not-leak"), "{json}");
}

#[test]
fn sync_disabled_calendar_is_not_synced() {
    let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", false)]);
    let events = FakeEventRepo::new();

    let watches = FakeWatchChannelRepo::new();
    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
    ))
    .unwrap();

    assert!(http.gets.lock().unwrap().is_empty());
    assert!(output.events.is_empty());
    assert!(output.sync_errors.is_empty());
}

#[test]
fn events_list_404_disables_sync_but_serves_cache() {
    let http = FakeHttp::new(vec![("/events", 404, r#"{"error":"not found"}"#)]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "holidays", true)]);
    let events = FakeEventRepo::new();

    let watches = FakeWatchChannelRepo::new();
    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
    ))
    .unwrap();

    assert_eq!(
        *calendars.disabled.lock().unwrap(),
        vec![("cal-1".to_string(), false)]
    );
    assert_eq!(output.sync_errors.len(), 1);
    assert!(output.sync_errors[0].contains("404"), "{}", output.sync_errors[0]);
    assert!(output.events.is_empty(), "cache still served");
    // Window path still ran (and failed) → source is window; dirty was bumped.
    assert_eq!(output.source, "window");

    let stored = calendars.stored.lock().unwrap();
    assert!(!stored[0].sync_enabled);
    // Window path does not record_sync_failure — disable alone drives health.
    assert!(stored[0].sync_token.is_empty());
    assert_eq!(stored[0].dirty_requested_generation, 1);
    drop(stored);
    assert_eq!(
        output.sync.calendars[0].state,
        crate::calendar_sync::CalendarReplicaState::Disabled
    );
}

#[test]
fn window_410_is_error_without_merge_full_or_token_write() {
    // Window never sends syncToken; 410 is a plain error (no merge-full loop).
    let http = FakeHttp::new(vec![("/events", 410, "")]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();

    let watches = FakeWatchChannelRepo::new();
    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
    ))
    .unwrap();

    assert_eq!(http.gets.lock().unwrap().len(), 1, "no 410 retry on window");
    assert_eq!(output.sync_errors.len(), 1);
    assert!(output.sync_errors[0].contains("410"), "{}", output.sync_errors[0]);
    assert!(calendars.sync_states.lock().unwrap().is_empty());
    assert!(calendars.stored.lock().unwrap()[0].sync_token.is_empty());
    assert_eq!(
        calendars.stored.lock().unwrap()[0].dirty_requested_generation,
        1
    );
    assert_eq!(output.source, "window");
}

#[test]
fn window_follows_next_page_token_without_publishing_sync_token() {
    let page_one = r#"{"items":[{"id":"p1","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}}],"nextPageToken":"tok-2"}"#;
    let page_two = r#"{"items":[{"id":"p2","start":{"dateTime":"2026-08-18T10:00:00Z"},"end":{"dateTime":"2026-08-18T10:30:00Z"}}],"nextSyncToken":"st-page"}"#;
    let http = FakeHttp::new(vec![
        ("pageToken=tok-2", 200, page_two),
        ("/events", 200, page_one),
    ]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();

    let watches = FakeWatchChannelRepo::new();
    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
    ))
    .unwrap();

    let gets = http.gets.lock().unwrap();
    assert_eq!(gets.len(), 2);
    assert!(gets[0].contains("singleEvents=true"), "{gets:?}");
    assert!(gets[1].contains("pageToken=tok-2"), "{gets:?}");
    assert!(gets.iter().all(|u| !u.contains("syncToken")), "{gets:?}");
    assert_eq!(events.upserted_batch.lock().unwrap().len(), 2);
    assert_eq!(output.events.len(), 2);
    assert!(output.sync_errors.is_empty(), "{:?}", output.sync_errors);
    assert!(calendars.sync_states.lock().unwrap().is_empty());
    assert!(calendars.stored.lock().unwrap()[0].sync_token.is_empty());
    assert_eq!(
        output.sync.calendars[0].state,
        crate::calendar_sync::CalendarReplicaState::NeverInitialized
    );
    assert_eq!(output.source, "window");
}

#[test]
fn window_poison_on_one_calendar_does_not_block_sibling() {
    // list_events Path A: invalid JSON on A must not block B's window.
    // Neither calendar publishes a replica token / becomes ready.
    let cal_a = calendar("cal-a", "a@example.com", true);
    let cal_b = calendar("cal-b", "b@example.com", true);
    let http = FakeHttp::new(vec![
        ("a%40example.com/events", 200, "not-json{{{"),
        (
            "b%40example.com/events",
            200,
            r#"{"items":[{"id":"b1","summary":"B","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}}],"nextSyncToken":"st-b"}"#,
        ),
    ]);
    let calendars = FakeCalendarRepo::with(vec![cal_a, cal_b]);
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

    assert_eq!(output.sync_errors.len(), 1, "{:?}", output.sync_errors);
    assert!(
        output.sync_errors[0].contains("cal-a") || output.sync_errors[0].contains("a@"),
        "{:?}",
        output.sync_errors
    );
    assert_eq!(output.events.len(), 1);
    assert_eq!(output.events[0].google_event_id, "b1");
    assert_eq!(output.source, "window");

    let stored = calendars.stored.lock().unwrap();
    let a = stored.iter().find(|c| c.id == "cal-a").unwrap();
    let b = stored.iter().find(|c| c.id == "cal-b").unwrap();
    // Window path does not record_sync_failure — health stays never_init.
    assert!(a.sync_token.is_empty());
    assert!(!a.initial_sync_complete);
    assert_eq!(a.dirty_requested_generation, 1);
    assert!(b.sync_token.is_empty(), "window nextSyncToken discarded");
    assert!(!b.initial_sync_complete);
    assert_eq!(b.dirty_requested_generation, 1);
    assert_eq!(
        output.sync.calendars.iter().find(|c| c.calendar_id == "cal-a").unwrap().state,
        crate::calendar_sync::CalendarReplicaState::NeverInitialized
    );
    assert_eq!(
        output.sync.calendars.iter().find(|c| c.calendar_id == "cal-b").unwrap().state,
        crate::calendar_sync::CalendarReplicaState::NeverInitialized
    );
}

#[test]
fn window_cancelled_event_delete_failure_does_not_advance_sync_token() {
    let body = r#"{"items":[
        {"id": "cancelled", "status": "cancelled",
         "start": {"dateTime": "2026-08-18T09:00:00Z"}, "end": {"dateTime": "2026-08-18T09:30:00Z"}},
        {"id": "real", "summary": "Real",
         "start": {"dateTime": "2026-08-18T09:00:00Z"}, "end": {"dateTime": "2026-08-18T09:30:00Z"}}
    ], "nextSyncToken": "st-9"}"#;
    let http = FakeHttp::new(vec![("/events", 200, body)]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    *events.fail_delete.lock().unwrap() = true;

    let watches = FakeWatchChannelRepo::new();
    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
    ))
    .unwrap();

    // Fenced delete path records only after a successful delete; fail_delete
    // returns Err without pushing. Attempt still failed and aborted apply.
    assert!(
        events.deleted_by_google_event_id.lock().unwrap().is_empty(),
        "failed fenced delete is not recorded: {:?}",
        events.deleted_by_google_event_id.lock().unwrap()
    );
    // Delete failure aborts the window apply: no upsert of the living
    // sibling, token must not advance (window never publishes anyway).
    assert!(events.upserted_batch.lock().unwrap().is_empty());
    assert!(calendars.sync_states.lock().unwrap().is_empty());
    assert_eq!(output.sync_errors.len(), 1);
    assert!(
        output.sync_errors[0].contains("cache delete failed"),
        "{}",
        output.sync_errors[0]
    );

    let stored = calendars.stored.lock().unwrap();
    assert!(stored[0].sync_token.is_empty(), "token never advanced");
    assert!(!stored[0].initial_sync_complete);
    assert_eq!(stored[0].dirty_requested_generation, 1);
    // Window path does not record_sync_failure.
    assert!(stored[0].last_error_code.is_empty());
    assert_eq!(
        output.sync.calendars[0].state,
        crate::calendar_sync::CalendarReplicaState::NeverInitialized
    );
    assert_eq!(output.source, "window");
}

#[test]
fn window_steal_before_first_upsert_writes_nothing() {
    // Two-page window; steal lease on the 1st fenced apply (page-1 upserts).
    // Loser must not write page-1 or page-2 and must not publish a token.
    let page_one = r#"{"items":[{"id":"p1","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}}],"nextPageToken":"tok-2"}"#;
    let page_two = r#"{"items":[{"id":"p2","start":{"dateTime":"2026-08-18T10:00:00Z"},"end":{"dateTime":"2026-08-18T10:30:00Z"}}],"nextSyncToken":"st-page"}"#;
    let cal = calendar("cal-1", "primary@example.com", true);
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();
    events.gate_applies_on(&calendars);
    *events.inject_lease_before_fenced_apply.lock().unwrap() =
        Some((1, "thief".to_string(), Some("2099-01-01T00:00:00Z".to_string())));
    let http = FakeHttp::new(vec![
        ("pageToken=tok-2", 200, page_two),
        ("/events", 200, page_one),
    ]);

    let result = pollster::block_on(fetch_and_apply_window(
        &http,
        &calendars,
        &events,
        &access(),
        &cal,
        "2026-08-01T00:00:00Z",
        "2026-09-01T00:00:00Z",
        "2023-11-14T22:13:20Z",
    ));
    assert!(result.is_ok(), "{result:?}");
    assert!(
        result.unwrap().is_empty(),
        "mid-window steal stops write-through with empty return"
    );
    assert!(
        events.upserted_batch.lock().unwrap().is_empty(),
        "no page-1 or page-2 upserts: {:?}",
        events.upserted_batch.lock().unwrap()
    );
    assert!(calendars.stored.lock().unwrap()[0].sync_token.is_empty());
    assert!(calendars.sync_states.lock().unwrap().is_empty());
    assert!(!calendars.stored.lock().unwrap()[0].initial_sync_complete);
}

#[test]
fn window_expire_before_tombstone_leaves_row_living() {
    // Cancelled + living on one page; expire lease on the 1st fenced apply
    // (the tombstone). Cancelled row stays living; sibling not upserted.
    let body = r#"{"items":[
        {"id":"cancelled","status":"cancelled",
         "start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}},
        {"id":"living","summary":"Keep",
         "start":{"dateTime":"2026-08-18T10:00:00Z"},"end":{"dateTime":"2026-08-18T10:30:00Z"}}
    ],"nextSyncToken":"st-win"}"#;
    let cal = calendar("cal-1", "primary@example.com", true);
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();
    events
        .stored
        .lock()
        .unwrap()
        .push(seeded_event("evt-cancel", "cal-1", "cancelled", ""));
    events.gate_applies_on(&calendars);
    *events.inject_lease_before_fenced_apply.lock().unwrap() =
        Some((1, "loser".to_string(), Some("2020-01-01T00:00:00Z".to_string())));
    let http = FakeHttp::new(vec![("/events", 200, body)]);

    let result = pollster::block_on(fetch_and_apply_window(
        &http,
        &calendars,
        &events,
        &access(),
        &cal,
        "2026-08-01T00:00:00Z",
        "2026-09-01T00:00:00Z",
        "2023-11-14T22:13:20Z",
    ));
    assert!(result.is_ok(), "{result:?}");
    assert!(result.unwrap().is_empty());
    let stored_events = events.stored.lock().unwrap();
    let cancelled = stored_events
        .iter()
        .find(|e| e.google_event_id == "cancelled")
        .expect("cancelled seed");
    assert!(
        cancelled.deleted_at.is_none(),
        "tombstone must not apply after lease expire"
    );
    assert!(
        events.upserted_batch.lock().unwrap().is_empty(),
        "living sibling must not upsert: {:?}",
        events.upserted_batch.lock().unwrap()
    );
    assert!(
        events.deleted_by_google_event_id.lock().unwrap().is_empty(),
        "delete must not be recorded: {:?}",
        events.deleted_by_google_event_id.lock().unwrap()
    );
    assert!(calendars.stored.lock().unwrap()[0].sync_token.is_empty());
}

#[test]
fn window_winner_after_expired_loser_can_write_through() {
    // After expire inject fails the loser, clear the hook and re-run: expired
    // foreign lease is stealable; winner write-throughs but never publishes.
    let body = r#"{"items":[
        {"id":"cancelled","status":"cancelled",
         "start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}},
        {"id":"living","summary":"Keep",
         "start":{"dateTime":"2026-08-18T10:00:00Z"},"end":{"dateTime":"2026-08-18T10:30:00Z"}}
    ],"nextSyncToken":"st-win"}"#;
    let cal = calendar("cal-1", "primary@example.com", true);
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();
    events
        .stored
        .lock()
        .unwrap()
        .push(seeded_event("evt-cancel", "cal-1", "cancelled", ""));
    events.gate_applies_on(&calendars);
    *events.inject_lease_before_fenced_apply.lock().unwrap() =
        Some((1, "loser".to_string(), Some("2020-01-01T00:00:00Z".to_string())));

    let http_lose = FakeHttp::new(vec![("/events", 200, body)]);
    let lose = pollster::block_on(fetch_and_apply_window(
        &http_lose,
        &calendars,
        &events,
        &access(),
        &cal,
        "2026-08-01T00:00:00Z",
        "2026-09-01T00:00:00Z",
        "2023-11-14T22:13:20Z",
    ));
    assert!(lose.is_ok(), "{lose:?}");
    assert!(lose.unwrap().is_empty());
    assert!(events.upserted_batch.lock().unwrap().is_empty());
    assert!(
        events
            .stored
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.google_event_id == "cancelled")
            .unwrap()
            .deleted_at
            .is_none()
    );

    // Clear inject; lease remains expired foreign — stealable on next window.
    *events.inject_lease_before_fenced_apply.lock().unwrap() = None;
    let http_win = FakeHttp::new(vec![("/events", 200, body)]);
    let win = pollster::block_on(fetch_and_apply_window(
        &http_win,
        &calendars,
        &events,
        &access(),
        &cal,
        "2026-08-01T00:00:00Z",
        "2026-09-01T00:00:00Z",
        "2023-11-14T22:13:20Z",
    ));
    assert!(win.is_ok(), "{win:?}");
    assert!(win.unwrap().is_empty(), "write-through returns empty vec");

    let stored_events = events.stored.lock().unwrap();
    let cancelled = stored_events
        .iter()
        .find(|e| e.google_event_id == "cancelled")
        .expect("cancelled");
    assert!(
        cancelled.deleted_at.is_some(),
        "winner tombstones cancelled row"
    );
    assert!(
        events
            .upserted_batch
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.google_event_id == "living"),
        "living id in upserted_batch: {:?}",
        events.upserted_batch.lock().unwrap()
    );
    assert!(
        calendars.stored.lock().unwrap()[0].sync_token.is_empty(),
        "window never publishes sync_token"
    );
    assert!(!calendars.stored.lock().unwrap()[0].initial_sync_complete);
    assert!(calendars.sync_states.lock().unwrap().is_empty());
}

#[test]
fn sync_error_does_not_fail_the_whole_listing() {
    let http = FakeHttp::new(vec![("/events", 500, "")]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();

    let watches = FakeWatchChannelRepo::new();
    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
    ))
    .unwrap();

    assert_eq!(output.sync_errors.len(), 1);
    assert!(output.sync_errors[0].contains("500"), "{}", output.sync_errors[0]);
    assert!(output.events.is_empty());
    assert!(calendars.disabled.lock().unwrap().is_empty(), "500 is not a 404");
    assert_eq!(output.source, "window");
    assert_eq!(
        calendars.stored.lock().unwrap()[0].dirty_requested_generation,
        1
    );
}

// ──────────────────────────────────────────
// list_events_after_refresh_failure
// ──────────────────────────────────────────

fn ready_synced_calendar(id: &str, google_cal_id: &str, sync_enabled: bool) -> crate::models::GoogleCalendar {
    let mut cal = calendar(id, google_cal_id, sync_enabled);
    cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.initial_sync_complete = true;
    cal.sync_status = "ready".to_string();
    cal.sync_token = "cursor-secret-xyz".to_string();
    cal
}

#[test]
fn refresh_failure_revoked_grant_serves_cache_and_stamps_auth_required() {
    let enabled = ready_synced_calendar("cal-1", "primary@example.com", true);
    let disabled = ready_synced_calendar("cal-disabled", "disabled@example.com", false);
    let success_before = enabled.last_success_at.clone();
    let token_before = enabled.sync_token.clone();

    let calendars = FakeCalendarRepo::with(vec![enabled, disabled]);
    let events = FakeEventRepo::new();
    events
        .stored
        .lock()
        .unwrap()
        .push(seeded_event("evt-local-1", "cal-1", "Standup", ""));

    let refresh_err = TokenError::Http(HttpError::Message(
        "POST https://oauth2.googleapis.com/token returned 400".into(),
    ));
    let output = pollster::block_on(list_events_after_refresh_failure(
        &calendars,
        &events,
        "u-1",
        "2026-08-01T00:00:00Z",
        "2026-09-01T00:00:00Z",
        NOW_UNIX,
        &refresh_err,
    ))
    .unwrap();

    assert_eq!(output.source, "cache");
    assert_eq!(output.events.len(), 1);
    assert_eq!(output.events[0].id, "evt-local-1");
    assert_eq!(output.events[0].title, "Standup");
    assert_eq!(
        output.sync.status,
        crate::calendar_sync::SyncAggregateStatus::AuthorizationRequired
    );
    let enabled_view = output
        .sync
        .calendars
        .iter()
        .find(|c| c.calendar_id == "cal-1")
        .expect("enabled calendar in envelope");
    assert_eq!(
        enabled_view.state,
        crate::calendar_sync::CalendarReplicaState::AuthorizationRequired
    );
    assert_eq!(enabled_view.error_code.as_deref(), Some("auth_revoked"));

    let stored = calendars.stored.lock().unwrap();
    let cal1 = stored.iter().find(|c| c.id == "cal-1").unwrap();
    assert_eq!(cal1.sync_status, "authorization_required");
    assert_eq!(cal1.last_error_code, "auth_revoked");
    assert_eq!(cal1.sync_token, token_before, "cursor must not move");
    assert_eq!(cal1.last_success_at, success_before, "success must not move");
    let cal_disabled = stored.iter().find(|c| c.id == "cal-disabled").unwrap();
    assert_ne!(
        cal_disabled.sync_status, "authorization_required",
        "disabled calendars are not stamped"
    );
    assert_eq!(cal_disabled.sync_status, "ready");

    let json = serde_json::to_string(&output.sync).unwrap();
    assert!(!json.contains("cursor-secret-xyz"), "{json}");
    assert!(!json.contains("invalid_grant"), "{json}");
    assert!(!json.contains("access_token"), "{json}");
    assert!(!json.contains("raw_json"), "{json}");
    assert!(!json.contains("ya29."), "{json}");
}

#[test]
fn refresh_failure_no_token_does_not_stamp() {
    let cal = ready_synced_calendar("cal-1", "primary@example.com", true);
    let calendars = FakeCalendarRepo::with(vec![cal]);
    let events = FakeEventRepo::new();
    events
        .stored
        .lock()
        .unwrap()
        .push(seeded_event("evt-local-1", "cal-1", "Standup", ""));

    let output = pollster::block_on(list_events_after_refresh_failure(
        &calendars,
        &events,
        "u-1",
        "2026-08-01T00:00:00Z",
        "2026-09-01T00:00:00Z",
        NOW_UNIX,
        &TokenError::NoToken,
    ))
    .unwrap();

    assert_eq!(output.source, "cache");
    assert_eq!(output.events.len(), 1);
    assert_ne!(
        output.sync.status,
        crate::calendar_sync::SyncAggregateStatus::AuthorizationRequired
    );
    assert_eq!(
        output.sync.calendars[0].state,
        crate::calendar_sync::CalendarReplicaState::Ready
    );
    assert_ne!(
        calendars.stored.lock().unwrap()[0].sync_status,
        "authorization_required"
    );
    assert_eq!(calendars.stored.lock().unwrap()[0].sync_status, "ready");
}

#[test]
fn refresh_failure_no_refresh_token_does_not_stamp() {
    let cal = ready_synced_calendar("cal-1", "primary@example.com", true);
    let calendars = FakeCalendarRepo::with(vec![cal]);
    let events = FakeEventRepo::new();
    events
        .stored
        .lock()
        .unwrap()
        .push(seeded_event("evt-local-1", "cal-1", "Standup", ""));

    let output = pollster::block_on(list_events_after_refresh_failure(
        &calendars,
        &events,
        "u-1",
        "2026-08-01T00:00:00Z",
        "2026-09-01T00:00:00Z",
        NOW_UNIX,
        &TokenError::NoRefreshToken,
    ))
    .unwrap();

    assert_eq!(output.source, "cache");
    assert_eq!(output.events.len(), 1);
    assert_ne!(
        output.sync.status,
        crate::calendar_sync::SyncAggregateStatus::AuthorizationRequired
    );
    assert_eq!(
        calendars.stored.lock().unwrap()[0].sync_status,
        "ready"
    );
}

#[test]
fn refresh_failure_revoked_grant_empty_cache_still_ok() {
    let cal = ready_synced_calendar("cal-1", "primary@example.com", true);
    let calendars = FakeCalendarRepo::with(vec![cal]);
    let events = FakeEventRepo::new();

    let refresh_err = TokenError::Http(HttpError::Message("invalid_grant".into()));
    let output = pollster::block_on(list_events_after_refresh_failure(
        &calendars,
        &events,
        "u-1",
        "2026-08-01T00:00:00Z",
        "2026-09-01T00:00:00Z",
        NOW_UNIX,
        &refresh_err,
    ))
    .unwrap();

    assert!(output.events.is_empty());
    assert_eq!(output.source, "cache");
    assert_eq!(
        output.sync.status,
        crate::calendar_sync::SyncAggregateStatus::AuthorizationRequired
    );
    assert_eq!(
        output.sync.calendars[0].state,
        crate::calendar_sync::CalendarReplicaState::AuthorizationRequired
    );
    assert_eq!(
        output.sync.calendars[0].error_code.as_deref(),
        Some("auth_revoked")
    );
}

// ──────────────────────────────────────────
// Living + sync-enabled parent filter (issue #54)
// ──────────────────────────────────────────

#[test]
fn list_events_omits_events_from_disabled_and_soft_deleted_parents() {
    let live = ready_synced_calendar("cal-1", "primary@example.com", true);
    let disabled = ready_synced_calendar("cal-disabled", "disabled@example.com", false);
    let mut deleted = ready_synced_calendar("cal-deleted", "deleted@example.com", true);
    deleted.deleted_at = Some("2023-11-14T20:00:00Z".to_string());

    let calendars = FakeCalendarRepo::with(vec![live.clone(), disabled.clone(), deleted.clone()]);
    let events = FakeEventRepo::new();
    *events.parent_calendars.lock().unwrap() = vec![live, disabled, deleted];
    events.stored.lock().unwrap().extend([
        seeded_event("evt-live", "cal-1", "live", ""),
        seeded_event("evt-off", "cal-disabled", "hidden-disabled", ""),
        seeded_event("evt-del", "cal-deleted", "hidden-deleted", ""),
    ]);

    let http = FakeHttp::new(vec![]);
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

    assert_eq!(output.events.len(), 1, "{:?}", output.events);
    assert_eq!(output.events[0].id, "evt-live");

    let disabled_view = output
        .sync
        .calendars
        .iter()
        .find(|c| c.calendar_id == "cal-disabled")
        .expect("disabled calendar still in envelope");
    assert_eq!(
        disabled_view.state,
        crate::calendar_sync::CalendarReplicaState::Disabled
    );
    assert!(
        output
            .sync
            .calendars
            .iter()
            .all(|c| c.calendar_id != "cal-deleted"),
        "soft-deleted parent must not appear in envelope: {:?}",
        output.sync.calendars
    );
    assert!(http.gets.lock().unwrap().is_empty(), "cache-only ready calendars");
}

#[test]
fn list_by_user_id_and_time_range_hides_non_living_parents() {
    let live = ready_synced_calendar("cal-1", "primary@example.com", true);
    let disabled = ready_synced_calendar("cal-disabled", "disabled@example.com", false);
    let mut deleted = ready_synced_calendar("cal-deleted", "deleted@example.com", true);
    deleted.deleted_at = Some("2023-11-14T20:00:00Z".to_string());
    let other_user = calendar_for_user("other-user", "cal-other", "other@example.com", true);

    let events = FakeEventRepo::new();
    *events.parent_calendars.lock().unwrap() =
        vec![live, disabled, deleted, other_user];
    events.stored.lock().unwrap().extend([
        seeded_event("evt-live", "cal-1", "live", ""),
        seeded_event("evt-off", "cal-disabled", "hidden-disabled", ""),
        seeded_event("evt-del", "cal-deleted", "hidden-deleted", ""),
        seeded_event("evt-other", "cal-other", "other-user-event", ""),
    ]);

    let rows = pollster::block_on(events.list_by_user_id_and_time_range(
        "u-1",
        "2026-08-01T00:00:00Z",
        "2026-09-01T00:00:00Z",
    ))
    .unwrap();

    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].id, "evt-live");
}

#[test]
fn list_running_by_user_id_hides_disabled_parent() {
    let live = ready_synced_calendar("cal-1", "primary@example.com", true);
    let disabled = ready_synced_calendar("cal-disabled", "disabled@example.com", false);

    let events = FakeEventRepo::new();
    *events.parent_calendars.lock().unwrap() = vec![live, disabled];
    events.stored.lock().unwrap().extend([
        seeded_event("evt-live", "cal-1", "running-live", "task-1"),
        seeded_event("evt-off", "cal-disabled", "running-off", "task-2"),
    ]);

    // seeded_event windows are 2026-08-18T09:00–09:30.
    let rows = pollster::block_on(events.list_running_by_user_id("u-1", "2026-08-18T09:15:00Z"))
        .unwrap();

    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].id, "evt-live");
    assert_eq!(rows[0].task_id, "task-1");
}
