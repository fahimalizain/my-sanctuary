//! Unit tests for session-authorized per-calendar repair (issue #59).

use super::support::*;
use crate::calendar::{
    request_calendar_repair, CalendarError, CalendarRepairStatus, CALENDAR_REPAIR_COOLDOWN_SECS,
};
use crate::repo::CalendarRepo;
use crate::time::unix_secs_to_rfc3339;

const NOW: i64 = 1_700_000_000; // 2023-11-14T22:13:20Z

fn stored(calendars: &FakeCalendarRepo, id: &str) -> crate::models::GoogleCalendar {
    pollster::block_on(calendars.get_by_id(id))
        .unwrap()
        .expect("calendar row present")
}

fn assert_snake_status(body: &crate::calendar::CalendarRepairResponse, expected: &str) {
    let v = serde_json::to_value(body).unwrap();
    assert_eq!(v["status"], expected, "{v}");
    // Never leak tokens / secrets in the public body.
    let obj = v.as_object().unwrap();
    assert!(!obj.contains_key("sync_token"), "{v}");
    assert!(!obj.contains_key("access_token"), "{v}");
}

#[test]
fn wrong_owner_is_not_found_and_leaves_row_untouched() {
    let mut cal = calendar_for_user("u-2", "cal-1", "other@example.com", true);
    cal.sync_token = "tok-secret".to_string();
    cal.full_sync_requested = false;
    let calendars = FakeCalendarRepo::with(vec![cal]);

    let err = pollster::block_on(request_calendar_repair(&calendars, "u-1", "cal-1", NOW))
        .unwrap_err();
    assert!(matches!(err, CalendarError::NotFound), "{err:?}");

    let row = stored(&calendars, "cal-1");
    assert!(!row.full_sync_requested);
    assert_eq!(row.sync_token, "tok-secret");
}

#[test]
fn missing_id_is_not_found() {
    let calendars = FakeCalendarRepo::with(vec![]);
    let err = pollster::block_on(request_calendar_repair(
        &calendars,
        "u-1",
        "cal-missing",
        NOW,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::NotFound), "{err:?}");
}

#[test]
fn unauthorized_owner_is_not_found_not_a_distinct_403() {
    // Same contract as wrong_owner: CalendarError::NotFound only (Worker → 404).
    let cal = calendar_for_user("u-2", "cal-1", "other@example.com", true);
    let calendars = FakeCalendarRepo::with(vec![cal]);
    let err = pollster::block_on(request_calendar_repair(&calendars, "u-1", "cal-1", NOW))
        .unwrap_err();
    assert!(matches!(err, CalendarError::NotFound), "{err:?}");
}

#[test]
fn in_flight_lease_returns_in_progress_and_flags_reseed_without_stealing_lease() {
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.lease_owner = "other".to_string();
    cal.lease_expires_at = Some(unix_secs_to_rfc3339(NOW + 120));
    cal.full_sync_requested = false;
    cal.sync_token = "tok-live".to_string();
    cal.sync_status = "ready".to_string();
    let calendars = FakeCalendarRepo::with(vec![cal]);

    let body = pollster::block_on(request_calendar_repair(&calendars, "u-1", "cal-1", NOW))
        .unwrap();
    assert_eq!(body.status, CalendarRepairStatus::InProgress);
    assert_eq!(body.retry_after_seconds, None);
    assert_snake_status(&body, "in_progress");

    let row = stored(&calendars, "cal-1");
    assert!(row.full_sync_requested, "failed in-flight must still reseed");
    assert_eq!(row.sync_status, "rebuilding");
    assert_eq!(row.sync_token, "tok-live");
    assert_eq!(row.lease_owner, "other", "must not steal / start a walk");
    assert_eq!(
        row.lease_expires_at.as_deref(),
        Some(unix_secs_to_rfc3339(NOW + 120).as_str())
    );
}

#[test]
fn already_flagged_no_lease_returns_queued_coalesced() {
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.full_sync_requested = true;
    cal.sync_status = "rebuilding".to_string();
    cal.sync_token = "tok-keep".to_string();
    cal.lease_owner = String::new();
    cal.lease_expires_at = None;
    let calendars = FakeCalendarRepo::with(vec![cal]);

    let body = pollster::block_on(request_calendar_repair(&calendars, "u-1", "cal-1", NOW))
        .unwrap();
    assert_eq!(body.status, CalendarRepairStatus::Queued);
    assert_eq!(body.retry_after_seconds, None);
    assert_snake_status(&body, "queued");

    let row = stored(&calendars, "cal-1");
    assert!(row.full_sync_requested);
    assert_eq!(row.sync_token, "tok-keep");
}

#[test]
fn expired_lease_is_not_in_flight_and_queues_reseed() {
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.lease_owner = "stale-owner".to_string();
    cal.lease_expires_at = Some(unix_secs_to_rfc3339(NOW - 30));
    cal.full_sync_requested = false;
    cal.sync_token = "tok-stale".to_string();
    cal.last_success_at = None;
    let calendars = FakeCalendarRepo::with(vec![cal]);

    let body = pollster::block_on(request_calendar_repair(&calendars, "u-1", "cal-1", NOW))
        .unwrap();
    assert_eq!(body.status, CalendarRepairStatus::Queued);
    assert_eq!(body.retry_after_seconds, None);
    assert_snake_status(&body, "queued");

    let row = stored(&calendars, "cal-1");
    assert!(row.full_sync_requested);
    assert_eq!(row.sync_status, "rebuilding");
    assert_eq!(row.sync_token, "tok-stale");
    // begin_replica_reseed does not clear the lease; cron acquires next.
    assert_eq!(row.lease_owner, "stale-owner");
}

#[test]
fn cooldown_after_recent_success_does_not_persist() {
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.full_sync_requested = false;
    cal.sync_token = "tok-fresh".to_string();
    cal.last_success_at = Some(unix_secs_to_rfc3339(NOW - 10));
    cal.sync_status = "ready".to_string();
    let calendars = FakeCalendarRepo::with(vec![cal]);

    let body = pollster::block_on(request_calendar_repair(&calendars, "u-1", "cal-1", NOW))
        .unwrap();
    assert_eq!(body.status, CalendarRepairStatus::Cooldown);
    assert_eq!(
        body.retry_after_seconds,
        Some(CALENDAR_REPAIR_COOLDOWN_SECS - 10)
    );
    assert_snake_status(&body, "cooldown");

    let row = stored(&calendars, "cal-1");
    assert!(!row.full_sync_requested, "cooldown must not persist");
    assert_eq!(row.sync_token, "tok-fresh");
    assert_eq!(row.sync_status, "ready");
}

#[test]
fn retrying_calendar_with_old_success_is_allowed() {
    // Explicit repair escalates a failed/retrying calendar to reseed even when
    // last_attempt is recent — cooldown keys on last_success_at only.
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.sync_status = "retrying".to_string();
    cal.last_success_at = Some(unix_secs_to_rfc3339(NOW - 2 * 60 * 60));
    cal.last_attempt_at = Some(unix_secs_to_rfc3339(NOW - 10));
    cal.full_sync_requested = false;
    cal.sync_token = "tok-retry".to_string();
    let calendars = FakeCalendarRepo::with(vec![cal]);

    let body = pollster::block_on(request_calendar_repair(&calendars, "u-1", "cal-1", NOW))
        .unwrap();
    assert_eq!(body.status, CalendarRepairStatus::Queued);
    assert_eq!(body.retry_after_seconds, None);

    let row = stored(&calendars, "cal-1");
    assert!(row.full_sync_requested);
    assert_eq!(row.sync_status, "rebuilding");
    assert_eq!(row.sync_token, "tok-retry");
}

#[test]
fn happy_path_first_repair_queues_reseed() {
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.sync_status = "ready".to_string();
    cal.full_sync_requested = false;
    cal.sync_token = "tok-ok".to_string();
    cal.last_success_at = Some(unix_secs_to_rfc3339(NOW - 3600));
    let calendars = FakeCalendarRepo::with(vec![cal]);

    let body = pollster::block_on(request_calendar_repair(&calendars, "u-1", "cal-1", NOW))
        .unwrap();
    assert_eq!(body.status, CalendarRepairStatus::Queued);
    assert_eq!(body.retry_after_seconds, None);
    assert_snake_status(&body, "queued");

    let row = stored(&calendars, "cal-1");
    assert!(row.full_sync_requested);
    assert_eq!(row.sync_status, "rebuilding");
    assert_eq!(row.sync_token, "tok-ok");
}

#[test]
fn free_busy_reader_is_invalid() {
    let mut cal = calendar("cal-1", "fb@example.com", true);
    cal.access_role = "freeBusyReader".to_string();
    cal.sync_token = "tok-fb".to_string();
    let calendars = FakeCalendarRepo::with(vec![cal]);

    let err = pollster::block_on(request_calendar_repair(&calendars, "u-1", "cal-1", NOW))
        .unwrap_err();
    match err {
        CalendarError::Invalid(msg) => {
            assert_eq!(msg, "calendar does not support replica repair");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    let row = stored(&calendars, "cal-1");
    assert!(!row.full_sync_requested);
    assert_eq!(row.sync_token, "tok-fb");
}

#[test]
fn authorization_required_is_invalid() {
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.sync_status = "authorization_required".to_string();
    cal.sync_token = "tok-auth".to_string();
    let calendars = FakeCalendarRepo::with(vec![cal]);

    let err = pollster::block_on(request_calendar_repair(&calendars, "u-1", "cal-1", NOW))
        .unwrap_err();
    match err {
        CalendarError::Invalid(msg) => {
            assert_eq!(msg, "calendar requires reauthorization");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    let row = stored(&calendars, "cal-1");
    assert!(!row.full_sync_requested);
    assert_eq!(row.sync_token, "tok-auth");
}

#[test]
fn sync_enabled_false_is_invalid() {
    let mut cal = calendar("cal-1", "primary@example.com", false);
    cal.sync_token = "tok-off".to_string();
    let calendars = FakeCalendarRepo::with(vec![cal]);

    let err = pollster::block_on(request_calendar_repair(&calendars, "u-1", "cal-1", NOW))
        .unwrap_err();
    match err {
        CalendarError::Invalid(msg) => {
            assert_eq!(msg, "calendar sync is disabled");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    let row = stored(&calendars, "cal-1");
    assert!(!row.full_sync_requested);
    assert_eq!(row.sync_token, "tok-off");
}
