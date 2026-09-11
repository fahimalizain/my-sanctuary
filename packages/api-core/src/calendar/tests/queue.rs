use super::support::*;
use crate::calendar::{run_queue_sync, QueueSyncAction};
use crate::time::{unix_secs_to_rfc3339, FrozenClock};

fn clock() -> FrozenClock {
    FrozenClock(NOW_UNIX)
}

/// Dirty calendar with a fresh last_success so the walk is dirty-driven
/// (not stale-driven).
fn dirty_cal(requested: i64, applied: i64) -> crate::models::GoogleCalendar {
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.last_success_at = Some("2023-11-14T22:12:20Z".to_string());
    cal.last_synced_at = cal.last_success_at.clone();
    cal.dirty_requested_generation = requested;
    cal.dirty_applied_generation = applied;
    cal
}

// ──────────────────────────────────────────
// Publish outcomes
// ──────────────────────────────────────────

#[test]
fn queue_published_clean_acks() {
    let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
    let cal = dirty_cal(1, 0);
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let report = pollster::block_on(run_queue_sync(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        &cal,
        NOW_UNIX,
        &clock(),
    ));

    assert_eq!(report.action, QueueSyncAction::Ack);
    assert!(report.published);
    assert!(report.diagnostic.is_some());
    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].dirty_applied_generation, 1);
    assert_eq!(stored[0].dirty_requested_generation, 1);
    let gets = http.gets.lock().unwrap();
    let event_gets: Vec<_> = gets.iter().filter(|u| u.contains("/events")).collect();
    assert_eq!(event_gets.len(), 1, "{gets:?}");
}

#[test]
fn queue_published_with_mid_walk_bump_reenqueues_once() {
    let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
    let cal = dirty_cal(5, 1);
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    // run_queue_sync pre-read is get_by_id #1; under-lease snapshot is #2.
    *calendars.bump_dirty_after_get_by_id.lock().unwrap() = Some(2);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let report = pollster::block_on(run_queue_sync(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        &cal,
        NOW_UNIX,
        &clock(),
    ));

    assert_eq!(report.action, QueueSyncAction::Reenqueue);
    assert!(report.published);
    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].dirty_applied_generation, 5, "snapshot at start");
    assert_eq!(
        stored[0].dirty_requested_generation, 6,
        "mid-walk bump preserved"
    );
}

#[test]
fn queue_multiple_mid_walk_bumps_single_reenqueue() {
    let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
    let cal = dirty_cal(5, 1);
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    *calendars.bump_dirty_after_get_by_id.lock().unwrap() = Some(2);
    // Jump requested 5 → 7 in one hook fire (coalesce to one Reenqueue).
    *calendars.bump_dirty_by.lock().unwrap() = 2;
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let report = pollster::block_on(run_queue_sync(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        &cal,
        NOW_UNIX,
        &clock(),
    ));

    assert_eq!(report.action, QueueSyncAction::Reenqueue);
    assert!(report.published);
    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].dirty_applied_generation, 5);
    assert_eq!(stored[0].dirty_requested_generation, 7);
}

// ──────────────────────────────────────────
// Lease / failure
// ──────────────────────────────────────────

#[test]
fn queue_lease_busy_acks_without_google_or_dirty_change() {
    let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
    let cal = dirty_cal(1, 0);
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    calendars.force_lease("cal-1", "other-owner", Some("2099-01-01T00:00:00Z"));
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let report = pollster::block_on(run_queue_sync(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        &cal,
        NOW_UNIX,
        &clock(),
    ));

    assert_eq!(report.action, QueueSyncAction::Ack);
    assert!(!report.published);
    assert!(report.diagnostic.is_some());
    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].dirty_requested_generation, 1);
    assert_eq!(stored[0].dirty_applied_generation, 0);
    let gets = http.gets.lock().unwrap();
    assert!(
        gets.iter().all(|u| !u.contains("/events")),
        "lease busy must not hit events.list: {gets:?}"
    );
}

#[test]
fn queue_sync_failure_acks_records_next_retry() {
    let http = FakeHttp::new(vec![("/events", 500, r#"{"error":"boom"}"#)]);
    let cal = dirty_cal(1, 0);
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let report = pollster::block_on(run_queue_sync(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        &cal,
        NOW_UNIX,
        &clock(),
    ));

    assert_eq!(report.action, QueueSyncAction::Ack);
    assert!(!report.published);
    assert!(report.diagnostic.is_some());
    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].dirty_applied_generation, 0);
    assert_eq!(stored[0].dirty_requested_generation, 1);
    assert!(
        stored[0].next_retry_at.is_some(),
        "failure must stamp next_retry_at"
    );
}

// ──────────────────────────────────────────
// Skip gates
// ──────────────────────────────────────────

#[test]
fn queue_missing_calendar_acks_no_work() {
    let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
    let cal = calendar("cal-missing", "primary@example.com", true);
    let calendars = FakeCalendarRepo::with(vec![]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let report = pollster::block_on(run_queue_sync(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        &cal,
        NOW_UNIX,
        &clock(),
    ));

    assert_eq!(report.action, QueueSyncAction::Ack);
    assert!(!report.published);
    assert!(report.diagnostic.is_none());
    let gets = http.gets.lock().unwrap();
    assert!(
        gets.iter().all(|u| !u.contains("/events")),
        "missing calendar must not hit events.list: {gets:?}"
    );
}

#[test]
fn queue_disabled_calendar_acks_no_work() {
    let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
    let mut cal = dirty_cal(1, 0);
    cal.sync_enabled = false;
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let report = pollster::block_on(run_queue_sync(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        &cal,
        NOW_UNIX,
        &clock(),
    ));

    assert_eq!(report.action, QueueSyncAction::Ack);
    assert!(!report.published);
    assert!(report.diagnostic.is_none());
    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].dirty_requested_generation, 1);
    assert_eq!(stored[0].dirty_applied_generation, 0);
    let gets = http.gets.lock().unwrap();
    assert!(
        gets.iter().all(|u| !u.contains("/events")),
        "disabled calendar must not hit events.list: {gets:?}"
    );
}

#[test]
fn queue_soft_deleted_calendar_acks_no_work() {
    let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
    let mut cal = dirty_cal(1, 0);
    cal.deleted_at = Some(unix_secs_to_rfc3339(NOW_UNIX - 60));
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let report = pollster::block_on(run_queue_sync(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        &cal,
        NOW_UNIX,
        &clock(),
    ));

    assert_eq!(report.action, QueueSyncAction::Ack);
    assert!(!report.published);
    assert!(report.diagnostic.is_none());
    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].dirty_requested_generation, 1);
    assert_eq!(stored[0].dirty_applied_generation, 0);
    let gets = http.gets.lock().unwrap();
    assert!(
        gets.iter().all(|u| !u.contains("/events")),
        "soft-deleted calendar must not hit events.list: {gets:?}"
    );
}

#[test]
fn queue_already_clean_acks_no_google() {
    let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
    let cal = dirty_cal(1, 1);
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let report = pollster::block_on(run_queue_sync(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        &cal,
        NOW_UNIX,
        &clock(),
    ));

    assert_eq!(report.action, QueueSyncAction::Ack);
    assert!(!report.published);
    assert!(report.diagnostic.is_none());
    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].dirty_requested_generation, 1);
    assert_eq!(stored[0].dirty_applied_generation, 1);
    let gets = http.gets.lock().unwrap();
    assert!(
        gets.iter().all(|u| !u.contains("/events")),
        "already-clean must not hit events.list: {gets:?}"
    );
}

#[test]
fn queue_backoff_acks_no_google() {
    let http = FakeHttp::new(vec![("/events", 200, EVENTS_JSON)]);
    let mut cal = dirty_cal(2, 0);
    cal.next_retry_at = Some(unix_secs_to_rfc3339(NOW_UNIX + 3600));
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let report = pollster::block_on(run_queue_sync(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        &cal,
        NOW_UNIX,
        &clock(),
    ));

    assert_eq!(report.action, QueueSyncAction::Ack);
    assert!(!report.published);
    assert!(report.diagnostic.is_none());
    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].dirty_requested_generation, 2);
    assert_eq!(stored[0].dirty_applied_generation, 0);
    let gets = http.gets.lock().unwrap();
    assert!(
        gets.iter().all(|u| !u.contains("/events")),
        "backoff must not hit events.list: {gets:?}"
    );
}
