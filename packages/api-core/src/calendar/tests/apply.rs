use super::support::*;
use crate::calendar::{list_events, sync_calendar};
use crate::models::NewCalendarEvent;
use crate::repo::CalendarEventRepo;

#[test]
fn all_day_and_no_time_events_are_upserted_and_all_day_in_projection() {
    // Window write-through reuses classify_replica_item; token not advanced.
    let body = r#"{"items":[
        {"id": "all-day", "summary": "Holiday",
         "start": {"date": "2026-08-01"}, "end": {"date": "2026-08-02"}},
        {"id": "no-time", "summary": "No times at all"},
        {"id": "real", "summary": "Real",
         "start": {"dateTime": "2026-08-18T09:00:00Z"}, "end": {"dateTime": "2026-08-18T09:30:00Z"}}
    ], "nextSyncToken": "st-9"}"#;
    let http = FakeHttp::new(vec![("/events", 200, body)]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();

    let watches = FakeWatchChannelRepo::new();
    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
    ))
    .unwrap();

    let upserted = events.upserted_batch.lock().unwrap();
    assert_eq!(upserted.len(), 3, "all-day, no-time, and timed are upserted");
    let all_day = upserted.iter().find(|e| e.google_event_id == "all-day").unwrap();
    assert!(all_day.is_all_day);
    assert_eq!(all_day.start_time, "2026-08-01T00:00:00Z");
    assert_eq!(all_day.end_time, "2026-08-02T00:00:00Z");
    let no_time = upserted.iter().find(|e| e.google_event_id == "no-time").unwrap();
    assert!(!no_time.is_all_day);
    assert!(no_time.start_time.is_empty());
    assert!(
        events.deleted_by_google_event_id.lock().unwrap().is_empty(),
        "all-day/no-time are not deleted"
    );
    // GET range includes all-day + timed; no-time has empty bounds so no overlap.
    assert_eq!(output.events.len(), 2);
    let ids: Vec<_> = output.events.iter().map(|e| e.google_event_id.as_str()).collect();
    assert!(ids.contains(&"all-day"), "{ids:?}");
    assert!(ids.contains(&"real"), "{ids:?}");
    assert!(calendars.sync_states.lock().unwrap().is_empty());
    assert!(calendars.stored.lock().unwrap()[0].sync_token.is_empty());
    assert_eq!(output.source, "window");
}

#[test]
fn cancelled_events_are_soft_deleted() {
    let body = r#"{"items":[
        {"id": "cancelled", "status": "cancelled",
         "start": {"dateTime": "2026-08-18T09:00:00Z"}, "end": {"dateTime": "2026-08-18T09:30:00Z"}},
        {"id": "real", "summary": "Real",
         "start": {"dateTime": "2026-08-18T09:00:00Z"}, "end": {"dateTime": "2026-08-18T09:30:00Z"}}
    ], "nextSyncToken": "st-9"}"#;
    let http = FakeHttp::new(vec![("/events", 200, body)]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();

    let watches = FakeWatchChannelRepo::new();
    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
    ))
    .unwrap();

    // Ordinary cancelled events (no recurringEventId) are soft-deleted.
    assert_eq!(
        *events.deleted_by_google_event_id.lock().unwrap(),
        vec![("cal-1".to_string(), "cancelled".to_string())]
    );
    let upserted = events.upserted_batch.lock().unwrap();
    assert_eq!(upserted.len(), 1, "only the timed, non-cancelled event");
    assert_eq!(upserted[0].google_event_id, "real");
    assert_eq!(output.events.len(), 1);
    // Window does not advance the replica token.
    assert!(calendars.sync_states.lock().unwrap().is_empty());
    assert!(calendars.stored.lock().unwrap()[0].sync_token.is_empty());
    assert_eq!(output.source, "window");
}

#[test]
fn cancelled_exception_is_upserted_not_deleted_and_out_of_projection() {
    let body = r#"{"items":[
        {"id": "exc-1", "status": "cancelled", "recurringEventId": "master-1",
         "originalStartTime": {"dateTime": "2026-08-20T15:00:00Z"}},
        {"id": "real", "summary": "Real",
         "start": {"dateTime": "2026-08-18T09:00:00Z"}, "end": {"dateTime": "2026-08-18T09:30:00Z"}}
    ], "nextSyncToken": "st-exc"}"#;
    let http = FakeHttp::new(vec![("/events", 200, body)]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();

    let watches = FakeWatchChannelRepo::new();
    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
    ))
    .unwrap();

    assert!(
        events.deleted_by_google_event_id.lock().unwrap().is_empty(),
        "cancelled exceptions must not be soft-deleted"
    );
    let upserted = events.upserted_batch.lock().unwrap();
    assert_eq!(upserted.len(), 2);
    let exc = upserted.iter().find(|e| e.google_event_id == "exc-1").unwrap();
    assert_eq!(exc.status, "cancelled");
    assert_eq!(exc.recurring_event_id, "master-1");
    assert_eq!(exc.start_time, "2026-08-20T15:00:00Z");
    // Projection: only the living timed event.
    assert_eq!(output.events.len(), 1);
    assert_eq!(output.events[0].google_event_id, "real");
    assert!(calendars.sync_states.lock().unwrap().is_empty());
    assert!(calendars.stored.lock().unwrap()[0].sync_token.is_empty());
    assert_eq!(output.source, "window");
}

#[test]
fn natural_key_upsert_returns_persisted_id() {
    let events = FakeEventRepo::new();
    let mut row = NewCalendarEvent {
        calendar_id: "cal-1".into(),
        google_event_id: "g-1".into(),
        google_etag: "e1".into(),
        google_updated_at: "2026-08-17T10:00:00Z".into(),
        last_synced_at: "2026-08-17T12:00:00Z".into(),
        title: "First".into(),
        description: String::new(),
        start_time: "2026-08-18T09:00:00Z".into(),
        end_time: "2026-08-18T09:30:00Z".into(),
        recurrence: String::new(),
        task_id: String::new(),
        ical_uid: "uid".into(),
        sequence: 0,
        status: "confirmed".into(),
        recurring_event_id: String::new(),
        original_start: String::new(),
        start_time_zone: String::new(),
        end_time_zone: String::new(),
        is_all_day: false,
        raw_json: "{}".into(),
        category_id: None,
        preserve_category: false,
        is_self_organized: true,
        event_type: String::new(),
    };
    let id1 = pollster::block_on(events.upsert(row.clone(), "2026-08-17T12:00:00Z")).unwrap();
    row.title = "Second".into();
    row.sequence = 1;
    let id2 = pollster::block_on(events.upsert(row, "2026-08-17T13:00:00Z")).unwrap();
    assert_eq!(id1, id2, "second upsert must return the persisted id");
    let stored = events.stored.lock().unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id, id1);
    assert_eq!(stored[0].title, "Second");
    assert_eq!(stored[0].sequence, 1);
    assert!(stored[0].deleted_at.is_none());
}

#[test]
fn upsert_after_delete_clears_deleted_at() {
    let events = FakeEventRepo::new();
    let row = NewCalendarEvent {
        calendar_id: "cal-1".into(),
        google_event_id: "g-1".into(),
        google_etag: "e1".into(),
        google_updated_at: "2026-08-17T10:00:00Z".into(),
        last_synced_at: "2026-08-17T12:00:00Z".into(),
        title: "Live".into(),
        description: String::new(),
        start_time: "2026-08-18T09:00:00Z".into(),
        end_time: "2026-08-18T09:30:00Z".into(),
        recurrence: String::new(),
        task_id: String::new(),
        ical_uid: String::new(),
        sequence: 0,
        status: "confirmed".into(),
        recurring_event_id: String::new(),
        original_start: String::new(),
        start_time_zone: String::new(),
        end_time_zone: String::new(),
        is_all_day: false,
        raw_json: String::new(),
        category_id: None,
        preserve_category: false,
        is_self_organized: true,
        event_type: String::new(),
    };
    let id = pollster::block_on(events.upsert(row.clone(), "2026-08-17T12:00:00Z")).unwrap();
    pollster::block_on(events.delete_by_google_event_id("cal-1", "g-1", "2026-08-17T12:30:00Z"))
        .unwrap();
    assert!(
        events.stored.lock().unwrap()[0].deleted_at.is_some(),
        "soft-deleted"
    );
    let id2 = pollster::block_on(events.upsert(row, "2026-08-17T13:00:00Z")).unwrap();
    assert_eq!(id, id2);
    let stored = events.stored.lock().unwrap();
    assert_eq!(stored.len(), 1);
    assert!(stored[0].deleted_at.is_none(), "upsert restores deleted_at");
    assert_eq!(stored[0].title, "Live");
}

// ──────────────────────────────────────────
// Write-time leaf category stamp (slice 1)
// ──────────────────────────────────────────

/// A full replica walk with a real taxonomy stamps the leaf category on the
/// upserted row; a retitle re-stamps (overwrites) it.
#[test]
fn replica_walk_stamps_and_replaces_the_leaf_category() {
    let body = r#"{"items":[
        {"id": "work-1", "summary": "Work",
         "organizer": {"self": true}, "eventType": "focusTime",
         "start": {"dateTime": "2026-08-18T09:00:00Z"}, "end": {"dateTime": "2026-08-18T09:30:00Z"}}
    ],"nextSyncToken":"st-1"}"#;
    let retitled = r#"{"items":[
        {"id": "work-1", "summary": "Lunch",
         "start": {"dateTime": "2026-08-18T09:00:00Z"}, "end": {"dateTime": "2026-08-18T09:30:00Z"}}
    ],"nextSyncToken":"st-2"}"#;
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "work@x.com", true)]);
    let events = FakeEventRepo::new();
    let categories = StubTaskCategoryRepo::with_pattern("cat-work", "^Work$", 0);

    let http = FakeHttp::new(vec![("/events", 200, body)]);
    let cal = calendars.stored.lock().unwrap()[0].clone();
    pollster::block_on(sync_calendar(
        &http,
        &calendars,
        &events,
        &FakeOperationRepo::new(),
        &access(),
        &cal,
        "2023-11-14T22:13:20Z",
        &categories,
    ))
    .expect("walk ok");

    let stored = events.stored.lock().unwrap().clone();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].category_id.as_deref(), Some("cat-work"));
    assert!(stored[0].is_self_organized, "organizer.self: true");
    assert_eq!(stored[0].event_type, "focusTime");
    drop(stored);

    // Retitle: the walk re-stamps to NULL (no pattern matches "Lunch").
    let http = FakeHttp::new(vec![("/events", 200, retitled)]);
    let cal = calendars.stored.lock().unwrap()[0].clone();
    pollster::block_on(sync_calendar(
        &http,
        &calendars,
        &events,
        &FakeOperationRepo::new(),
        &access(),
        &cal,
        "2023-11-14T22:14:00Z",
        &categories,
    ))
    .expect("second walk ok");

    let stored = events.stored.lock().unwrap().clone();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].title, "Lunch");
    assert_eq!(
        stored[0].category_id, None,
        "an unclassified retitle replaces the stale stamp"
    );
    assert!(
        stored[0].is_self_organized,
        "organizer absent → organized (Google-owned overwrite)"
    );
    assert_eq!(stored[0].event_type, "");
}

/// A taxonomy-load **failure** must not erase a stamp: the walk falls back to
/// [`EventClassifyCtx::preserve`], so the stored `category_id` survives the
/// upsert (a later reclassify fills any genuinely missing stamp).
#[test]
fn taxonomy_failure_keeps_the_existing_category() {
    let body = r#"{"items":[
        {"id": "work-1", "summary": "Work",
         "start": {"dateTime": "2026-08-18T09:00:00Z"}, "end": {"dateTime": "2026-08-18T09:30:00Z"}}
    ],"nextSyncToken":"st-1"}"#;
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "work@x.com", true)]);
    let events = FakeEventRepo::new();
    let categories = StubTaskCategoryRepo::with_pattern("cat-work", "^Work$", 0);

    let http = FakeHttp::new(vec![("/events", 200, body)]);
    let cal = calendars.stored.lock().unwrap()[0].clone();
    pollster::block_on(sync_calendar(
        &http, &calendars, &events, &FakeOperationRepo::new(), &access(), &cal,
        "2023-11-14T22:13:20Z", &categories,
    ))
    .expect("stamped walk ok");
    assert_eq!(
        events.stored.lock().unwrap()[0].category_id.as_deref(),
        Some("cat-work")
    );

    // The same walk with a taxonomy read that fails: the stamp stays.
    let http = FakeHttp::new(vec![("/events", 200, body)]);
    let cal = calendars.stored.lock().unwrap()[0].clone();
    pollster::block_on(sync_calendar(
        &http, &calendars, &events, &FakeOperationRepo::new(), &access(), &cal,
        "2023-11-14T22:14:00Z", &FAILING_TAXONOMY,
    ))
    .expect("failing-taxonomy walk ok");
    assert_eq!(
        events.stored.lock().unwrap()[0].category_id.as_deref(),
        Some("cat-work"),
        "a failed taxonomy load keeps the stored stamp"
    );
}

/// A **successful** load whose patterns match nothing still writes `NULL`:
/// the title really is unclassified, and keeping a stale stamp would be a lie.
#[test]
fn a_successful_taxonomy_with_no_match_still_clears_the_stamp() {
    let body = r#"{"items":[
        {"id": "work-1", "summary": "Work",
         "start": {"dateTime": "2026-08-18T09:00:00Z"}, "end": {"dateTime": "2026-08-18T09:30:00Z"}}
    ],"nextSyncToken":"st-1"}"#;
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "work@x.com", true)]);
    let events = FakeEventRepo::new();
    let categories = StubTaskCategoryRepo::with_pattern("cat-work", "^Work$", 0);

    let http = FakeHttp::new(vec![("/events", 200, body)]);
    let cal = calendars.stored.lock().unwrap()[0].clone();
    pollster::block_on(sync_calendar(
        &http, &calendars, &events, &FakeOperationRepo::new(), &access(), &cal,
        "2023-11-14T22:13:20Z", &categories,
    ))
    .expect("stamped walk ok");
    assert_eq!(
        events.stored.lock().unwrap()[0].category_id.as_deref(),
        Some("cat-work")
    );

    let http = FakeHttp::new(vec![("/events", 200, body)]);
    let cal = calendars.stored.lock().unwrap()[0].clone();
    pollster::block_on(sync_calendar(
        &http, &calendars, &events, &FakeOperationRepo::new(), &access(), &cal,
        "2023-11-14T22:14:00Z", &NO_TAXONOMY,
    ))
    .expect("empty-taxonomy walk ok");
    assert_eq!(
        events.stored.lock().unwrap()[0].category_id, None,
        "an empty (but successful) taxonomy stamps unclassified"
    );
}

/// Window write-through (first paint) stamps from the taxonomy the caller
/// already loaded for paint — `list_events_with_snapshot` is the worker's
/// route, and `None` (a failed taxonomy load) degrades to no stamp.
#[test]
fn window_write_through_stamps_from_the_paint_taxonomy() {
    let body = r#"{"items":[
        {"id": "work-1", "summary": "Work",
         "start": {"dateTime": "2026-08-18T09:00:00Z"}, "end": {"dateTime": "2026-08-18T09:30:00Z"}}
    ]}"#;
    let http = FakeHttp::new(vec![("/events", 200, body)]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "work@x.com", true)]);
    let events = FakeEventRepo::new();
    let watches = FakeWatchChannelRepo::new();
    let categories = StubTaskCategoryRepo::with_pattern("cat-work", "^Work$", 0);
    let taxonomy =
        pollster::block_on(crate::calendar_color::load_classify_taxonomy(&categories, "u-1"))
            .unwrap();
    let snapshot =
        pollster::block_on(crate::calendar::load_list_snapshot(&calendars, &watches, "u-1"))
            .unwrap();

    pollster::block_on(crate::calendar::list_events_with_snapshot(
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
        snapshot,
        Some(&taxonomy),
    ))
    .expect("window listing ok");

    let stored = events.stored.lock().unwrap().clone();
    assert_eq!(stored.len(), 1, "write-through upserted the window row");
    assert_eq!(
        stored[0].category_id.as_deref(),
        Some("cat-work"),
        "the window row is stamped from the paint taxonomy"
    );
}
