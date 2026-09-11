//! Walk instrumentation + cron operator-warning integration tests.

use super::support::*;
use crate::calendar::diagnostics::{
    CheckpointResult, OperatorWarningLevel, ReplicaWalkPhase, ReplicaWalkTrigger,
};
use crate::calendar::{
    run_fallback_cron, sync_calendar_traced, CalendarError, ReplicaWalkMeta, SyncCalendarOutcome,
};
use crate::calendar_sync::SyncErrorCode;
use crate::time::unix_secs_to_rfc3339;

#[test]
fn traced_paginated_walk_counts_and_sanitizes_diagnostic() {
    let page_one = r#"{"items":[{"id":"p1","summary":"SecretStandupTitle","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}}],"nextPageToken":"tok-2"}"#;
    let page_two = r#"{"items":[{"id":"p2","summary":"AnotherSecret","start":{"dateTime":"2026-08-18T10:00:00Z"},"end":{"dateTime":"2026-08-18T10:30:00Z"}}],"nextSyncToken":"st-final"}"#;
    let now = "2023-11-14T22:13:20Z";

    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.sync_token = "old-tok".to_string();
    cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.initial_sync_complete = true;
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let events = FakeEventRepo::new();
    let http = FakeHttp::new(vec![
        ("pageToken=tok-2", 200, page_two),
        ("/events", 200, page_one),
    ]);

    let result = pollster::block_on(sync_calendar_traced(
        &http,
        &calendars,
        &events,
        &FakeOperationRepo::new(),
        &access(),
        &cal,
        now,
        ReplicaWalkMeta {
            run_id: "run-paginated-test".into(),
            trigger: ReplicaWalkTrigger::Cron,
            deployed_version: "9.9.9".into(),
            started_unix_ms: NOW_UNIX.saturating_mul(1000),
        },
    ));

    assert!(
        matches!(result.outcome, Ok(SyncCalendarOutcome::Published)),
        "{:?}",
        result.outcome
    );
    let d = &result.diagnostic;
    assert_eq!(d.run_id, "run-paginated-test");
    assert_eq!(d.trigger, ReplicaWalkTrigger::Cron);
    assert_eq!(d.calendar_id, "cal-1");
    assert_eq!(d.deployed_version, "9.9.9");
    assert_eq!(d.phase, ReplicaWalkPhase::Done);
    assert!(d.pages >= 2, "pages={}", d.pages);
    assert!(d.upserts >= 2, "upserts={}", d.upserts);
    assert!(d.attempts >= 2, "attempts={}", d.attempts);
    assert_eq!(d.checkpoint, CheckpointResult::Published);
    assert_eq!(d.error_category, None);

    let json = serde_json::to_string(d).unwrap();
    for needle in [
        "run-paginated-test",
        "cron",
        "cal-1",
        "9.9.9",
        "done",
        "published",
    ] {
        assert!(json.contains(needle), "missing {needle} in {json}");
    }
    for secret in [
        "old-tok",
        "st-final",
        "tok-2",
        "SecretStandupTitle",
        "AnotherSecret",
        "https://www.googleapis.com",
        "syncToken",
        "pageToken",
        "access_token",
        "raw_json",
    ] {
        assert!(!json.contains(secret), "leaked {secret} in {json}");
    }
}

#[test]
fn traced_missing_terminal_token_sets_checkpoint_and_preserves_success() {
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

    let result = pollster::block_on(sync_calendar_traced(
        &http,
        &calendars,
        &events,
        &FakeOperationRepo::new(),
        &access(),
        &cal,
        "2023-11-14T22:13:20Z",
        ReplicaWalkMeta {
            run_id: "run-missing-token".into(),
            trigger: ReplicaWalkTrigger::Unspecified,
            deployed_version: String::new(),
            started_unix_ms: 0,
        },
    ));

    assert!(
        matches!(
            result.outcome,
            Err(CalendarError::InvalidResponse(ref m)) if m.contains("missing nextSyncToken")
        ),
        "{:?}",
        result.outcome
    );
    assert_eq!(
        result.diagnostic.checkpoint,
        CheckpointResult::MissingSyncToken
    );
    assert_eq!(
        result.diagnostic.error_category,
        Some(SyncErrorCode::MissingSyncToken)
    );
    assert_eq!(result.diagnostic.calendar_id, "cal-1");
    assert_eq!(result.diagnostic.duration_ms, 0);

    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].sync_token, "old-tok");
    assert_eq!(
        stored[0].last_success_at.as_deref(),
        Some("2023-11-14T21:00:00Z")
    );
    assert_eq!(stored[0].last_error_code, "missing_sync_token");
}

#[test]
fn cron_warns_stale_and_auth_even_when_not_replica_due() {
    // Stale calendar: last_success 2h ago, not dirty, next_retry far future
    // → replica_due is false (backoff gate), but operator warning must still fire.
    let mut stale = calendar("cal-stale", "stale@example.com", true);
    stale.last_success_at = Some(unix_secs_to_rfc3339(NOW_UNIX - 2 * 60 * 60));
    stale.last_synced_at = stale.last_success_at.clone();
    stale.initial_sync_complete = true;
    stale.sync_status = "ready".to_string();
    stale.dirty_requested_generation = 0;
    stale.dirty_applied_generation = 0;
    stale.full_sync_requested = false;
    stale.next_retry_at = Some(unix_secs_to_rfc3339(NOW_UNIX + 3600));

    // Auth-required calendar: replica_due skips it; warning is immediate.
    let mut auth = calendar("cal-auth", "auth@example.com", true);
    auth.last_success_at = Some(unix_secs_to_rfc3339(NOW_UNIX - 60));
    auth.last_synced_at = auth.last_success_at.clone();
    auth.initial_sync_complete = true;
    auth.sync_status = "authorization_required".to_string();
    auth.last_error_code = "auth_revoked".to_string();
    auth.dirty_requested_generation = 0;
    auth.dirty_applied_generation = 0;
    auth.next_retry_at = Some(unix_secs_to_rfc3339(NOW_UNIX + 3600));

    // Fresh healthy calendar should not warn.
    let mut fresh = calendar("cal-fresh", "fresh@example.com", true);
    fresh.last_success_at = Some(unix_secs_to_rfc3339(NOW_UNIX - 10 * 60));
    fresh.last_synced_at = fresh.last_success_at.clone();
    fresh.initial_sync_complete = true;
    fresh.sync_status = "ready".to_string();
    fresh.next_retry_at = Some(unix_secs_to_rfc3339(NOW_UNIX + 3600));

    let http = FakeHttp::new(vec![]);
    let calendars = FakeCalendarRepo::with(vec![stale, auth, fresh]);
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

    assert_eq!(report.synced, 0, "no replica walks expected");
    assert!(
        report.diagnostics.is_empty(),
        "no due walks: {:?}",
        report.diagnostics
    );

    let stale_w = report
        .warnings
        .iter()
        .find(|w| w.calendar_id == "cal-stale")
        .expect("stale warning");
    assert_eq!(stale_w.level, OperatorWarningLevel::Stale);

    let auth_w = report
        .warnings
        .iter()
        .find(|w| w.calendar_id == "cal-auth")
        .expect("auth warning");
    assert_eq!(auth_w.level, OperatorWarningLevel::AuthorizationRequired);

    assert!(
        report
            .warnings
            .iter()
            .all(|w| w.calendar_id != "cal-fresh"),
        "fresh calendar must not warn: {:?}",
        report.warnings
    );

    // No events.list for non-due calendars.
    let gets = http.gets.lock().unwrap();
    assert!(
        gets.iter().all(|u| !u.contains("/events")),
        "must not hit events.list: {gets:?}"
    );
}
