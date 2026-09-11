use super::support::*;
use crate::calendar::catalog::refresh_calendar_list;
use crate::calendar::{
    list_calendars, list_calendars_after_refresh_failure, run_fallback_cron, CalendarView,
    CalendarsResponse,
};
use crate::oauth::HttpError;
use crate::repo::CalendarRepo;
use crate::time::unix_secs_to_rfc3339;
use crate::token::TokenError;

// ──────────────────────────────────────────
// list_calendars
// ──────────────────────────────────────────

#[test]
fn list_calendars_empty_store_imports_calendar_list_without_syncing_events() {
    // Only a calendarList route + the FakeHttp default for bare
    // `calendars.get` backfill URLs: any events.list or watch call would
    // make the fake panic — this test proves list_calendars does neither.
    let http = FakeHttp::new(vec![("calendarList", 200, CALENDAR_LIST_JSON)]);
    let calendars = FakeCalendarRepo::with(vec![]);

    let output = pollster::block_on(list_calendars(
        &http,
        &calendars,
        &access(),
        "u-1",
        "2026-08-17T00:00:00Z",
    ))
    .unwrap();

    let views = output.calendars;
    assert_eq!(views.len(), 2);
    assert_eq!(views[0].google_calendar_id, "primary@example.com");
    assert_eq!(
        views[1].google_calendar_id,
        "en.usa#holiday@group.v.calendar.google.com"
    );
    assert_eq!(views[0].summary, "Work");
    assert_eq!(views[1].summary, "Holidays");
    assert!(views[0].is_primary, "fixture marks the primary calendar");
    assert_eq!(views[0].access_role, "owner");
    assert!(!views[1].is_primary);
    assert_eq!(views[1].access_role, "reader");

    // The calendarList import plus one `calendars.get` event-label
    // backfill per imported row (the FakeHttp default answers them) —
    // and nothing else.
    let gets = http.gets.lock().unwrap();
    assert_eq!(
        gets.len(),
        3,
        "calendarList import + 2 event-label backfills"
    );
    assert!(gets[0].contains("calendarList"), "{gets:?}");
    assert!(
        gets[1..].iter().all(|url| url.contains("/calendar/v3/calendars/")
            && !url.contains("/events")
            && !url.contains("calendarList")),
        "backfills hit the bare calendar resource: {gets:?}"
    );
    assert!(http.posts.lock().unwrap().is_empty(), "no watch POSTs");
    assert_eq!(calendars.upserted.lock().unwrap().len(), 2);
    assert!(
        calendars.sync_states.lock().unwrap().is_empty(),
        "no event sync"
    );
    // Every imported row got the backfilled cache (`[]` = fetched, no
    // labels — the FakeHttp default body has no labels).
    assert_eq!(calendars.label_updates.lock().unwrap().len(), 2);
    assert!(
        calendars
            .stored
            .lock()
            .unwrap()
            .iter()
            .all(|cal| cal.event_labels == "[]"),
        "all imported rows have a fetched label cache"
    );
}

#[test]
fn list_calendars_non_empty_store_is_cache_only() {
    // No routes: any HTTP call would make the fake panic.
    let http = FakeHttp::new(vec![]);
    let calendars =
        FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);

    let output = pollster::block_on(list_calendars(
        &http,
        &calendars,
        &access(),
        "u-1",
        "2026-08-17T00:00:00Z",
    ))
    .unwrap();

    assert!(
        http.gets.lock().unwrap().is_empty(),
        "no Google calls for a cached store"
    );
    let views = output.calendars;
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].id, "cal-1");
    assert_eq!(views[0].google_calendar_id, "primary@example.com");
    assert_eq!(views[0].summary, "Work");
    assert!(views[0].is_primary);
    assert!(views[0].sync_enabled);
    assert!(
        calendars.upserted.lock().unwrap().is_empty(),
        "no re-import"
    );
}

#[test]
fn list_calendars_import_error_propagates() {
    let http = FakeHttp::new(vec![("calendarList", 500, "nope")]);
    let calendars = FakeCalendarRepo::with(vec![]);

    let err = pollster::block_on(list_calendars(
        &http,
        &calendars,
        &access(),
        "u-1",
        "2026-08-17T00:00:00Z",
    ))
    .unwrap_err();

    assert!(err.to_string().contains("calendarList fetch"), "{err}");
}

#[test]
fn list_calendars_view_json_omits_sync_internals() {
    let json = serde_json::to_value(CalendarsResponse {
        calendars: vec![CalendarView::from(calendar(
            "cal-1",
            "primary@example.com",
            true,
        ))],
    })
    .unwrap();
    let object = json.as_object().unwrap();
    let view = object["calendars"][0].as_object().unwrap();
    for key in [
        "id",
        "google_calendar_id",
        "summary",
        "time_zone",
        "is_primary",
        "access_role",
        "sync_enabled",
    ] {
        assert!(view.contains_key(key), "missing picker field {key}");
    }
    for key in ["sync_token", "last_synced_at", "deleted_at"] {
        assert!(
            !view.contains_key(key),
            "sync internals must stay hidden: {key}"
        );
    }
}

// ──────────────────────────────────────────
// list_calendars_after_refresh_failure
// ──────────────────────────────────────────

fn ready_synced_calendar(
    id: &str,
    google_cal_id: &str,
    sync_enabled: bool,
) -> crate::models::GoogleCalendar {
    let mut cal = calendar(id, google_cal_id, sync_enabled);
    cal.last_synced_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.last_success_at = Some("2023-11-14T21:00:00Z".to_string());
    cal.initial_sync_complete = true;
    cal.sync_status = "ready".to_string();
    cal.sync_token = "cursor-secret-xyz".to_string();
    cal
}

#[test]
fn refresh_failure_revoked_grant_serves_cached_calendars_and_stamps() {
    let enabled = ready_synced_calendar("cal-1", "primary@example.com", true);
    let disabled = ready_synced_calendar("cal-disabled", "disabled@example.com", false);
    let calendars = FakeCalendarRepo::with(vec![enabled, disabled]);

    let refresh_err = TokenError::Http(HttpError::Message(
        "POST https://oauth2.googleapis.com/token returned 400: {\"error\":\"invalid_grant\"}"
            .into(),
    ));
    let output = pollster::block_on(list_calendars_after_refresh_failure(
        &calendars,
        "u-1",
        NOW_UNIX,
        &refresh_err,
    ))
    .unwrap();

    assert_eq!(output.calendars.len(), 2);
    let enabled_view = output
        .calendars
        .iter()
        .find(|c| c.id == "cal-1")
        .expect("enabled calendar in picker");
    let disabled_view = output
        .calendars
        .iter()
        .find(|c| c.id == "cal-disabled")
        .expect("disabled calendar still listed");
    assert!(enabled_view.sync_enabled);
    assert!(!disabled_view.sync_enabled);

    let stored = calendars.stored.lock().unwrap();
    let cal1 = stored.iter().find(|c| c.id == "cal-1").unwrap();
    assert_eq!(cal1.sync_status, "authorization_required");
    assert_eq!(cal1.last_error_code, "auth_revoked");
    let cal_disabled = stored.iter().find(|c| c.id == "cal-disabled").unwrap();
    assert_eq!(cal_disabled.sync_status, "ready");
    assert!(
        calendars.upserted.lock().unwrap().is_empty(),
        "no Google import on refresh failure"
    );

    let json = serde_json::to_string(&output).unwrap();
    assert!(!json.contains("sync_token"), "{json}");
    assert!(!json.contains("access_token"), "{json}");
    assert!(!json.contains("invalid_grant"), "{json}");
    assert!(!json.contains("cursor-secret-xyz"), "{json}");
}

#[test]
fn refresh_failure_no_token_does_not_stamp() {
    let cal = ready_synced_calendar("cal-1", "primary@example.com", true);
    let calendars = FakeCalendarRepo::with(vec![cal]);

    let output = pollster::block_on(list_calendars_after_refresh_failure(
        &calendars,
        "u-1",
        NOW_UNIX,
        &TokenError::NoToken,
    ))
    .unwrap();

    assert_eq!(output.calendars.len(), 1);
    assert_eq!(output.calendars[0].id, "cal-1");
    assert_eq!(calendars.stored.lock().unwrap()[0].sync_status, "ready");
    assert_ne!(
        calendars.stored.lock().unwrap()[0].sync_status,
        "authorization_required"
    );

    let json = serde_json::to_string(&output).unwrap();
    assert!(!json.contains("sync_token"), "{json}");
    assert!(!json.contains("access_token"), "{json}");
    assert!(!json.contains("invalid_grant"), "{json}");
}

#[test]
fn refresh_failure_revoked_empty_store_still_ok() {
    let calendars = FakeCalendarRepo::with(vec![]);

    let refresh_err = TokenError::Http(HttpError::Message("invalid_grant".into()));
    let output = pollster::block_on(list_calendars_after_refresh_failure(
        &calendars,
        "u-1",
        NOW_UNIX,
        &refresh_err,
    ))
    .unwrap();

    assert!(output.calendars.is_empty());
    assert!(
        calendars.upserted.lock().unwrap().is_empty(),
        "empty store must not import on refresh failure"
    );

    let json = serde_json::to_string(&output).unwrap();
    assert!(!json.contains("sync_token"), "{json}");
    assert!(!json.contains("access_token"), "{json}");
    assert!(!json.contains("invalid_grant"), "{json}");
}


// ──────────────────────────────────────────
// refresh_calendar_list (incremental)
// ──────────────────────────────────────────

#[test]
fn calendar_list_incremental_new_calendar_appears() {
    let now = unix_secs_to_rfc3339(NOW_UNIX);
    let body = r#"{
        "items": [
            {"id": "new@example.com", "summary": "New", "timeZone": "UTC", "accessRole": "owner"}
        ],
        "nextSyncToken": "tok-2"
    }"#;
    let http = FakeHttp::new(vec![("calendarList", 200, body)]);
    let calendars =
        FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    // with() seeds "test-list-token"; set the token the fixture expects.
    pollster::block_on(calendars.set_calendar_list_sync_token("u-1", "tok-1", &now)).unwrap();
    let watches = FakeWatchChannelRepo::new();

    let new_ids = pollster::block_on(refresh_calendar_list(
        &http,
        &calendars,
        Some(&watches),
        &access(),
        "u-1",
        &now,
    ))
    .unwrap();

    assert_eq!(new_ids.len(), 1, "one newly inserted local id");
    let stored = calendars.stored.lock().unwrap();
    let primary = stored
        .iter()
        .find(|c| c.google_calendar_id == "primary@example.com")
        .unwrap();
    assert!(primary.sync_enabled);
    assert!(primary.deleted_at.is_none());
    let newbie = stored
        .iter()
        .find(|c| c.google_calendar_id == "new@example.com")
        .unwrap();
    assert!(newbie.sync_enabled);
    assert!(newbie.deleted_at.is_none());
    assert_eq!(newbie.id, new_ids[0]);
    drop(stored);
    let token = pollster::block_on(calendars.get_calendar_list_sync_token("u-1"))
        .unwrap()
        .unwrap();
    assert_eq!(token, "tok-2");
    let gets = http.gets.lock().unwrap();
    assert!(gets[0].contains("syncToken=tok-1"), "{gets:?}");
    assert!(gets[0].contains("showDeleted=true"), "{gets:?}");
}

#[test]
fn calendar_list_incremental_deleted_disables_and_stops_watches() {
    let now = unix_secs_to_rfc3339(NOW_UNIX);
    let body = r#"{
        "items": [
            {"id": "primary@example.com", "deleted": true}
        ],
        "nextSyncToken": "tok-del"
    }"#;
    let http = FakeHttp::new(vec![
        ("calendarList", 200, body),
        ("/channels/stop", 200, "{}"),
    ]);
    let calendars =
        FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    pollster::block_on(calendars.set_calendar_list_sync_token("u-1", "tok-1", &now)).unwrap();
    // Preload an event so soft-delete of the calendar does not wipe events.
    let events = FakeEventRepo::new();
    events
        .stored
        .lock()
        .unwrap()
        .push(seeded_event("ev-1", "cal-1", "g-1", ""));
    let watches =
        FakeWatchChannelRepo::with(vec![watch_channel("cal-1", "2023-11-21T22:13:20Z")]);

    let dirty_before = calendars.stored.lock().unwrap()[0].dirty_requested_generation;

    pollster::block_on(refresh_calendar_list(
        &http,
        &calendars,
        Some(&watches),
        &access(),
        "u-1",
        &now,
    ))
    .unwrap();

    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].sync_enabled, false);
    assert!(stored[0].deleted_at.is_some());
    assert_eq!(
        stored[0].dirty_requested_generation, dirty_before,
        "deleted list entry must not bump dirty"
    );
    drop(stored);
    assert_eq!(
        *watches.deleted_by_calendar_id.lock().unwrap(),
        vec!["cal-1".to_string()]
    );
    let posts = http.posts.lock().unwrap();
    assert!(
        posts.iter().any(|(u, _)| u.contains("/channels/stop")),
        "watch stop POST expected: {posts:?}"
    );
    // Events repo not wiped.
    let evs = events.stored.lock().unwrap();
    assert_eq!(evs.len(), 1);
    assert!(evs[0].deleted_at.is_none());
}

#[test]
fn calendar_list_incremental_absence_does_not_delete() {
    let now = unix_secs_to_rfc3339(NOW_UNIX);
    let body = r#"{"items":[],"nextSyncToken":"tok-empty"}"#;
    let http = FakeHttp::new(vec![("calendarList", 200, body)]);
    let calendars =
        FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    pollster::block_on(calendars.set_calendar_list_sync_token("u-1", "tok-1", &now)).unwrap();
    let watches = FakeWatchChannelRepo::new();

    pollster::block_on(refresh_calendar_list(
        &http,
        &calendars,
        Some(&watches),
        &access(),
        "u-1",
        &now,
    ))
    .unwrap();

    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored.len(), 1);
    assert!(stored[0].sync_enabled);
    assert!(stored[0].deleted_at.is_none());
    drop(stored);
    let token = pollster::block_on(calendars.get_calendar_list_sync_token("u-1"))
        .unwrap()
        .unwrap();
    assert_eq!(token, "tok-empty");
}

#[test]
fn calendar_list_full_absence_orphans() {
    let now = unix_secs_to_rfc3339(NOW_UNIX);
    let body = r#"{
        "items": [
            {"id": "b@example.com", "summary": "B", "accessRole": "owner"}
        ],
        "nextSyncToken": "tok-full"
    }"#;
    let http = FakeHttp::new(vec![
        ("calendarList", 200, body),
        ("/channels/stop", 200, "{}"),
    ]);
    let calendars = FakeCalendarRepo::with(vec![
        calendar("cal-a", "a@example.com", true),
        calendar("cal-b", "b@example.com", true),
    ]);
    // Clear the seeded list token so this walk is full (orphan path).
    pollster::block_on(calendars.set_calendar_list_sync_token("u-1", "", &now)).unwrap();
    let watches =
        FakeWatchChannelRepo::with(vec![watch_channel("cal-a", "2023-11-21T22:13:20Z")]);

    pollster::block_on(refresh_calendar_list(
        &http,
        &calendars,
        Some(&watches),
        &access(),
        "u-1",
        &now,
    ))
    .unwrap();

    let stored = calendars.stored.lock().unwrap();
    let a = stored.iter().find(|c| c.id == "cal-a").unwrap();
    assert!(!a.sync_enabled);
    assert!(a.deleted_at.is_some());
    let b = stored.iter().find(|c| c.id == "cal-b").unwrap();
    assert!(b.sync_enabled);
    assert!(b.deleted_at.is_none());
    drop(stored);
    assert_eq!(
        *watches.deleted_by_calendar_id.lock().unwrap(),
        vec!["cal-a".to_string()]
    );
    let token = pollster::block_on(calendars.get_calendar_list_sync_token("u-1"))
        .unwrap()
        .unwrap();
    assert_eq!(token, "tok-full");
    // Full list URL must not carry syncToken.
    let gets = http.gets.lock().unwrap();
    assert!(
        gets.iter().any(|u| u.contains("calendarList") && !u.contains("syncToken=")),
        "{gets:?}"
    );
}

#[test]
fn calendar_list_410_merge_full_does_not_wipe_on_mid_failure() {
    let now = unix_secs_to_rfc3339(NOW_UNIX);
    // First GET is incremental (has syncToken) → 410.
    // Second GET is full (no syncToken) → 500.
    let http = FakeHttp::new(vec![
        ("syncToken=", 410, r#"{"error":"gone"}"#),
        ("calendarList", 500, r#"{"error":"boom"}"#),
    ]);
    let calendars =
        FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    pollster::block_on(calendars.set_calendar_list_sync_token("u-1", "old", &now)).unwrap();
    let watches = FakeWatchChannelRepo::new();

    let err = pollster::block_on(refresh_calendar_list(
        &http,
        &calendars,
        Some(&watches),
        &access(),
        "u-1",
        &now,
    ))
    .unwrap_err();
    assert!(err.to_string().contains("500") || err.to_string().contains("calendarList"), "{err}");

    let stored = calendars.stored.lock().unwrap();
    assert!(stored[0].sync_enabled);
    assert!(stored[0].deleted_at.is_none());
    drop(stored);
    // Cursor cleared on 410 start — never a fabricated new success token.
    let token = pollster::block_on(calendars.get_calendar_list_sync_token("u-1"))
        .unwrap()
        .unwrap_or_default();
    assert_eq!(token, "", "must not persist a new success token after failed walk");
}

#[test]
fn calendar_list_metadata_upsert_does_not_re_enable_disabled() {
    let now = unix_secs_to_rfc3339(NOW_UNIX);
    let body = r#"{
        "items": [
            {"id": "primary@example.com", "summary": "Renamed", "accessRole": "writer", "timeZone": "Asia/Colombo"}
        ],
        "nextSyncToken": "tok-meta"
    }"#;
    let http = FakeHttp::new(vec![("calendarList", 200, body)]);
    let mut cal = calendar("cal-1", "primary@example.com", false);
    cal.summary = "Old".to_string();
    cal.access_role = "owner".to_string();
    let calendars = FakeCalendarRepo::with(vec![cal]);
    // Full walk also exercises the same upsert contract.
    pollster::block_on(calendars.set_calendar_list_sync_token("u-1", "", &now)).unwrap();
    let watches = FakeWatchChannelRepo::new();

    pollster::block_on(refresh_calendar_list(
        &http,
        &calendars,
        Some(&watches),
        &access(),
        "u-1",
        &now,
    ))
    .unwrap();

    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored.len(), 1);
    assert!(
        !stored[0].sync_enabled,
        "deliberate disable must survive metadata upsert"
    );
    assert_eq!(stored[0].summary, "Renamed");
    assert_eq!(stored[0].access_role, "writer");
    assert_eq!(stored[0].time_zone, "Asia/Colombo");
    assert!(stored[0].deleted_at.is_none());
}

#[test]
fn cron_publishes_newly_appeared_calendar_from_list_refresh() {
    let now = unix_secs_to_rfc3339(NOW_UNIX);
    let list_body = r#"{
        "items": [
            {"id": "new@example.com", "summary": "New", "accessRole": "owner"}
        ],
        "nextSyncToken": "tok-cron"
    }"#;
    // New calendar is never_initialized → replica_due; script events.list.
    let http = FakeHttp::new(vec![
        ("calendarList", 200, list_body),
        ("/events", 200, EVENTS_JSON),
    ]);
    let mut existing = calendar("cal-1", "primary@example.com", true);
    // Fresh so only the new calendar is replica-due (never synced).
    existing.last_success_at = Some("2023-11-14T22:12:20Z".to_string());
    existing.last_synced_at = existing.last_success_at.clone();
    let calendars = FakeCalendarRepo::with(vec![existing]);
    pollster::block_on(calendars.set_calendar_list_sync_token("u-1", "tok-1", &now)).unwrap();
    let events = FakeEventRepo::new();
    let watches = FakeWatchChannelRepo::new();
    let tokens = FakeTokenRepo::with(vec![fresh_token("u-1", "at-1")]);
    let oauth = oauth_config();

    let report = pollster::block_on(run_fallback_cron(
        &http, &calendars, &events, &FakeOperationRepo::new(), &watches, &tokens, &oauth, None, NOW_UNIX,
    ));

    // published includes the new calendar id from list refresh, plus the
    // replica publish for that new calendar (never-synced → due).
    assert!(
        report
            .published
            .iter()
            .any(|(u, _)| u == "u-1"),
        "{:?}",
        report.published
    );
    let new_rows: Vec<_> = calendars
        .stored
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.google_calendar_id == "new@example.com")
        .cloned()
        .collect();
    assert_eq!(new_rows.len(), 1);
    assert!(new_rows[0].sync_enabled);
    assert!(
        report
            .published
            .iter()
            .any(|(_, id)| id == &new_rows[0].id),
        "new calendar id must be in published: {:?}",
        report.published
    );
    assert!(report.errors.is_empty(), "{:?}", report.errors);
}
