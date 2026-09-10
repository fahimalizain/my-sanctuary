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
        &http, &calendars, &events, &FakeOperationRepo::new(), &access(), &cal, "2023-11-14T22:13:20Z",
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
        &http, &calendars, &events, &FakeOperationRepo::new(), &access(), &cal, "2023-11-14T22:13:20Z",
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
        &http, &calendars, &events, &FakeOperationRepo::new(), &access(), &cal, "2023-11-14T22:13:20Z",
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
        &http, &calendars, &events, &FakeOperationRepo::new(), &access(), &cal, "2023-11-14T22:13:20Z",
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
    // First 410 retries in-invocation (durable reseed); second 410 is gone.
    let http = FakeHttp::new(vec![("/events", 410, "")]);
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.sync_token = "stale-token".to_string();
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();

    let err = pollster::block_on(sync_calendar(
        &http, &calendars, &events, &FakeOperationRepo::new(), &access(), &cal, "2023-11-14T22:13:20Z",
    ))
    .unwrap_err();
    assert!(
        matches!(err, CalendarError::GoogleApi(ref m) if m.contains("410")),
        "{err:?}"
    );

    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].sync_token, "stale-token", "token not cleared on gone");
    assert!(
        stored[0].full_sync_requested,
        "reseed must stay durable after unfinished 410"
    );
    assert_eq!(stored[0].last_error_code, "gone");
    assert_eq!(stored[0].sync_status, "rebuilding");
    assert_eq!(stored[0].failure_streak, 1);
    assert!(stored[0].last_success_at.is_none());
    assert_eq!(
        stored[0].last_attempt_at.as_deref(),
        Some("2023-11-14T22:13:20Z")
    );
    drop(stored);

    // Envelope reads the real write path (begin_replica_reseed + Gone → rebuilding).
    let watches = FakeWatchChannelRepo::new();
    let output = pollster::block_on(list_events(
        &http, &calendars, &events, &watches, &access(), "u-1",
        "2026-08-01T00:00:00Z", "2026-09-01T00:00:00Z", NOW_UNIX, None,
    ))
    .unwrap();
    assert_eq!(
        output.sync.calendars[0].state,
        crate::calendar_sync::CalendarReplicaState::Rebuilding
    );
    assert_eq!(
        output.sync.calendars[0].error_code.as_deref(),
        Some("gone")
    );
}

#[test]
fn events_list_410_then_success_clears_full_sync_requested() {
    // First GET with syncToken → 410; second without token → 200 + nextSyncToken.
    let merge = r#"{"items":[],"nextSyncToken":"st-after-410"}"#;
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.sync_token = "stale-token".to_string();
    cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.initial_sync_complete = true;
    cal.sync_status = "ready".to_string();
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();
    let http = FakeHttp::new(vec![
        ("syncToken=stale-token", 410, ""),
        ("/events", 200, merge),
    ]);

    pollster::block_on(sync_calendar(
        &http,
        &calendars,
        &events,
        &FakeOperationRepo::new(),
        &access(),
        &cal,
        "2023-11-14T22:13:20Z",
    ))
    .unwrap();

    let stored = calendars.stored.lock().unwrap();
    assert!(!stored[0].full_sync_requested, "success clears reseed flag");
    assert_eq!(stored[0].sync_status, "ready");
    assert_eq!(stored[0].sync_token, "st-after-410");
    assert!(stored[0].last_error_code.is_empty());
}

#[test]
fn full_sync_requested_drops_stored_token_on_next_walk() {
    // Isolate-death recovery: durable flag must not retry incremental with
    // the old stored token.
    let body = r#"{"items":[],"nextSyncToken":"st-reseed"}"#;
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.sync_token = "old-tok".to_string();
    cal.full_sync_requested = true;
    cal.sync_status = "retrying".to_string();
    cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.initial_sync_complete = true;
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();
    // Guard: any request that still carries the old token must not succeed.
    let http = FakeHttp::new(vec![
        ("syncToken=old-tok", 599, ""),
        ("/events", 200, body),
    ]);

    pollster::block_on(sync_calendar(
        &http,
        &calendars,
        &events,
        &FakeOperationRepo::new(),
        &access(),
        &cal,
        "2023-11-14T22:13:20Z",
    ))
    .unwrap();

    let gets = http.gets.lock().unwrap();
    assert!(
        gets.iter().any(|u| u.contains("/events") && !u.contains("syncToken=")),
        "walk must list without syncToken: {gets:?}"
    );
    assert!(
        gets.iter().all(|u| !u.contains("syncToken=old-tok")),
        "must not send old token: {gets:?}"
    );
    drop(gets);

    let stored = calendars.stored.lock().unwrap();
    assert!(!stored[0].full_sync_requested);
    assert_eq!(stored[0].sync_token, "st-reseed");
    assert_eq!(stored[0].sync_status, "ready");
}

#[test]
fn fingerprint_mismatch_requests_full_sync() {
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.sync_token = "old-tok".to_string();
    cal.sync_query_fingerprint = "not-the-current-fp".to_string();
    cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.initial_sync_complete = true;
    cal.sync_status = "ready".to_string();
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();

    // Phase 1: merge-full GET (no token) fails — flag stays, token untouched,
    // status is retrying (google_transient), not rebuilding.
    let http_fail = FakeHttp::new(vec![
        ("syncToken=old-tok", 599, ""),
        ("/events", 500, ""),
    ]);
    let err = pollster::block_on(sync_calendar(
        &http_fail,
        &calendars,
        &events,
        &FakeOperationRepo::new(),
        &access(),
        &cal,
        "2023-11-14T22:13:20Z",
    ))
    .unwrap_err();
    assert!(
        matches!(err, CalendarError::GoogleApi(ref m) if m.contains("500")),
        "{err:?}"
    );
    {
        let stored = calendars.stored.lock().unwrap();
        assert!(stored[0].full_sync_requested);
        assert_eq!(stored[0].sync_token, "old-tok");
        assert_eq!(stored[0].sync_status, "retrying");
        assert_eq!(stored[0].last_error_code, "google_transient");
    }
    let fail_gets = http_fail.gets.lock().unwrap();
    assert!(
        fail_gets
            .iter()
            .all(|u| !u.contains("syncToken=old-tok")),
        "fingerprint mismatch must drop token: {fail_gets:?}"
    );
    drop(fail_gets);

    // Phase 2: successful merge-full clears the flag and publishes a new token.
    // Re-seed flag on the stored row (already true) and use a fresh cal snapshot
    // that still carries the stale fingerprint so the walk reseeds again.
    let http_ok = FakeHttp::new(vec![
        ("syncToken=old-tok", 599, ""),
        ("/events", 200, r#"{"items":[],"nextSyncToken":"st-fp"}"#),
    ]);
    // Pass a cal that still has the mismatch; sync_calendar re-reads from store.
    pollster::block_on(sync_calendar(
        &http_ok,
        &calendars,
        &events,
        &FakeOperationRepo::new(),
        &access(),
        &cal,
        "2023-11-14T22:13:20Z",
    ))
    .unwrap();

    let stored = calendars.stored.lock().unwrap();
    assert!(!stored[0].full_sync_requested);
    assert_eq!(stored[0].sync_token, "st-fp");
    assert_eq!(stored[0].sync_status, "ready");
    assert_eq!(
        stored[0].sync_query_fingerprint,
        crate::calendar_sync::replica_query_fingerprint()
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
    // Pre-existing ghost must survive incremental (no merge-full sweep).
    events
        .stored
        .lock()
        .unwrap()
        .push(seeded_event("e-ghost", "cal-1", "ghost", ""));
    let http_fail = FakeHttp::new(vec![
        ("pageToken=tok-2", 500, ""),
        ("/events", 200, page_one),
    ]);
    let err = pollster::block_on(sync_calendar(
        &http_fail, &calendars, &events, &FakeOperationRepo::new(), &access(), &cal, now,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::GoogleApi(ref m) if m.contains("500")), "{err:?}");
    assert_eq!(events.upserted_batch.lock().unwrap().len(), 1);
    assert_eq!(events.upserted_batch.lock().unwrap()[0].google_event_id, "p1");
    assert_eq!(calendars.stored.lock().unwrap()[0].sync_token, "old-tok");
    assert!(calendars.sync_states.lock().unwrap().is_empty());
    assert!(events.deleted_stale.lock().unwrap().is_empty());
    assert!(
        events
            .stored
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.google_event_id == "ghost")
            .unwrap()
            .deleted_at
            .is_none(),
        "mid-walk incremental failure must not sweep"
    );

    // Phase 2: both pages succeed — terminal token published + fingerprint.
    // Incremental completed walk must still leave the ghost living.
    let http_ok = FakeHttp::new(vec![
        ("pageToken=tok-2", 200, page_two),
        ("/events", 200, page_one),
    ]);
    pollster::block_on(sync_calendar(
        &http_ok, &calendars, &events, &FakeOperationRepo::new(), &access(), &cal, now,
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
    let ghost = events
        .stored
        .lock()
        .unwrap()
        .iter()
        .find(|e| e.google_event_id == "ghost")
        .unwrap()
        .clone();
    assert!(
        ghost.deleted_at.is_none(),
        "incremental success must not sweep pre-existing ghost"
    );
}

#[test]
fn replica_410_completed_merge_full_sweeps_ghosts_and_preserves_task_id() {
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
        &http, &calendars, &events, &FakeOperationRepo::new(), &access(), &cal, "2023-11-14T22:13:20Z",
    ))
    .unwrap();

    let stored_ev = events.stored.lock().unwrap();
    let keep = stored_ev.iter().find(|e| e.google_event_id == "keep").unwrap();
    assert_eq!(keep.task_id, "keep-me", "COALESCE must preserve task_id");
    assert!(keep.deleted_at.is_none(), "seen keep must stay living");
    let ghost = stored_ev.iter().find(|e| e.google_event_id == "ghost").unwrap();
    assert!(
        ghost.deleted_at.is_some(),
        "empty-task_id ghost absent from merge-full must be soft-deleted"
    );
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
        &http, &calendars, &events, &FakeOperationRepo::new(), &access(), &cal, "2023-11-14T22:13:20Z",
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
    assert!(keep.deleted_at.is_none(), "keep in merge body stays living");
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
        &FakeOperationRepo::new(), &access(),
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
        &FakeOperationRepo::new(), &access(),
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
        &http, &calendars, &events, &FakeOperationRepo::new(), &access(), &cal, "2023-11-14T22:13:20Z",
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
        &FakeOperationRepo::new(), &access(),
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
        &FakeOperationRepo::new(), &access(),
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
        &FakeOperationRepo::new(), &access(),
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
fn replica_fenced_apply_steal_before_first_upsert_writes_nothing() {
    // Two-page walk; steal lease on the 1st fenced apply (page-1 upserts).
    // Loser must not write page-1 or page-2 rows and must not move the cursor.
    let page_one = r#"{"items":[{"id":"p1","summary":"A","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}}],"nextPageToken":"tok-2"}"#;
    let page_two = r#"{"items":[{"id":"p2","summary":"B","start":{"dateTime":"2026-08-18T10:00:00Z"},"end":{"dateTime":"2026-08-18T10:30:00Z"}}],"nextSyncToken":"st-stolen"}"#;
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.sync_token = "old-tok".to_string();
    cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.initial_sync_complete = true;
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();
    events.gate_applies_on(&calendars);
    *events.inject_lease_before_fenced_apply.lock().unwrap() =
        Some((1, "thief".to_string(), Some("2099-01-01T00:00:00Z".to_string())));
    let http = FakeHttp::new(vec![
        ("pageToken=tok-2", 200, page_two),
        ("/events", 200, page_one),
    ]);

    let err = pollster::block_on(sync_calendar(
        &http,
        &calendars,
        &events,
        &FakeOperationRepo::new(),
        &access(),
        &cal,
        "2023-11-14T22:13:20Z",
    ))
    .unwrap_err();
    assert!(
        matches!(err, CalendarError::Invalid(ref m) if m.contains("lost replica lease")),
        "{err:?}"
    );
    assert!(
        events.upserted_batch.lock().unwrap().is_empty(),
        "no page-1 or page-2 upserts: {:?}",
        events.upserted_batch.lock().unwrap()
    );
    assert_eq!(calendars.stored.lock().unwrap()[0].sync_token, "old-tok");
    assert!(calendars.sync_states.lock().unwrap().is_empty());
}

#[test]
fn replica_fenced_apply_expire_before_tombstone_leaves_row_living() {
    // Cancelled + living on one page; expire lease on the 1st fenced apply
    // (the tombstone). Cancelled row stays living; sibling not upserted.
    let body = r#"{"items":[
        {"id":"cancelled","status":"cancelled",
         "start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}},
        {"id":"living","summary":"Keep",
         "start":{"dateTime":"2026-08-18T10:00:00Z"},"end":{"dateTime":"2026-08-18T10:30:00Z"}}
    ],"nextSyncToken":"st-new"}"#;
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
        .push(seeded_event("evt-cancel", "cal-1", "cancelled", ""));
    events.gate_applies_on(&calendars);
    *events.inject_lease_before_fenced_apply.lock().unwrap() =
        Some((1, "loser".to_string(), Some("2020-01-01T00:00:00Z".to_string())));
    let http = FakeHttp::new(vec![("/events", 200, body)]);

    let err = pollster::block_on(sync_calendar(
        &http,
        &calendars,
        &events,
        &FakeOperationRepo::new(),
        &access(),
        &cal,
        "2023-11-14T22:13:20Z",
    ))
    .unwrap_err();
    assert!(
        matches!(err, CalendarError::Invalid(ref m) if m.contains("lost replica lease")),
        "{err:?}"
    );
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
    assert_eq!(calendars.stored.lock().unwrap()[0].sync_token, "old-tok");
    assert!(calendars.sync_states.lock().unwrap().is_empty());
}

#[test]
fn replica_winner_after_expired_loser_applies_and_publishes() {
    // After test-style expire inject fails the loser, clear the hook and
    // re-run: expired foreign lease is stealable; winner applies + publishes.
    let body = r#"{"items":[
        {"id":"cancelled","status":"cancelled",
         "start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}},
        {"id":"living","summary":"Keep",
         "start":{"dateTime":"2026-08-18T10:00:00Z"},"end":{"dateTime":"2026-08-18T10:30:00Z"}}
    ],"nextSyncToken":"st-win"}"#;
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
        .push(seeded_event("evt-cancel", "cal-1", "cancelled", ""));
    events.gate_applies_on(&calendars);
    *events.inject_lease_before_fenced_apply.lock().unwrap() =
        Some((1, "loser".to_string(), Some("2020-01-01T00:00:00Z".to_string())));

    let http_lose = FakeHttp::new(vec![("/events", 200, body)]);
    let err = pollster::block_on(sync_calendar(
        &http_lose,
        &calendars,
        &events,
        &FakeOperationRepo::new(),
        &access(),
        &cal,
        "2023-11-14T22:13:20Z",
    ))
    .unwrap_err();
    assert!(
        matches!(err, CalendarError::Invalid(ref m) if m.contains("lost replica lease")),
        "{err:?}"
    );
    assert_eq!(calendars.stored.lock().unwrap()[0].sync_token, "old-tok");

    // Clear inject; lease remains expired foreign — stealable on next walk.
    *events.inject_lease_before_fenced_apply.lock().unwrap() = None;
    let http_win = FakeHttp::new(vec![("/events", 200, body)]);
    pollster::block_on(sync_calendar(
        &http_win,
        &calendars,
        &events,
        &FakeOperationRepo::new(),
        &access(),
        &cal,
        "2023-11-14T22:13:20Z",
    ))
    .unwrap();

    assert_eq!(calendars.stored.lock().unwrap()[0].sync_token, "st-win");
    assert!(
        !calendars.sync_states.lock().unwrap().is_empty(),
        "winner must publish"
    );
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
        "winner upserts living: {:?}",
        events.upserted_batch.lock().unwrap()
    );
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
        &http, &calendars, &events, &FakeOperationRepo::new(), &access(), &cal, "2023-11-14T22:13:20Z",
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

#[test]
fn replica_skips_inflight_google_event_ids() {
    // In-flight journal row for g-inflight: replica page must not upsert or
    // soft-delete that id (would clobber a concurrent user write).
    use crate::models::{
        CalendarEventOperation, OP_STATUS_PENDING, OP_VERB_PATCH,
    };

    let body = r#"{"items":[
        {"id":"g-inflight","summary":"Stale title",
         "start":{"dateTime":"2026-08-18T09:00:00Z"},
         "end":{"dateTime":"2026-08-18T09:30:00Z"}},
        {"id":"g-ok","summary":"Ok",
         "start":{"dateTime":"2026-08-18T10:00:00Z"},
         "end":{"dateTime":"2026-08-18T10:30:00Z"}},
        {"id":"g-cancel","status":"cancelled"}
    ],"nextSyncToken":"st-skip"}"#;
    let http = FakeHttp::new(vec![("/events", 200, body)]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    // Pre-seed the in-flight local row with the user's newer title.
    let mut living = living_event("local-inflight", "cal-1", "g-inflight");
    living.title = "User rewrite".to_string();
    events.stored.lock().unwrap().push(living);

    let ops = FakeOperationRepo::with(vec![CalendarEventOperation {
        id: "op-1".to_string(),
        user_id: "u-1".to_string(),
        calendar_id: "cal-1".to_string(),
        local_event_id: "local-inflight".to_string(),
        google_event_id: "g-inflight".to_string(),
        verb: OP_VERB_PATCH.to_string(),
        payload_fingerprint: "fp".to_string(),
        payload_json: "{}".to_string(),
        status: OP_STATUS_PENDING.to_string(),
        google_etag: "e1".to_string(),
        attempt_count: 0,
        last_error: String::new(),
        created_at: "2023-11-14T22:00:00Z".to_string(),
        updated_at: "2023-11-14T22:00:00Z".to_string(),
    }]);
    let cal = calendars.stored.lock().unwrap()[0].clone();

    pollster::block_on(sync_calendar(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        &cal,
        "2023-11-14T22:13:20Z",
    ))
    .unwrap();

    let upserted = events.upserted_batch.lock().unwrap();
    assert!(
        upserted.iter().all(|e| e.google_event_id != "g-inflight"),
        "inflight id must not be upserted: {upserted:?}"
    );
    assert!(
        upserted.iter().any(|e| e.google_event_id == "g-ok"),
        "non-inflight still applied: {upserted:?}"
    );
    let deleted = events.deleted_by_google_event_id.lock().unwrap();
    assert!(
        deleted.iter().all(|(_, id)| id != "g-inflight"),
        "inflight must not soft-delete: {deleted:?}"
    );
    // Cancelled non-inflight still soft-deleted.
    assert!(
        deleted.iter().any(|(_, id)| id == "g-cancel"),
        "cancelled non-inflight still deleted: {deleted:?}"
    );
    // User row title preserved.
    let stored = events.stored.lock().unwrap();
    let inflight = stored
        .iter()
        .find(|e| e.google_event_id == "g-inflight")
        .unwrap();
    assert_eq!(inflight.title, "User rewrite");
}

#[test]
fn replica_merge_full_sweeps_absent_preserves_exceptions_and_task_id() {
    // Successful reseed merge-full: ghost gone; cancelled exception + task-linked
    // absent stay living; seen row's task_id COALESCE preserved.
    let merge = r#"{"items":[
        {"id":"keep","summary":"Keep","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}},
        {"id":"exc-seen","status":"cancelled","recurringEventId":"master-1",
         "originalStartTime":{"dateTime":"2026-08-18T10:00:00Z"},
         "start":{"dateTime":"2026-08-18T10:00:00Z"},"end":{"dateTime":"2026-08-18T10:30:00Z"}}
    ],"nextSyncToken":"st-mf"}"#;
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.sync_token = "old".to_string();
    cal.full_sync_requested = true;
    cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.initial_sync_complete = true;
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();
    let mut cancel_exc = seeded_event("e-exc-absent", "cal-1", "exc-absent", "");
    cancel_exc.status = "cancelled".to_string();
    cancel_exc.recurring_event_id = "master-1".to_string();
    events.stored.lock().unwrap().extend([
        seeded_event("e-keep", "cal-1", "keep", "keep-me"),
        seeded_event("e-ghost", "cal-1", "ghost", ""),
        seeded_event("e-task", "cal-1", "task-linked", "task-99"),
        cancel_exc,
        seeded_event("e-exc-seen", "cal-1", "exc-seen", ""),
    ]);
    let http = FakeHttp::new(vec![("/events", 200, merge)]);
    pollster::block_on(sync_calendar(
        &http,
        &calendars,
        &events,
        &FakeOperationRepo::new(),
        &access(),
        &cal,
        "2023-11-14T22:13:20Z",
    ))
    .unwrap();

    let stored = events.stored.lock().unwrap();
    let keep = stored.iter().find(|e| e.google_event_id == "keep").unwrap();
    assert_eq!(keep.task_id, "keep-me");
    assert!(keep.deleted_at.is_none());
    let ghost = stored.iter().find(|e| e.google_event_id == "ghost").unwrap();
    assert!(ghost.deleted_at.is_some(), "absent ghost must be swept");
    let task = stored
        .iter()
        .find(|e| e.google_event_id == "task-linked")
        .unwrap();
    assert!(
        task.deleted_at.is_none(),
        "task_id row absent from snapshot must stay living"
    );
    assert_eq!(task.task_id, "task-99");
    let exc_absent = stored
        .iter()
        .find(|e| e.google_event_id == "exc-absent")
        .unwrap();
    assert!(
        exc_absent.deleted_at.is_none(),
        "cancelled exception absent from snapshot must stay living"
    );
    assert!(events.deleted_stale.lock().unwrap().is_empty());
    assert_eq!(calendars.stored.lock().unwrap()[0].sync_token, "st-mf");
    assert!(!calendars.stored.lock().unwrap()[0].full_sync_requested);
}

#[test]
fn replica_merge_full_mid_walk_failure_does_not_sweep() {
    // full_sync_requested walk: page1 ok, page2 500 → no sweep, ghost living,
    // token unchanged, flag still set.
    let page_one = r#"{"items":[{"id":"p1","summary":"A","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}}],"nextPageToken":"tok-2"}"#;
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.sync_token = "old-tok".to_string();
    cal.full_sync_requested = true;
    cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.initial_sync_complete = true;
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();
    events
        .stored
        .lock()
        .unwrap()
        .push(seeded_event("e-ghost", "cal-1", "ghost", ""));
    let http = FakeHttp::new(vec![
        ("pageToken=tok-2", 500, ""),
        ("/events", 200, page_one),
    ]);
    let err = pollster::block_on(sync_calendar(
        &http,
        &calendars,
        &events,
        &FakeOperationRepo::new(),
        &access(),
        &cal,
        "2023-11-14T22:13:20Z",
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::GoogleApi(ref m) if m.contains("500")), "{err:?}");
    assert_eq!(calendars.stored.lock().unwrap()[0].sync_token, "old-tok");
    assert!(
        calendars.stored.lock().unwrap()[0].full_sync_requested,
        "flag must remain set after mid-walk failure"
    );
    let ghost = events
        .stored
        .lock()
        .unwrap()
        .iter()
        .find(|e| e.google_event_id == "ghost")
        .unwrap()
        .clone();
    assert!(
        ghost.deleted_at.is_none(),
        "mid-walk failure must never sweep"
    );
    assert!(events.deleted_stale.lock().unwrap().is_empty());
}

#[test]
fn replica_merge_full_lease_lost_before_sweep_does_not_tombstone_or_publish() {
    // One-page reseed: upsert is fenced apply #1; steal lease on sweep (#2).
    // No tombstones; token unchanged.
    let merge = r#"{"items":[
        {"id":"keep","summary":"Keep","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}}
    ],"nextSyncToken":"st-lost"}"#;
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.sync_token = "old-tok".to_string();
    cal.full_sync_requested = true;
    cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.initial_sync_complete = true;
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();
    events.stored.lock().unwrap().extend([
        seeded_event("e-keep", "cal-1", "keep", ""),
        seeded_event("e-ghost", "cal-1", "ghost", ""),
    ]);
    events.gate_applies_on(&calendars);
    *events.inject_lease_before_fenced_apply.lock().unwrap() =
        Some((2, "thief".to_string(), Some("2099-01-01T00:00:00Z".to_string())));
    let http = FakeHttp::new(vec![("/events", 200, merge)]);
    let err = pollster::block_on(sync_calendar(
        &http,
        &calendars,
        &events,
        &FakeOperationRepo::new(),
        &access(),
        &cal,
        "2023-11-14T22:13:20Z",
    ))
    .unwrap_err();
    assert!(
        matches!(err, CalendarError::Invalid(ref m) if m.contains("lost replica lease")),
        "{err:?}"
    );
    assert_eq!(calendars.stored.lock().unwrap()[0].sync_token, "old-tok");
    assert!(
        calendars.stored.lock().unwrap()[0].full_sync_requested,
        "must not clear flag without publish"
    );
    let ghost = events
        .stored
        .lock()
        .unwrap()
        .iter()
        .find(|e| e.google_event_id == "ghost")
        .unwrap()
        .clone();
    assert!(
        ghost.deleted_at.is_none(),
        "lost lease must not tombstone ghosts"
    );
    // Upsert counted + sweep attempt = 2 fenced applies.
    assert_eq!(*events.fenced_apply_count.lock().unwrap(), 2);
    assert!(events.deleted_stale.lock().unwrap().is_empty());
}

#[test]
fn replica_incremental_completed_does_not_sweep_preexisting_ghost() {
    let body = r#"{"items":[
        {"id":"p1","summary":"A","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}}
    ],"nextSyncToken":"st-inc"}"#;
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
        .push(seeded_event("e-ghost", "cal-1", "ghost", ""));
    let http = FakeHttp::new(vec![("/events", 200, body)]);
    pollster::block_on(sync_calendar(
        &http,
        &calendars,
        &events,
        &FakeOperationRepo::new(),
        &access(),
        &cal,
        "2023-11-14T22:13:20Z",
    ))
    .unwrap();
    assert_eq!(calendars.stored.lock().unwrap()[0].sync_token, "st-inc");
    let ghost = events
        .stored
        .lock()
        .unwrap()
        .iter()
        .find(|e| e.google_event_id == "ghost")
        .unwrap()
        .clone();
    assert!(
        ghost.deleted_at.is_none(),
        "incremental must not sweep pre-existing ghost"
    );
    assert!(events.replica_seen.lock().unwrap().is_empty());
    assert!(events.deleted_stale.lock().unwrap().is_empty());
}

#[test]
fn replica_merge_full_records_inflight_as_seen_and_does_not_sweep() {
    use crate::models::{CalendarEventOperation, OP_STATUS_PENDING, OP_VERB_PATCH};

    let merge = r#"{"items":[
        {"id":"keep","summary":"Keep","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}},
        {"id":"g-inflight","summary":"Stale","start":{"dateTime":"2026-08-18T10:00:00Z"},"end":{"dateTime":"2026-08-18T10:30:00Z"}}
    ],"nextSyncToken":"st-inf"}"#;
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.sync_token = "old".to_string();
    cal.full_sync_requested = true;
    cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.initial_sync_complete = true;
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();
    let mut living = living_event("local-inflight", "cal-1", "g-inflight");
    living.title = "User rewrite".to_string();
    events.stored.lock().unwrap().extend([
        seeded_event("e-keep", "cal-1", "keep", ""),
        living,
        seeded_event("e-ghost", "cal-1", "ghost", ""),
    ]);
    let ops = FakeOperationRepo::with(vec![CalendarEventOperation {
        id: "op-1".to_string(),
        user_id: "u-1".to_string(),
        calendar_id: "cal-1".to_string(),
        local_event_id: "local-inflight".to_string(),
        google_event_id: "g-inflight".to_string(),
        verb: OP_VERB_PATCH.to_string(),
        payload_fingerprint: "fp".to_string(),
        payload_json: "{}".to_string(),
        status: OP_STATUS_PENDING.to_string(),
        google_etag: "e1".to_string(),
        attempt_count: 0,
        last_error: String::new(),
        created_at: "2023-11-14T22:00:00Z".to_string(),
        updated_at: "2023-11-14T22:00:00Z".to_string(),
    }]);
    let http = FakeHttp::new(vec![("/events", 200, merge)]);
    pollster::block_on(sync_calendar(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        &cal,
        "2023-11-14T22:13:20Z",
    ))
    .unwrap();

    let stored = events.stored.lock().unwrap();
    let inflight = stored
        .iter()
        .find(|e| e.google_event_id == "g-inflight")
        .unwrap();
    assert!(
        inflight.deleted_at.is_none(),
        "in-flight id recorded as seen must not be swept"
    );
    assert_eq!(inflight.title, "User rewrite");
    let ghost = stored.iter().find(|e| e.google_event_id == "ghost").unwrap();
    assert!(ghost.deleted_at.is_some(), "true ghost still swept");
    assert!(
        events
            .upserted_batch
            .lock()
            .unwrap()
            .iter()
            .all(|e| e.google_event_id != "g-inflight"),
        "inflight must not be upserted"
    );
    assert_eq!(calendars.stored.lock().unwrap()[0].sync_token, "st-inf");
}
