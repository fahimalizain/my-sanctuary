use super::support::*;
use crate::calendar::list_events;
use crate::models::NewCalendarEvent;
use crate::repo::CalendarEventRepo;

#[test]
fn all_day_and_no_time_events_are_upserted_out_of_projection() {
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
    // GET projection only surfaces the timed living event.
    assert_eq!(output.events.len(), 1);
    assert_eq!(output.events[0].google_event_id, "real");
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
