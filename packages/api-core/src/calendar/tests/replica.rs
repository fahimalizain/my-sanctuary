use super::support::*;
use crate::calendar::{list_events, sync_calendar, CalendarError};
use crate::repo::CalendarRepo;

#[test]
fn empty_items_with_next_sync_token_is_replica_success() {
    // Completed empty incremental: items=[] + nextSyncToken is publication
    // on the replica path only.
    let http = FakeHttp::new(vec![("/events", 200, r#"{"items":[],"nextSyncToken":"st-empty"}"#)]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let cal = calendars.stored.lock().unwrap()[0].clone();

    pollster::block_on(sync_calendar(
        &http, &calendars, &events, &access(), &cal, "2023-11-14T22:13:20Z",
    ))
    .unwrap();

    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].sync_token, "st-empty");
    assert_eq!(
        stored[0].last_success_at.as_deref(),
        Some("2023-11-14T22:13:20Z")
    );
    assert_eq!(
        stored[0].last_attempt_at.as_deref(),
        Some("2023-11-14T22:13:20Z")
    );
    assert_eq!(stored[0].failure_streak, 0);
    assert_eq!(stored[0].sync_status, "ready");
    assert!(stored[0].initial_sync_complete);
    assert_eq!(stored[0].cache_revision, 1);
    assert!(stored[0].last_error_code.is_empty());
}

#[test]
fn google_list_failure_records_attempt_not_success() {
    // Seed a previously healthy calendar; call sync_calendar directly
    // (list_events would be cache-only), then list_events for the envelope.
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.sync_token = "old-tok".to_string();
    cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.initial_sync_complete = true;
    cal.failure_streak = 0;
    cal.sync_status = "ready".to_string();

    let http = FakeHttp::new(vec![("/events", 500, "")]);
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();

    let err = pollster::block_on(sync_calendar(
        &http, &calendars, &events, &access(), &cal, "2023-11-14T22:13:20Z",
    ))
    .unwrap_err();
    assert!(
        matches!(err, CalendarError::GoogleApi(ref m) if m.contains("500")),
        "{err:?}"
    );

    let stored = calendars.stored.lock().unwrap();
    assert_eq!(
        stored[0].last_attempt_at.as_deref(),
        Some("2023-11-14T22:13:20Z")
    );
    assert_eq!(
        stored[0].last_success_at.as_deref(),
        Some("2023-11-14T21:00:00Z"),
        "success timestamp must not move on failure"
    );
    assert_eq!(stored[0].sync_token, "old-tok");
    assert_eq!(stored[0].failure_streak, 1);
    assert_eq!(stored[0].sync_status, "retrying");
    assert_eq!(stored[0].last_error_code, "google_transient");
    assert!(calendars.sync_states.lock().unwrap().is_empty());
    drop(stored);

    let watches = FakeWatchChannelRepo::new();
    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
    ))
    .unwrap();

    // Cache-only: no additional Google calls beyond the failed sync.
    assert_eq!(http.gets.lock().unwrap().len(), 1);
    assert_eq!(
        output.sync.status,
        crate::calendar_sync::SyncAggregateStatus::Degraded
    );
    let health = &output.sync.calendars[0];
    assert_eq!(health.state, crate::calendar_sync::CalendarReplicaState::Retrying);
    assert_eq!(health.error_code.as_deref(), Some("google_transient"));

    let json = serde_json::to_string(&output.sync).unwrap();
    assert!(!json.contains("old-tok"), "{json}");
}

#[test]
fn upsert_failure_records_storage_transient_without_advancing_token() {
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.sync_token = "old-tok".to_string();
    cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.initial_sync_complete = true;
    cal.sync_status = "ready".to_string();

    let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();
    *events.fail_upsert.lock().unwrap() = true;

    let err = pollster::block_on(sync_calendar(
        &http, &calendars, &events, &access(), &cal, "2023-11-14T22:13:20Z",
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::Repo(_)), "{err:?}");

    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].sync_token, "old-tok");
    assert_eq!(
        stored[0].last_success_at.as_deref(),
        Some("2023-11-14T21:00:00Z")
    );
    assert_eq!(
        stored[0].last_attempt_at.as_deref(),
        Some("2023-11-14T22:13:20Z")
    );
    assert_eq!(stored[0].failure_streak, 1);
    assert_eq!(stored[0].sync_status, "retrying");
    assert_eq!(stored[0].last_error_code, "storage_transient");
    assert!(calendars.sync_states.lock().unwrap().is_empty());
}

#[test]
fn missing_terminal_next_sync_token_is_not_success() {
    // Single page 200 with items but no nextSyncToken / nextPageToken.
    let body = r#"{"items":[
        {"id": "evt-1", "summary": "Standup",
         "start": {"dateTime": "2026-08-18T09:00:00Z"},
         "end": {"dateTime": "2026-08-18T09:30:00Z"}}
    ]}"#;
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.sync_token = "old-tok".to_string();
    cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.initial_sync_complete = true;
    cal.sync_status = "ready".to_string();

    let http = FakeHttp::new(vec![("/events", 200, body)]);
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();

    let err = pollster::block_on(sync_calendar(
        &http, &calendars, &events, &access(), &cal, "2023-11-14T22:13:20Z",
    ))
    .unwrap_err();
    assert!(
        matches!(err, CalendarError::InvalidResponse(ref m) if m.contains("missing nextSyncToken")),
        "{err:?}"
    );

    // Apply may have happened; publication did not.
    assert_eq!(events.upserted_batch.lock().unwrap().len(), 1);

    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].sync_token, "old-tok");
    assert_eq!(
        stored[0].last_success_at.as_deref(),
        Some("2023-11-14T21:00:00Z")
    );
    assert_eq!(
        stored[0].last_synced_at.as_deref(),
        Some("2023-11-14T21:00:00Z")
    );
    assert_eq!(
        stored[0].last_attempt_at.as_deref(),
        Some("2023-11-14T22:13:20Z")
    );
    assert_eq!(stored[0].last_error_code, "missing_sync_token");
    assert_eq!(stored[0].sync_status, "retrying");
    assert_eq!(stored[0].failure_streak, 1);
    assert!(calendars.sync_states.lock().unwrap().is_empty());
    drop(stored);

    // list_events is cache-only and surfaces the persisted health +
    // does not re-sync; first-paint path would also surface sync_errors.
    let watches = FakeWatchChannelRepo::new();
    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
    ))
    .unwrap();
    assert_eq!(
        output.sync.calendars[0].error_code.as_deref(),
        Some("missing_sync_token")
    );
    assert_eq!(
        output.sync.calendars[0].state,
        crate::calendar_sync::CalendarReplicaState::Retrying
    );
}

#[test]
fn events_list_410_after_retry_records_gone() {
    // First 410 retries in-invocation; second 410 is gone (not success).
    let http = FakeHttp::new(vec![("/events", 410, "")]);
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.sync_token = "stale-token".to_string();
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();

    let err = pollster::block_on(sync_calendar(
        &http, &calendars, &events, &access(), &cal, "2023-11-14T22:13:20Z",
    ))
    .unwrap_err();
    assert!(
        matches!(err, CalendarError::GoogleApi(ref m) if m.contains("410")),
        "{err:?}"
    );

    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].sync_token, "stale-token", "token not cleared on gone");
    assert_eq!(stored[0].last_error_code, "gone");
    assert_eq!(stored[0].sync_status, "retrying");
    assert_eq!(stored[0].failure_streak, 1);
    assert!(stored[0].last_success_at.is_none());
    assert_eq!(
        stored[0].last_attempt_at.as_deref(),
        Some("2023-11-14T22:13:20Z")
    );
}

#[test]
fn replica_paginated_apply_publishes_token_only_after_last_page() {
    let page_one = r#"{"items":[{"id":"p1","summary":"A","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}}],"nextPageToken":"tok-2"}"#;
    let page_two = r#"{"items":[{"id":"p2","summary":"B","start":{"dateTime":"2026-08-18T10:00:00Z"},"end":{"dateTime":"2026-08-18T10:30:00Z"}}],"nextSyncToken":"st-final"}"#;
    let now = "2023-11-14T22:13:20Z";

    // Phase 1: page 2 fails — page 1 applied, token stays old.
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.sync_token = "old-tok".to_string();
    cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.initial_sync_complete = true;
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();
    let http_fail = FakeHttp::new(vec![
        ("pageToken=tok-2", 500, ""),
        ("/events", 200, page_one),
    ]);
    let err = pollster::block_on(sync_calendar(
        &http_fail, &calendars, &events, &access(), &cal, now,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::GoogleApi(ref m) if m.contains("500")), "{err:?}");
    assert_eq!(events.upserted_batch.lock().unwrap().len(), 1);
    assert_eq!(events.upserted_batch.lock().unwrap()[0].google_event_id, "p1");
    assert_eq!(calendars.stored.lock().unwrap()[0].sync_token, "old-tok");
    assert!(calendars.sync_states.lock().unwrap().is_empty());
    assert!(events.deleted_stale.lock().unwrap().is_empty());

    // Phase 2: both pages succeed — terminal token published + fingerprint.
    let http_ok = FakeHttp::new(vec![
        ("pageToken=tok-2", 200, page_two),
        ("/events", 200, page_one),
    ]);
    pollster::block_on(sync_calendar(
        &http_ok, &calendars, &events, &access(), &cal, now,
    ))
    .unwrap();
    let upserted = events.upserted_batch.lock().unwrap();
    assert!(upserted.iter().any(|e| e.google_event_id == "p1"));
    assert!(upserted.iter().any(|e| e.google_event_id == "p2"));
    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].sync_token, "st-final");
    assert_eq!(
        stored[0].sync_query_fingerprint,
        crate::calendar_sync::replica_query_fingerprint()
    );
    assert!(events.deleted_stale.lock().unwrap().is_empty());
}

#[test]
fn replica_410_first_page_merge_full_preserves_task_id_and_ghosts() {
    let merge = r#"{"items":[
        {"id":"keep","summary":"Keep","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}}
    ],"nextSyncToken":"st-merge"}"#;
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.sync_token = "stale-token".to_string();
    cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.initial_sync_complete = true;
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();
    events.stored.lock().unwrap().extend([
        seeded_event("e-keep", "cal-1", "keep", "keep-me"),
        seeded_event("e-ghost", "cal-1", "ghost", ""),
    ]);
    let http = FakeHttp::new(vec![
        ("syncToken=stale-token", 410, ""),
        ("/events", 200, merge),
    ]);
    pollster::block_on(sync_calendar(
        &http, &calendars, &events, &access(), &cal, "2023-11-14T22:13:20Z",
    ))
    .unwrap();

    let stored_ev = events.stored.lock().unwrap();
    let keep = stored_ev.iter().find(|e| e.google_event_id == "keep").unwrap();
    assert_eq!(keep.task_id, "keep-me", "COALESCE must preserve task_id");
    let ghost = stored_ev.iter().find(|e| e.google_event_id == "ghost").unwrap();
    assert!(ghost.deleted_at.is_none(), "ghost must not be truncated");
    assert!(events.deleted_stale.lock().unwrap().is_empty());
    assert_eq!(calendars.stored.lock().unwrap()[0].sync_token, "st-merge");
}

#[test]
fn replica_410_later_page_merge_full_keeps_partial_apply() {
    let page_one = r#"{"items":[{"id":"a","summary":"A","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}}],"nextPageToken":"tok-2"}"#;
    let merge = r#"{"items":[
        {"id":"a","summary":"A2","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}},
        {"id":"keep","summary":"Keep","start":{"dateTime":"2026-08-18T11:00:00Z"},"end":{"dateTime":"2026-08-18T11:30:00Z"}}
    ],"nextSyncToken":"st-mf"}"#;
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.sync_token = "old".to_string();
    cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.initial_sync_complete = true;
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();
    events
        .stored
        .lock()
        .unwrap()
        .push(seeded_event("e-keep", "cal-1", "keep", "keep-me"));
    let http = FakeHttp::new(vec![
        ("pageToken=tok-2", 410, ""),
        ("syncToken=old", 200, page_one),
        ("/events", 200, merge),
    ]);
    pollster::block_on(sync_calendar(
        &http, &calendars, &events, &access(), &cal, "2023-11-14T22:13:20Z",
    ))
    .unwrap();

    assert!(events.deleted_stale.lock().unwrap().is_empty());
    assert_eq!(calendars.stored.lock().unwrap()[0].sync_token, "st-mf");
    let keep = events
        .stored
        .lock()
        .unwrap()
        .iter()
        .find(|e| e.google_event_id == "keep")
        .unwrap()
        .clone();
    assert_eq!(keep.task_id, "keep-me");
    // Partial apply of A from page1 is OK (also in merge-full).
    assert!(events
        .stored
        .lock()
        .unwrap()
        .iter()
        .any(|e| e.google_event_id == "a"));
}

#[test]
fn replica_failure_after_page1_delete_replays_idempotently() {
    let page_one = r#"{"items":[
        {"id":"gone","status":"cancelled"},
        {"id":"live","summary":"Live","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}}
    ],"nextPageToken":"tok-2"}"#;
    let page_two = r#"{"items":[{"id":"p2","summary":"P2","start":{"dateTime":"2026-08-18T10:00:00Z"},"end":{"dateTime":"2026-08-18T10:30:00Z"}}],"nextSyncToken":"st-done"}"#;
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.sync_token = "old-tok".to_string();
    cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.initial_sync_complete = true;
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();
    events
        .stored
        .lock()
        .unwrap()
        .push(seeded_event("e-gone", "cal-1", "gone", ""));

    let http_fail = FakeHttp::new(vec![
        ("pageToken=tok-2", 500, ""),
        ("/events", 200, page_one),
    ]);
    let err = pollster::block_on(sync_calendar(
        &http_fail,
        &calendars,
        &events,
        &access(),
        &cal,
        "2023-11-14T22:13:20Z",
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::GoogleApi(_)), "{err:?}");
    assert_eq!(calendars.stored.lock().unwrap()[0].sync_token, "old-tok");
    assert!(events
        .stored
        .lock()
        .unwrap()
        .iter()
        .find(|e| e.google_event_id == "gone")
        .unwrap()
        .deleted_at
        .is_some());

    let http_ok = FakeHttp::new(vec![
        ("pageToken=tok-2", 200, page_two),
        ("/events", 200, page_one),
    ]);
    pollster::block_on(sync_calendar(
        &http_ok,
        &calendars,
        &events,
        &access(),
        &cal,
        "2023-11-14T22:13:20Z",
    ))
    .unwrap();
    assert_eq!(calendars.stored.lock().unwrap()[0].sync_token, "st-done");
    // Idempotent: still soft-deleted once, live + p2 present.
    assert!(events
        .stored
        .lock()
        .unwrap()
        .iter()
        .find(|e| e.google_event_id == "gone")
        .unwrap()
        .deleted_at
        .is_some());
    assert!(events
        .stored
        .lock()
        .unwrap()
        .iter()
        .any(|e| e.google_event_id == "live" && e.deleted_at.is_none()));
    assert!(events
        .stored
        .lock()
        .unwrap()
        .iter()
        .any(|e| e.google_event_id == "p2"));
}

#[test]
fn replica_poison_on_one_calendar_records_mapping_poison() {
    // Replica path (sync_calendar) still classifies invalid JSON as
    // mapping_poison via record_sync_failure.
    let cal = calendar("cal-a", "a@example.com", true);
    let http = FakeHttp::new(vec![("a%40example.com/events", 200, "not-json{{{")]);
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();
    let err = pollster::block_on(sync_calendar(
        &http, &calendars, &events, &access(), &cal, "2023-11-14T22:13:20Z",
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::InvalidResponse(_)), "{err:?}");
    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].last_error_code, "mapping_poison");
    assert_eq!(stored[0].sync_status, "retrying");
    assert!(stored[0].sync_token.is_empty());
}

#[test]
fn replica_unexpired_foreign_lease_skips_without_failure() {
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.sync_token = "old-tok".to_string();
    cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.initial_sync_complete = true;
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    calendars.force_lease("cal-1", "other-owner", Some("2099-01-01T00:00:00Z"));
    let events = FakeEventRepo::new();
    let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);

    pollster::block_on(sync_calendar(
        &http,
        &calendars,
        &events,
        &access(),
        &cal,
        "2023-11-14T22:13:20Z",
    ))
    .unwrap();

    assert!(
        http.gets.lock().unwrap().is_empty(),
        "must not fetch when lease is held"
    );
    assert!(events.upserted_batch.lock().unwrap().is_empty());
    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].sync_token, "old-tok");
    assert_eq!(stored[0].failure_streak, 0);
    assert_eq!(stored[0].lease_owner, "other-owner");
    // Attempt was stamped; no failure.
    assert_eq!(
        stored[0].last_attempt_at.as_deref(),
        Some("2023-11-14T22:13:20Z")
    );
    assert!(stored[0].last_error_code.is_empty());
}

#[test]
fn replica_expired_foreign_lease_is_stolen_and_walk_succeeds() {
    let cal = calendar("cal-1", "primary@example.com", true);
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    calendars.force_lease("cal-1", "stale-owner", Some("2020-01-01T00:00:00Z"));
    let events = FakeEventRepo::new();
    let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);

    pollster::block_on(sync_calendar(
        &http,
        &calendars,
        &events,
        &access(),
        &cal,
        "2023-11-14T22:13:20Z",
    ))
    .unwrap();

    assert_eq!(calendars.stored.lock().unwrap()[0].sync_token, "st-9");
    assert!(calendars.stored.lock().unwrap()[0].lease_owner.is_empty());
    assert_eq!(events.upserted_batch.lock().unwrap().len(), 2);
}

#[test]
fn record_sync_success_if_owner_rejects_wrong_owner() {
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    calendars.force_lease("cal-1", "owner-a", Some("2099-01-01T00:00:00Z"));
    let ok = pollster::block_on(calendars.record_sync_success_if_owner(
        "cal-1",
        "new-tok",
        "fp",
        "owner-b",
        "2023-11-14T22:13:20Z",
    ))
    .unwrap();
    assert!(!ok);
    assert!(calendars.stored.lock().unwrap()[0].sync_token.is_empty());
    assert!(calendars.sync_states.lock().unwrap().is_empty());
}

#[test]
fn replica_mid_walk_lease_loss_does_not_publish_token() {
    let page_one = r#"{"items":[{"id":"p1","summary":"A","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}}],"nextPageToken":"tok-2"}"#;
    let page_two = r#"{"items":[{"id":"p2","summary":"B","start":{"dateTime":"2026-08-18T10:00:00Z"},"end":{"dateTime":"2026-08-18T10:30:00Z"}}],"nextSyncToken":"st-stolen"}"#;
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.sync_token = "old-tok".to_string();
    cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.initial_sync_complete = true;
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    // After re-read (#1) + page1 fence (#2), steal before renew.
    *calendars.steal_lease_after_get_by_id.lock().unwrap() = Some(2);
    let events = FakeEventRepo::new();
    let http = FakeHttp::new(vec![
        ("pageToken=tok-2", 200, page_two),
        ("/events", 200, page_one),
    ]);

    let err = pollster::block_on(sync_calendar(
        &http,
        &calendars,
        &events,
        &access(),
        &cal,
        "2023-11-14T22:13:20Z",
    ))
    .unwrap_err();
    assert!(
        matches!(err, CalendarError::Invalid(ref m) if m.contains("lost replica lease")),
        "{err:?}"
    );
    // Page 1 may have applied.
    assert!(events
        .upserted_batch
        .lock()
        .unwrap()
        .iter()
        .any(|e| e.google_event_id == "p1"));
    assert_eq!(calendars.stored.lock().unwrap()[0].sync_token, "old-tok");
    assert!(calendars.sync_states.lock().unwrap().is_empty());
    // Must not have applied page 2 / published st-stolen.
    assert!(!events
        .upserted_batch
        .lock()
        .unwrap()
        .iter()
        .any(|e| e.google_event_id == "p2"));
}

#[test]
fn replica_cancelled_event_delete_failure_records_storage_transient() {
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
    let cal = calendars.stored.lock().unwrap()[0].clone();

    let err = pollster::block_on(sync_calendar(
        &http, &calendars, &events, &access(), &cal, "2023-11-14T22:13:20Z",
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::Repo(_)), "{err:?}");
    assert!(calendars.sync_states.lock().unwrap().is_empty());
    let stored = calendars.stored.lock().unwrap();
    assert!(stored[0].sync_token.is_empty());
    assert_eq!(stored[0].last_error_code, "storage_transient");
    assert_eq!(stored[0].failure_streak, 1);
    assert_eq!(stored[0].sync_status, "retrying");
    assert_eq!(
        stored[0].last_attempt_at.as_deref(),
        Some("2023-11-14T22:13:20Z")
    );
}

#[test]
fn sync_maps_sanctuary_task_id_onto_cached_events() {
    let body = r#"{"items":[
        {"id": "timed", "summary": "Deep Work",
         "start": {"dateTime": "2026-08-18T09:00:00Z"},
         "end": {"dateTime": "2026-08-18T10:00:00Z"},
         "extendedProperties": {"shared": {"sanctuary_task_id": "task-1"}}}
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
    assert_eq!(upserted.len(), 1);
    assert_eq!(upserted[0].task_id, "task-1", "carrier copied from shared props");
    assert_eq!(output.events[0].task_id, "task-1");
}
