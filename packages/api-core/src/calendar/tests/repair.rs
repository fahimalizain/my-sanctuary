use super::support::*;
use crate::calendar::repair_inflight_operations;
use crate::models::{
    CalendarEventOperation, OP_STATUS_CACHE_APPLIED, OP_STATUS_FAILED, OP_STATUS_GOOGLE_COMMITTED,
    OP_STATUS_PENDING, OP_VERB_INSERT, OP_VERB_MOVE,
};
use crate::repo::CalendarEventRepo;

fn journal_op(
    id: &str,
    verb: &str,
    status: &str,
    google_event_id: &str,
    local_event_id: &str,
) -> CalendarEventOperation {
    CalendarEventOperation {
        id: id.to_string(),
        user_id: "u-1".to_string(),
        calendar_id: "cal-1".to_string(),
        local_event_id: local_event_id.to_string(),
        google_event_id: google_event_id.to_string(),
        verb: verb.to_string(),
        payload_fingerprint: "fp".to_string(),
        payload_json: "{}".to_string(),
        status: status.to_string(),
        google_etag: String::new(),
        attempt_count: 0,
        last_error: String::new(),
        created_at: "2023-11-14T22:00:00Z".to_string(),
        updated_at: "2023-11-14T22:00:00Z".to_string(),
    }
}

fn move_journal_op(
    id: &str,
    status: &str,
    google_event_id: &str,
    local_event_id: &str,
    dest_google_cal_id: &str,
) -> CalendarEventOperation {
    let mut op = journal_op(id, OP_VERB_MOVE, status, google_event_id, local_event_id);
    op.payload_json = serde_json::json!({ "destination": dest_google_cal_id }).to_string();
    op
}

#[test]
fn repair_google_committed_insert_get_200_upserts_cache_zero_posts() {
    // Google 200, D1 upsert failed earlier → empty cache, journal stuck at
    // google_committed. Repair GETs and finishes cache apply. Never POSTs.
    let body = r#"{
        "id": "sancaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "etag": "e-repaired",
        "summary": "Recovered",
        "start": {"dateTime": "2026-08-19T09:00:00Z"},
        "end": {"dateTime": "2026-08-19T10:00:00Z"}
    }"#;
    let http = FakeHttp::new(vec![("/events/sancaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 200, body)]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::with(vec![journal_op(
        "op-1",
        OP_VERB_INSERT,
        OP_STATUS_GOOGLE_COMMITTED,
        "sancaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "",
    )]);

    let errors = pollster::block_on(repair_inflight_operations(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "u-1",
        NOW_UNIX,
    ));
    assert!(errors.is_empty(), "{errors:?}");
    assert!(http.posts.lock().unwrap().is_empty(), "repair is GET-only");
    assert_eq!(http.gets.lock().unwrap().len(), 1);

    let stored = events.stored.lock().unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].google_event_id, "sancaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    assert_eq!(stored[0].title, "Recovered");
    assert_eq!(stored[0].id, "evt-1");

    let op = &ops.stored.lock().unwrap()[0];
    assert_eq!(op.status, OP_STATUS_CACHE_APPLIED);
    assert_eq!(op.local_event_id, "evt-1");
    assert_eq!(op.google_etag, "e-repaired");
}

#[test]
fn repair_pending_insert_get_404_marks_failed_zero_posts() {
    // Google never saw the insert — mark failed, never re-POST.
    let http = FakeHttp::new(vec![("/events/sancbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", 404, "")]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::with(vec![journal_op(
        "op-1",
        OP_VERB_INSERT,
        OP_STATUS_PENDING,
        "sancbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "",
    )]);

    let errors = pollster::block_on(repair_inflight_operations(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "u-1",
        NOW_UNIX,
    ));
    assert!(errors.is_empty(), "{errors:?}");
    assert!(http.posts.lock().unwrap().is_empty(), "never re-insert");
    assert!(events.stored.lock().unwrap().is_empty());
    assert!(events.upserted_single.lock().unwrap().is_none());

    let op = &ops.stored.lock().unwrap()[0];
    assert_eq!(op.status, OP_STATUS_FAILED);
    assert!(op.last_error.contains("never reached Google"), "{}", op.last_error);
}

#[test]
fn repair_google_committed_insert_get_404_local_deletes_and_closes() {
    // Vanished after commit: local-delete + cache_applied (no second insert).
    let http = FakeHttp::new(vec![("/events/g-gone", 404, "")]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    events
        .stored
        .lock()
        .unwrap()
        .push(living_event("local-1", "cal-1", "g-gone"));
    let ops = FakeOperationRepo::with(vec![journal_op(
        "op-1",
        OP_VERB_INSERT,
        OP_STATUS_GOOGLE_COMMITTED,
        "g-gone",
        "local-1",
    )]);

    let errors = pollster::block_on(repair_inflight_operations(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "u-1",
        NOW_UNIX,
    ));
    assert!(errors.is_empty(), "{errors:?}");
    assert!(http.posts.lock().unwrap().is_empty());
    let stored = events.stored.lock().unwrap();
    assert!(stored[0].deleted_at.is_some(), "local row soft-deleted");
    assert_eq!(ops.stored.lock().unwrap()[0].status, OP_STATUS_CACHE_APPLIED);
}

#[test]
fn repair_skips_other_users_and_continues_on_get_error() {
    let http = FakeHttp::new(vec![
        ("/events/mine", 500, "nope"),
        ("/events/other-user", 200, r#"{"id":"other-user","summary":"x"}"#),
    ]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let mut other = journal_op(
        "op-other",
        OP_VERB_INSERT,
        OP_STATUS_GOOGLE_COMMITTED,
        "other-user",
        "",
    );
    other.user_id = "u-2".to_string();
    let ops = FakeOperationRepo::with(vec![
        journal_op(
            "op-mine",
            OP_VERB_INSERT,
            OP_STATUS_GOOGLE_COMMITTED,
            "mine",
            "",
        ),
        other,
    ]);

    let errors = pollster::block_on(repair_inflight_operations(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "u-1",
        NOW_UNIX,
    ));
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("500"), "{errors:?}");
    // Other user's op untouched; mine left google_committed.
    let stored = ops.stored.lock().unwrap();
    assert_eq!(stored[0].status, OP_STATUS_GOOGLE_COMMITTED);
    assert_eq!(stored[1].status, OP_STATUS_GOOGLE_COMMITTED);
    assert!(events.stored.lock().unwrap().is_empty());
}

#[test]
fn repair_google_committed_move_source_gone_dest_200_reassigns_zero_posts() {
    // Move committed on Google; cache apply failed. Source GET 404, dest GET
    // 200 → reassign + upsert dest, cache_applied. Never POSTs.
    let dest_body = r#"{
        "id": "g-moved",
        "etag": "e-dest",
        "summary": "Moved meeting",
        "start": {"dateTime": "2026-08-19T09:00:00Z"},
        "end": {"dateTime": "2026-08-19T10:00:00Z"}
    }"#;
    // One-shots: source 404 first, then dest 200 (substring match order).
    let http = FakeHttp::new(vec![]).with_one_shots(vec![
        ("/calendars/primary%40example.com/events/g-moved", 404, ""),
        ("/calendars/work%40example.com/events/g-moved", 200, dest_body),
    ]);
    let calendars = FakeCalendarRepo::with(vec![
        calendar("cal-1", "primary@example.com", true),
        calendar("cal-2", "work@example.com", true),
    ]);
    let events = FakeEventRepo::new();
    events
        .stored
        .lock()
        .unwrap()
        .push(living_event("local-1", "cal-1", "g-moved"));
    let ops = FakeOperationRepo::with(vec![move_journal_op(
        "op-move",
        OP_STATUS_GOOGLE_COMMITTED,
        "g-moved",
        "local-1",
        "work@example.com",
    )]);

    let errors = pollster::block_on(repair_inflight_operations(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "u-1",
        NOW_UNIX,
    ));
    assert!(errors.is_empty(), "{errors:?}");
    assert!(http.posts.lock().unwrap().is_empty(), "repair is GET-only");
    assert_eq!(http.gets.lock().unwrap().len(), 2, "source + dest GET");

    let stored = events.stored.lock().unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id, "local-1", "local id preserved");
    assert_eq!(stored[0].calendar_id, "cal-2");
    assert_eq!(stored[0].title, "Moved meeting");
    assert!(stored[0].deleted_at.is_none());

    let op = &ops.stored.lock().unwrap()[0];
    assert_eq!(op.status, OP_STATUS_CACHE_APPLIED);
    assert_eq!(op.local_event_id, "local-1");
}

#[test]
fn repair_pending_move_source_and_dest_gone_marks_failed_keeps_local() {
    // Pending move, event on neither calendar → failed; local row remains.
    let http = FakeHttp::new(vec![]).with_one_shots(vec![
        ("/calendars/primary%40example.com/events/g-pending", 404, ""),
        ("/calendars/work%40example.com/events/g-pending", 404, ""),
    ]);
    let calendars = FakeCalendarRepo::with(vec![
        calendar("cal-1", "primary@example.com", true),
        calendar("cal-2", "work@example.com", true),
    ]);
    let events = FakeEventRepo::new();
    events
        .stored
        .lock()
        .unwrap()
        .push(living_event("local-1", "cal-1", "g-pending"));
    let ops = FakeOperationRepo::with(vec![move_journal_op(
        "op-move",
        OP_STATUS_PENDING,
        "g-pending",
        "local-1",
        "work@example.com",
    )]);

    let errors = pollster::block_on(repair_inflight_operations(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "u-1",
        NOW_UNIX,
    ));
    assert!(errors.is_empty(), "{errors:?}");
    assert!(http.posts.lock().unwrap().is_empty());

    let stored = events.stored.lock().unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id, "local-1");
    assert_eq!(stored[0].calendar_id, "cal-1");
    assert!(stored[0].deleted_at.is_none(), "do not delete on pending 404");

    let op = &ops.stored.lock().unwrap()[0];
    assert_eq!(op.status, OP_STATUS_FAILED);
    assert!(op.last_error.contains("never reached Google"), "{}", op.last_error);
}

#[test]
fn projection_hides_unmodified_instance_when_master_exists() {
    let events = FakeEventRepo::new();
    let mut master = living_event("m-1", "cal-1", "abc");
    master.title = "Standup".to_string();
    master.recurrence = r#"["RRULE:FREQ=DAILY"]"#.to_string();
    master.start_time = "2026-09-01T09:00:00Z".to_string();
    master.end_time = "2026-09-01T09:30:00Z".to_string();

    let mut instance = living_event("i-1", "cal-1", "abc_20260910T090000Z");
    instance.title = "Standup".to_string();
    instance.recurring_event_id = "abc".to_string();
    instance.original_start = "2026-09-10T09:00:00Z".to_string();
    instance.start_time = "2026-09-10T09:00:00Z".to_string();
    instance.end_time = "2026-09-10T09:30:00Z".to_string();

    events.stored.lock().unwrap().extend([master, instance]);

    let listed = pollster::block_on(events.list_by_user_id_and_time_range(
        "u-1",
        "2026-09-01T00:00:00Z",
        "2026-09-30T00:00:00Z",
    ))
    .unwrap();
    assert_eq!(listed.len(), 1, "only master: {listed:?}");
    assert_eq!(listed[0].google_event_id, "abc");
}

#[test]
fn projection_keeps_instance_when_no_master() {
    let events = FakeEventRepo::new();
    let mut instance = living_event("i-1", "cal-1", "abc_20260910T090000Z");
    instance.title = "Standup".to_string();
    instance.recurring_event_id = "abc".to_string();
    instance.original_start = "2026-09-10T09:00:00Z".to_string();
    instance.start_time = "2026-09-10T09:00:00Z".to_string();
    instance.end_time = "2026-09-10T09:30:00Z".to_string();
    events.stored.lock().unwrap().push(instance);

    let listed = pollster::block_on(events.list_by_user_id_and_time_range(
        "u-1",
        "2026-09-01T00:00:00Z",
        "2026-09-30T00:00:00Z",
    ))
    .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].google_event_id, "abc_20260910T090000Z");
}

#[test]
fn projection_keeps_modified_exception_with_master() {
    let events = FakeEventRepo::new();
    let mut master = living_event("m-1", "cal-1", "abc");
    master.title = "Standup".to_string();
    master.recurrence = r#"["RRULE:FREQ=DAILY"]"#.to_string();
    master.start_time = "2026-09-01T09:00:00Z".to_string();
    master.end_time = "2026-09-01T09:30:00Z".to_string();

    let mut exception = living_event("e-1", "cal-1", "abc_20260910T090000Z");
    exception.title = "Standup".to_string();
    exception.recurring_event_id = "abc".to_string();
    exception.original_start = "2026-09-10T09:00:00Z".to_string();
    // Moved later → modified exception.
    exception.start_time = "2026-09-10T11:00:00Z".to_string();
    exception.end_time = "2026-09-10T11:30:00Z".to_string();

    events.stored.lock().unwrap().extend([master, exception]);

    let listed = pollster::block_on(events.list_by_user_id_and_time_range(
        "u-1",
        "2026-09-01T00:00:00Z",
        "2026-09-30T00:00:00Z",
    ))
    .unwrap();
    let ids: Vec<_> = listed.iter().map(|e| e.google_event_id.as_str()).collect();
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert!(ids.contains(&"abc"));
    assert!(ids.contains(&"abc_20260910T090000Z"));
}
