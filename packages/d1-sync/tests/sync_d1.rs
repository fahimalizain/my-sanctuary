//! Integration tests: replica apply + health against real Worker migrations
//! (local SQLite = D1 schema). Fail if D1 SQL diverges from in-memory FakeRepo
//! suite behaviour for publish fencing, dirty webhook, and cron notify-after-commit.

use std::sync::Mutex;

use api_core::models::{GoogleCalendar, NewCalendarEvent, WatchChannel};
use api_core::oauth::{HttpClient, HttpError};
use api_core::repo::{CalendarEventRepo, CalendarRepo};
use api_core::DigestRepo;
use api_core::time::unix_secs_to_rfc3339;
use api_core::token::GoogleAccess;
use api_core::{
    calendar_sync_view, decide_webhook, persist_webhook_decision, replica_query_fingerprint,
    run_fallback_cron, sync_calendar, CalendarError, CalendarReplicaState, CheckpointResult,
    OAuthConfig, OperatorWarningLevel, SyncCalendarOutcome, WebhookPersistResult,
};
use d1_sync::{
    count_rows, open_harness, seed_user_token_calendar, EmptyTaskCategoryRepo, SeedOpts,
};

/// The walk boundaries take a category repo for the write-time leaf stamp.
/// These tests are not about classification: an empty taxonomy stamps NULL.
const NO_TAXONOMY: EmptyTaskCategoryRepo = EmptyTaskCategoryRepo;

const NOW_UNIX: i64 = 1_700_000_000; // 2023-11-14T22:13:20Z
const NOW_RFC: &str = "2023-11-14T22:13:20Z";

const EVENTS_JSON: &str = r#"{
    "items": [
        {"id": "evt-1", "etag": "e1", "updated": "2026-08-17T10:00:00.000Z",
         "summary": "Standup", "description": "Daily",
         "start": {"dateTime": "2026-08-18T09:00:00Z"},
         "end": {"dateTime": "2026-08-18T09:30:00Z"},
         "recurrence": ["RRULE:FREQ=DAILY"]},
        {"id": "evt-2", "summary": "Lunch",
         "start": {"dateTime": "2026-08-18T12:00:00Z"},
         "end": {"dateTime": "2026-08-18T13:00:00Z"}}
    ],
    "nextSyncToken": "st-9"
}"#;

/// Minimal scripted HTTP client (api-core FakeHttp is pub(crate)).
struct ScriptedHttp {
    routes: Vec<(String, u16, String)>,
    gets: Mutex<Vec<String>>,
}

impl ScriptedHttp {
    fn new(routes: Vec<(&str, u16, &str)>) -> Self {
        Self {
            routes: routes
                .into_iter()
                .map(|(s, st, b)| (s.to_string(), st, b.to_string()))
                .collect(),
            gets: Mutex::new(Vec::new()),
        }
    }

    fn route(&self, url: &str) -> (u16, Vec<u8>) {
        for (substr, status, body) in &self.routes {
            if url.contains(substr.as_str()) {
                return (*status, body.clone().into_bytes());
            }
        }
        // Default calendars.get label backfill.
        if url.contains("/calendar/v3/calendars/")
            && !url.contains("/events")
            && !url.contains("calendarList")
        {
            return (200, br#"{"labelProperties":{"eventLabels":[]}}"#.to_vec());
        }
        // Default incremental calendarList empty delta.
        if url.contains("calendarList") && url.contains("syncToken=") {
            return (200, br#"{"items":[]}"#.to_vec());
        }
        panic!("no route for {url}");
    }
}

#[async_trait::async_trait(?Send)]
impl HttpClient for ScriptedHttp {
    async fn post_form(&self, _url: &str, _form: &[(&str, &str)]) -> Result<Vec<u8>, HttpError> {
        Ok(Vec::new())
    }

    async fn get_bearer(&self, _url: &str, _token: &str) -> Result<Vec<u8>, HttpError> {
        Ok(Vec::new())
    }

    async fn get_bearer_raw(
        &self,
        url: &str,
        _token: &str,
    ) -> Result<(u16, Vec<u8>), HttpError> {
        self.gets.lock().unwrap().push(url.to_string());
        Ok(self.route(url))
    }

    async fn post_json(
        &self,
        _url: &str,
        _token: &str,
        _body: &[u8],
    ) -> Result<(u16, Vec<u8>), HttpError> {
        Ok((200, Vec::new()))
    }

    async fn patch_json(
        &self,
        _url: &str,
        _token: &str,
        _body: &[u8],
    ) -> Result<(u16, Vec<u8>), HttpError> {
        Ok((200, Vec::new()))
    }
}

fn access() -> GoogleAccess {
    GoogleAccess {
        access_token: "at-1".to_string(),
        token_type: "Bearer".to_string(),
    }
}

fn oauth_config() -> OAuthConfig {
    OAuthConfig {
        client_id: "client-id.apps.googleusercontent.com".to_string(),
        client_secret: "client-secret".to_string(),
        redirect_url: "http://localhost:5173/auth/google/callback".to_string(),
    }
}

fn webhook_channel(calendar_id: &str) -> WatchChannel {
    WatchChannel {
        id: "wc-1".to_string(),
        calendar_id: calendar_id.to_string(),
        channel_id: "minted-id".to_string(),
        resource_id: "resource-1".to_string(),
        token: "tok-1".to_string(),
        expiration: "2023-11-21T22:13:20Z".to_string(),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
    }
}

async fn load_cal(h: &d1_sync::Harness, id: &str) -> GoogleCalendar {
    h.calendars
        .get_by_id(id)
        .await
        .expect("get_by_id ok")
        .expect("calendar exists")
}

#[test]
fn d1_paginated_replica_publish_writes_token_only_after_last_page() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().unwrap();
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed");
    }

    let page_one = r#"{"items":[{"id":"p1","summary":"A","start":{"dateTime":"2026-08-18T09:00:00Z"},"end":{"dateTime":"2026-08-18T09:30:00Z"}}],"nextPageToken":"tok-2"}"#;
    let page_two = r#"{"items":[{"id":"p2","summary":"B","start":{"dateTime":"2026-08-18T10:00:00Z"},"end":{"dateTime":"2026-08-18T10:30:00Z"}}],"nextSyncToken":"st-final"}"#;

    let http = ScriptedHttp::new(vec![
        ("pageToken=tok-2", 200, page_two),
        ("/events", 200, page_one),
    ]);

    let cal = pollster::block_on(load_cal(&h, "cal-1"));
    let outcome = pollster::block_on(sync_calendar(
        &http,
        &h.calendars,
        &h.events,
        &h.operations,
        &access(),
        &cal,
        NOW_RFC,
        &NO_TAXONOMY,
    ))
    .expect("sync ok");
    assert_eq!(outcome, SyncCalendarOutcome::Published);

    let stored = pollster::block_on(load_cal(&h, "cal-1"));
    assert_eq!(stored.sync_token, "st-final");
    assert_eq!(
        stored.last_success_at.as_deref(),
        Some("2023-11-14T22:13:20Z")
    );
    assert_eq!(stored.sync_status, "ready");
    assert_eq!(stored.failure_streak, 0);
    assert!(stored.last_error_code.is_empty());
    assert!(stored.initial_sync_complete);
    assert_eq!(stored.sync_query_fingerprint, replica_query_fingerprint());

    let p1 = pollster::block_on(h.events.get_by_calendar_and_google_id("cal-1", "p1"))
        .expect("p1 query")
        .expect("p1 living");
    assert!(p1.deleted_at.is_none());
    let p2 = pollster::block_on(h.events.get_by_calendar_and_google_id("cal-1", "p2"))
        .expect("p2 query")
        .expect("p2 living");
    assert!(p2.deleted_at.is_none());

    let view = calendar_sync_view(&stored, NOW_UNIX);
    assert_eq!(view.operator_warning, OperatorWarningLevel::None);
    assert!(!view.stale);
}

#[test]
fn d1_missing_terminal_token_is_not_success() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().unwrap();
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed");
    }

    let body = r#"{"items":[
        {"id": "evt-1", "summary": "Standup",
         "start": {"dateTime": "2026-08-18T09:00:00Z"},
         "end": {"dateTime": "2026-08-18T09:30:00Z"}}
    ]}"#;
    let http = ScriptedHttp::new(vec![("/events", 200, body)]);
    let cal = pollster::block_on(load_cal(&h, "cal-1"));

    let err = pollster::block_on(sync_calendar(
        &http,
        &h.calendars,
        &h.events,
        &h.operations,
        &access(),
        &cal,
        NOW_RFC,
        &NO_TAXONOMY,
    ))
    .unwrap_err();
    assert!(
        matches!(err, CalendarError::InvalidResponse(ref m) if m.contains("missing nextSyncToken")),
        "{err:?}"
    );

    let stored = pollster::block_on(load_cal(&h, "cal-1"));
    assert_eq!(stored.sync_token, "old-tok");
    assert_eq!(
        stored.last_success_at.as_deref(),
        Some("2023-11-14T21:00:00Z")
    );
    assert_eq!(
        stored.last_attempt_at.as_deref(),
        Some("2023-11-14T22:13:20Z")
    );
    assert_eq!(stored.last_error_code, "missing_sync_token");
    assert_eq!(stored.sync_status, "retrying");
    assert_eq!(stored.failure_streak, 1);

    // Apply happened; publication did not.
    let evt = pollster::block_on(h.events.get_by_calendar_and_google_id("cal-1", "evt-1"))
        .expect("evt query")
        .expect("evt-1 living after apply");
    assert!(evt.deleted_at.is_none());

    let view = calendar_sync_view(&stored, NOW_UNIX);
    assert_eq!(view.error_code.as_deref(), Some("missing_sync_token"));
    assert_eq!(view.state, CalendarReplicaState::Retrying);
}

#[test]
fn d1_webhook_dirty_then_accepted() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().unwrap();
        seed_user_token_calendar(
            &conn,
            SeedOpts {
                dirty_requested_generation: 0,
                dirty_applied_generation: 0,
                ..SeedOpts::default()
            },
        )
        .expect("seed");
    }

    let cal = pollster::block_on(load_cal(&h, "cal-1"));
    let channel = webhook_channel("cal-1");
    let decision = decide_webhook("exists", Some(&channel), Some("tok-1"), Some(&cal));
    let now = unix_secs_to_rfc3339(NOW_UNIX);
    let result = pollster::block_on(persist_webhook_decision(&h.calendars, &decision, &now));
    assert_eq!(
        result,
        WebhookPersistResult::DirtyAccepted {
            calendar_id: "cal-1".to_string(),
        }
    );

    let stored = pollster::block_on(load_cal(&h, "cal-1"));
    assert_eq!(stored.dirty_requested_generation, 1);
    assert!(stored.sync_enabled);
    assert_eq!(stored.dirty_applied_generation, 0);
    assert_eq!(stored.sync_token, "old-tok");
}

#[test]
fn d1_cron_published_only_after_health_commit() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().unwrap();
        seed_user_token_calendar(
            &conn,
            SeedOpts {
                // Fresh last_success so dirty (not 15m backstop) triggers walk.
                last_success_at: Some("2023-11-14T22:12:20Z"),
                last_synced_at: Some("2023-11-14T22:12:20Z"),
                dirty_requested_generation: 3,
                dirty_applied_generation: 1,
                ..SeedOpts::default()
            },
        )
        .expect("seed");
    }

    let http = ScriptedHttp::new(vec![("/events", 200, EVENTS_JSON)]);
    let oauth = oauth_config();

    let report = pollster::block_on(run_fallback_cron(
        &http,
        &h.calendars,
        &h.events,
        &h.operations,
        &h.watches,
        &h.tokens,
        &oauth,
        None,
        NOW_UNIX,
        &NO_TAXONOMY,
    ));

    assert_eq!(report.synced, 1, "errors={:?}", report.errors);
    assert_eq!(
        report.published,
        vec![("u-1".to_string(), "cal-1".to_string())]
    );
    assert!(report.errors.is_empty(), "{:?}", report.errors);

    let stored = pollster::block_on(load_cal(&h, "cal-1"));
    assert_eq!(
        stored.dirty_applied_generation, 3,
        "generation-at-start must be applied"
    );
    assert_eq!(stored.sync_token, "st-9");
    assert_eq!(
        stored.last_success_at.as_deref(),
        Some("2023-11-14T22:13:20Z")
    );
    assert_eq!(stored.sync_status, "ready");
    assert_eq!(stored.failure_streak, 0);

    assert_eq!(report.diagnostics.len(), 1);
    let d = &report.diagnostics[0];
    assert_eq!(d.checkpoint, CheckpointResult::Published);
    assert_eq!(d.calendar_id, "cal-1");
    let json = serde_json::to_string(d).expect("serialize diagnostic");
    assert!(
        !json.contains("st-9"),
        "diagnostic must not leak sync token: {json}"
    );
    assert!(
        !json.contains("at-1"),
        "diagnostic must not leak access token: {json}"
    );

    // 1h operator-warning rule reads persisted health.
    let view_later = calendar_sync_view(&stored, NOW_UNIX + 7200);
    assert_eq!(view_later.operator_warning, OperatorWarningLevel::Stale);
    assert_eq!(
        stored.last_success_at.as_deref(),
        Some("2023-11-14T22:13:20Z")
    );
}

fn sample_new_event(google_event_id: &str, title: &str) -> NewCalendarEvent {
    NewCalendarEvent {
        calendar_id: "cal-1".to_string(),
        google_event_id: google_event_id.to_string(),
        google_etag: "etag-1".to_string(),
        google_updated_at: "2026-08-17T10:00:00Z".to_string(),
        last_synced_at: NOW_RFC.to_string(),
        title: title.to_string(),
        description: String::new(),
        start_time: "2026-08-18T09:00:00Z".to_string(),
        end_time: "2026-08-18T09:30:00Z".to_string(),
        recurrence: String::new(),
        task_id: String::new(),
        ical_uid: "uid-batch".to_string(),
        sequence: 0,
        status: "confirmed".to_string(),
        recurring_event_id: String::new(),
        original_start: String::new(),
        start_time_zone: "UTC".to_string(),
        end_time_zone: "UTC".to_string(),
        is_all_day: false,
        raw_json: r#"{"id":"g-batch"}"#.to_string(),
        category_id: Some("cat-batch".to_string()),
        preserve_category: false,
        is_self_organized: false,
        event_type: "focusTime".to_string(),
    }
}

/// Batch upsert mints candidate UUIDs without per-event SELECT; ON CONFLICT
/// keeps the original id, updates fields, and revives soft-deletes.
#[test]
fn d1_upsert_batch_preserves_id_and_revives_soft_delete() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().unwrap();
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed");
    }

    let original_id = pollster::block_on(h.events.upsert(
        sample_new_event("g-batch", "Original"),
        NOW_RFC,
    ))
    .expect("single upsert");

    // Re-batch same natural key with a different title; impl mints a fresh UUID
    // that ON CONFLICT must discard so the original id is kept.
    pollster::block_on(h.events.upsert_batch(
        vec![sample_new_event("g-batch", "Updated title")],
        NOW_RFC,
    ))
    .expect("upsert_batch");

    let after_batch =
        pollster::block_on(h.events.get_by_calendar_and_google_id("cal-1", "g-batch"))
            .expect("get")
            .expect("row living");
    assert_eq!(after_batch.id, original_id, "ON CONFLICT must preserve id");
    assert_eq!(after_batch.title, "Updated title");
    assert!(after_batch.deleted_at.is_none());
    // The classification columns survive the round trip (0019): the leaf
    // category overwrites from the incoming row, the Google-owned flags follow.
    assert_eq!(after_batch.category_id.as_deref(), Some("cat-batch"));
    assert!(!after_batch.is_self_organized);
    assert_eq!(after_batch.event_type, "focusTime");

    // Soft-delete, then batch-upsert again → revived with same id.
    pollster::block_on(h.events.delete(&original_id, NOW_RFC)).expect("soft-delete");
    {
        let conn = h.db.lock().unwrap();
        let deleted_at: Option<String> = conn
            .query_row(
                "SELECT deleted_at FROM calendar_events WHERE id = ?1",
                rusqlite::params![original_id],
                |r| r.get(0),
            )
            .expect("deleted row");
        assert!(deleted_at.is_some(), "row must be soft-deleted");
    }

    pollster::block_on(h.events.upsert_batch(
        vec![sample_new_event("g-batch", "Revived")],
        NOW_RFC,
    ))
    .expect("upsert_batch revive");

    let revived =
        pollster::block_on(h.events.get_by_calendar_and_google_id("cal-1", "g-batch"))
            .expect("get revived")
            .expect("row living again");
    assert_eq!(revived.id, original_id, "revive must keep original id");
    assert_eq!(revived.title, "Revived");
    assert!(revived.deleted_at.is_none());

    // Fenced batch path: live lease + upsert_batch_if_owner still preserves id.
    {
        let conn = h.db.lock().unwrap();
        conn.execute(
            "UPDATE google_calendars
             SET lease_owner = ?1, lease_expires_at = ?2
             WHERE id = 'cal-1'",
            rusqlite::params!["owner-batch", "2099-01-01T00:00:00Z"],
        )
        .expect("set lease");
    }
    let ok = pollster::block_on(h.events.upsert_batch_if_owner(
        vec![sample_new_event("g-batch", "Fenced title")],
        "owner-batch",
        NOW_RFC,
    ))
    .expect("if_owner");
    assert!(ok, "live lease must succeed");

    let fenced =
        pollster::block_on(h.events.get_by_calendar_and_google_id("cal-1", "g-batch"))
            .expect("get fenced")
            .expect("row living");
    assert_eq!(fenced.id, original_id, "if_owner must preserve id");
    assert_eq!(fenced.title, "Fenced title");
    assert!(fenced.deleted_at.is_none());
    assert_eq!(fenced.category_id.as_deref(), Some("cat-batch"));
    assert!(!fenced.is_self_organized);
    assert_eq!(fenced.event_type, "focusTime");
}

/// The 0019 columns are app/Google-owned: the fenced upsert clears a stale
/// leaf category when the incoming row is unclassified (`NULLIF(?, '')` →
/// SQL `NULL`), and the organizer/eventType flags overwrite.
#[test]
fn d1_upsert_overwrites_category_and_organizer_flags() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().unwrap();
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed");
    }

    let mut stamped = sample_new_event("g-flags", "Stamped");
    stamped.category_id = Some("cat-work".to_string());
    stamped.is_self_organized = true;
    stamped.event_type = "default".to_string();
    pollster::block_on(h.events.upsert(stamped, NOW_RFC)).expect("stamped upsert");

    let mut unclassified = sample_new_event("g-flags", "Retitled");
    unclassified.category_id = None;
    unclassified.is_self_organized = false;
    unclassified.event_type = "focusTime".to_string();
    pollster::block_on(h.events.upsert(unclassified, NOW_RFC)).expect("retitle upsert");

    let row = pollster::block_on(h.events.get_by_calendar_and_google_id("cal-1", "g-flags"))
        .expect("get")
        .expect("living");
    assert_eq!(row.title, "Retitled");
    assert_eq!(row.category_id, None, "a retitle clears the stale leaf stamp");
    assert!(!row.is_self_organized);
    assert_eq!(row.event_type, "focusTime");
}

/// An empty category id binds SQL NULL rather than an empty string.
#[test]
fn d1_upsert_stores_empty_category_as_null() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().unwrap();
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed");
    }

    let mut event = sample_new_event("g-blank", "Blank");
    event.category_id = Some(String::new());
    pollster::block_on(h.events.upsert(event, NOW_RFC)).expect("upsert");

    let conn = h.db.lock().unwrap();
    let stored: Option<String> = conn
        .query_row(
            "SELECT category_id FROM calendar_events WHERE google_event_id = 'g-blank'",
            [],
            |row| row.get(0),
        )
        .expect("category_id column");
    assert_eq!(stored, None, "\"\" must be stored as SQL NULL");
}

/// `preserve_category` keeps the stored stamp on conflict — the merge arm a
/// failed taxonomy load takes — while a real (stamping) context still writes
/// NULL. The flag binds once per statement, so this is also the `CASE WHEN ?`
/// round trip through the real SQL.
#[test]
fn d1_upsert_preserve_category_keeps_the_stored_stamp() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().unwrap();
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed");
    }

    let mut stamped = sample_new_event("g-preserve", "Work");
    stamped.category_id = Some("cat-work".to_string());
    stamped.preserve_category = false;
    pollster::block_on(h.events.upsert(stamped, NOW_RFC)).expect("stamped upsert");

    // A walk whose taxonomy load failed: no leaf, preserve the stored one.
    let mut preserving = sample_new_event("g-preserve", "Work");
    preserving.category_id = None;
    preserving.preserve_category = true;
    pollster::block_on(h.events.upsert(preserving, NOW_RFC)).expect("preserving upsert");

    let row = pollster::block_on(h.events.get_by_calendar_and_google_id("cal-1", "g-preserve"))
        .expect("get")
        .expect("living");
    assert_eq!(
        row.category_id.as_deref(),
        Some("cat-work"),
        "a preserving upsert must not erase the stamp"
    );

    // A successful (stamping) context with no match really is unclassified.
    let mut unclassified = sample_new_event("g-preserve", "Work");
    unclassified.category_id = None;
    unclassified.preserve_category = false;
    pollster::block_on(h.events.upsert(unclassified, NOW_RFC)).expect("stamp-null upsert");

    let row = pollster::block_on(h.events.get_by_calendar_and_google_id("cal-1", "g-preserve"))
        .expect("get")
        .expect("living");
    assert_eq!(
        row.category_id, None,
        "a successful load that matches nothing writes NULL"
    );
}

/// The write-time upsert records `category_classified` (0022): a real
/// (stamping) context stores `1`, a `preserve_category` chunk stores `0` for a
/// **new** row — nothing classified it, so the digest's verification scan may
/// still pick it up — and keeps whatever the stored row has on conflict.
#[test]
fn d1_upsert_records_and_preserves_the_classified_flag() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().unwrap();
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed");
    }
    let flag = |google_event_id: &str| -> i64 {
        let conn = h.db.lock().unwrap();
        conn.query_row(
            "SELECT category_classified FROM calendar_events WHERE google_event_id = ?1",
            rusqlite::params![google_event_id],
            |row| row.get(0),
        )
        .expect("category_classified column")
    };

    // A normal write ran the classifier: flag 1 even when nothing matched.
    let mut stamped = sample_new_event("g-flag", "Stamped");
    stamped.category_id = None;
    pollster::block_on(h.events.upsert(stamped, NOW_RFC)).expect("stamping upsert");
    assert_eq!(flag("g-flag"), 1);

    // A preserving chunk does not clear a stored 1…
    let mut preserving = sample_new_event("g-flag", "Stamped");
    preserving.category_id = None;
    preserving.preserve_category = true;
    pollster::block_on(h.events.upsert(preserving, NOW_RFC)).expect("preserving upsert");
    assert_eq!(flag("g-flag"), 1, "a preserving walk keeps a stored 1");

    // …and does not claim a stored 0 either.
    {
        let conn = h.db.lock().unwrap();
        conn.execute(
            "UPDATE calendar_events SET category_classified = 0 WHERE google_event_id = 'g-flag'",
            [],
        )
        .expect("mark never classified");
    }
    let mut preserving = sample_new_event("g-flag", "Stamped");
    preserving.category_id = None;
    preserving.preserve_category = true;
    pollster::block_on(h.events.upsert(preserving, NOW_RFC)).expect("preserving upsert");
    assert_eq!(flag("g-flag"), 0, "a preserving walk keeps a stored 0");

    // A brand-new row written by a preserving walk is 0: classification did
    // not run, so the digest must still treat it as pending.
    let mut fresh = sample_new_event("g-fresh", "Fresh");
    fresh.category_id = None;
    fresh.preserve_category = true;
    pollster::block_on(h.events.upsert(fresh, NOW_RFC)).expect("fresh upsert");
    assert_eq!(flag("g-fresh"), 0);

    // The fenced (replica-walk) builder carries the same two flags.
    {
        let conn = h.db.lock().unwrap();
        conn.execute(
            "UPDATE google_calendars
             SET lease_owner = ?1, lease_expires_at = ?2
             WHERE id = 'cal-1'",
            rusqlite::params!["owner-flag", "2099-01-01T00:00:00Z"],
        )
        .expect("set lease");
    }
    let mut fenced = sample_new_event("g-fenced", "Fenced");
    fenced.category_id = None;
    fenced.preserve_category = true;
    let ok = pollster::block_on(h.events.upsert_batch_if_owner(
        vec![fenced],
        "owner-flag",
        NOW_RFC,
    ))
    .expect("fenced upsert");
    assert!(ok, "live lease must succeed");
    assert_eq!(flag("g-fenced"), 0, "the fenced builder binds the flag too");

    // Stamping through the fenced path also writes 1 and overwrites.
    let mut fenced = sample_new_event("g-fenced", "Fenced");
    fenced.category_id = Some("cat-work".to_string());
    let ok = pollster::block_on(h.events.upsert_batch_if_owner(
        vec![fenced],
        "owner-flag",
        NOW_RFC,
    ))
    .expect("fenced upsert");
    assert!(ok);
    assert_eq!(flag("g-fenced"), 1);
}

fn seed_event(
    conn: &rusqlite::Connection,
    id: &str,
    google_event_id: &str,
    task_id: Option<&str>,
    status: &str,
    recurring_event_id: &str,
) -> Result<(), String> {
    // The sweep tests' default span: 2026-08-18 09:00–09:30 UTC, zone unset.
    seed_timed_event(
        conn,
        id,
        google_event_id,
        "2026-08-18T09:00:00Z",
        "2026-08-18T09:30:00Z",
        "",
        task_id,
        status,
        recurring_event_id,
    )
}

/// Seed one living event with an explicit span, so the dirty-marking
/// assertions can name the dates the sweep touches.
#[allow(clippy::too_many_arguments)]
fn seed_timed_event(
    conn: &rusqlite::Connection,
    id: &str,
    google_event_id: &str,
    start_time: &str,
    end_time: &str,
    time_zone: &str,
    task_id: Option<&str>,
    status: &str,
    recurring_event_id: &str,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO calendar_events (
            id, calendar_id, google_event_id, google_etag, google_updated_at,
            last_synced_at, title, description, start_time, end_time, recurrence,
            task_id, ical_uid, sequence, status, recurring_event_id, original_start,
            start_time_zone, end_time_zone, is_all_day, raw_json,
            created_at, updated_at, deleted_at
         ) VALUES (
            ?1, 'cal-1', ?2, '', '',
            ?3, ?2, '', ?4, ?5, '',
            ?6, '', 0, ?7, ?8, '',
            ?9, ?9, 0, '',
            ?3, ?3, NULL
         )",
        rusqlite::params![
            id,
            google_event_id,
            NOW_RFC,
            start_time,
            end_time,
            task_id,
            status,
            recurring_event_id,
            time_zone,
        ],
    )
    .map_err(|e| format!("seed event {id}: {e}"))?;
    Ok(())
}

#[test]
fn d1_sweep_absent_if_owner_membership_and_lease_fence() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().unwrap();
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed");
        // Hold lease for owner-a.
        conn.execute(
            "UPDATE google_calendars
             SET lease_owner = ?1, lease_expires_at = ?2
             WHERE id = 'cal-1'",
            rusqlite::params!["owner-a", "2099-01-01T00:00:00Z"],
        )
        .expect("set lease");
        seed_event(&conn, "e-keep", "keep", None, "confirmed", "").expect("keep");
        seed_event(&conn, "e-ghost", "ghost", None, "confirmed", "").expect("ghost");
        seed_event(
            &conn,
            "e-exc",
            "exc-absent",
            None,
            "cancelled",
            "master-1",
        )
        .expect("exc");
        seed_event(
            &conn,
            "e-task",
            "task-linked",
            Some("task-99"),
            "confirmed",
            "",
        )
        .expect("task");
        // Seen snapshot contains keep only.
        conn.execute(
            "INSERT INTO calendar_replica_seen (calendar_id, run_id, google_event_id, created_at)
             VALUES ('cal-1', 'run-1', 'keep', ?1)",
            rusqlite::params![NOW_RFC],
        )
        .expect("seed seen");
    }

    let ok = pollster::block_on(h.events.sweep_absent_if_owner(
        "cal-1",
        "run-1",
        "owner-a",
        NOW_RFC,
    ))
    .expect("sweep ok");
    assert!(ok, "owner must hold lease");

    let keep = pollster::block_on(h.events.get_by_calendar_and_google_id("cal-1", "keep"))
        .expect("keep q")
        .expect("keep living");
    assert!(keep.deleted_at.is_none());

    let ghost = pollster::block_on(h.events.get_by_calendar_and_google_id("cal-1", "ghost"))
        .expect("ghost q");
    assert!(ghost.is_none(), "ghost must be soft-deleted (get filters deleted)");
    // Confirm tombstone via raw SQL.
    {
        let conn = h.db.lock().unwrap();
        let deleted_at: Option<String> = conn
            .query_row(
                "SELECT deleted_at FROM calendar_events WHERE google_event_id = 'ghost'",
                [],
                |r| r.get(0),
            )
            .expect("ghost row");
        assert!(deleted_at.is_some(), "ghost deleted_at stamped");
    }

    let exc = pollster::block_on(h.events.get_by_calendar_and_google_id("cal-1", "exc-absent"))
        .expect("exc q")
        .expect("cancelled exception living");
    assert!(exc.deleted_at.is_none());
    assert_eq!(exc.status, "cancelled");
    assert_eq!(exc.recurring_event_id, "master-1");

    let task = pollster::block_on(h.events.get_by_calendar_and_google_id("cal-1", "task-linked"))
        .expect("task q")
        .expect("task-linked living");
    assert!(task.deleted_at.is_none());
    assert_eq!(task.task_id, "task-99");

    // Foreign / expired lease → false and no further mutation.
    // Restore ghost as living to observe no-op under foreign lease.
    {
        let conn = h.db.lock().unwrap();
        conn.execute(
            "UPDATE calendar_events SET deleted_at = NULL WHERE google_event_id = 'ghost'",
            [],
        )
        .expect("restore ghost");
        conn.execute(
            "UPDATE google_calendars
             SET lease_owner = ?1, lease_expires_at = ?2
             WHERE id = 'cal-1'",
            rusqlite::params!["other-owner", "2099-01-01T00:00:00Z"],
        )
        .expect("steal lease");
    }
    let denied = pollster::block_on(h.events.sweep_absent_if_owner(
        "cal-1",
        "run-1",
        "owner-a",
        NOW_RFC,
    ))
    .expect("sweep denied");
    assert!(!denied, "foreign lease must return false");
    let ghost_again =
        pollster::block_on(h.events.get_by_calendar_and_google_id("cal-1", "ghost"))
            .expect("ghost q2")
            .expect("ghost still living under foreign lease");
    assert!(ghost_again.deleted_at.is_none());

    // Expired lease also denies.
    {
        let conn = h.db.lock().unwrap();
        conn.execute(
            "UPDATE google_calendars
             SET lease_owner = ?1, lease_expires_at = ?2
             WHERE id = 'cal-1'",
            rusqlite::params!["owner-a", "2020-01-01T00:00:00Z"],
        )
        .expect("expire lease");
    }
    let expired = pollster::block_on(h.events.sweep_absent_if_owner(
        "cal-1",
        "run-1",
        "owner-a",
        NOW_RFC,
    ))
    .expect("sweep expired");
    assert!(!expired, "expired lease must return false");
    let ghost_exp =
        pollster::block_on(h.events.get_by_calendar_and_google_id("cal-1", "ghost"))
            .expect("ghost q3")
            .expect("ghost still living under expired lease");
    assert!(ghost_exp.deleted_at.is_none());
}

/// A full resync removes events the walk no longer sees; their days must be
/// queued for a digest rebuild (the sweep's bugs used to leave stale
/// `daily_meta` / category / sleep rows behind).
#[test]
fn d1_sweep_absent_if_owner_dirties_the_days_it_deletes() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().unwrap();
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed");
        conn.execute(
            "UPDATE google_calendars
             SET lease_owner = ?1, lease_expires_at = ?2
             WHERE id = 'cal-1'",
            rusqlite::params!["owner-a", "2099-01-01T00:00:00Z"],
        )
        .expect("set lease");
        // The ghost: living, absent from the seen snapshot, on its own date.
        seed_timed_event(
            &conn,
            "e-ghost",
            "ghost",
            "2026-09-10T22:00:00Z",
            "2026-09-10T23:00:00Z",
            "UTC",
            None,
            "confirmed",
            "",
        )
        .expect("ghost");
        // Rows the sweep must skip: a cancelled exception and a task-linked
        // event, deliberately on another date so the dirty assertions can tell
        // them apart from the ghost's day.
        seed_timed_event(
            &conn,
            "e-exc",
            "exc-absent",
            "2026-10-02T09:00:00Z",
            "2026-10-02T09:30:00Z",
            "UTC",
            None,
            "cancelled",
            "master-1",
        )
        .expect("exc");
        seed_timed_event(
            &conn,
            "e-task",
            "task-linked",
            "2026-10-02T09:00:00Z",
            "2026-10-02T09:30:00Z",
            "UTC",
            Some("task-99"),
            "confirmed",
            "",
        )
        .expect("task");
        // Seen snapshot: neither ghost nor the excluded rows.
        conn.execute(
            "INSERT INTO calendar_replica_seen (calendar_id, run_id, google_event_id, created_at)
             VALUES ('cal-1', 'run-1', 'seen-elsewhere', ?1)",
            rusqlite::params![NOW_RFC],
        )
        .expect("seed seen");
    }

    let ok = pollster::block_on(h.events.sweep_absent_if_owner(
        "cal-1",
        "run-1",
        "owner-a",
        NOW_RFC,
    ))
    .expect("sweep ok");
    assert!(ok, "owner must hold lease");

    let ghost = pollster::block_on(h.events.get_by_calendar_and_google_id("cal-1", "ghost"))
        .expect("ghost q");
    assert!(ghost.is_none(), "ghost must be soft-deleted");

    // The excluded rows stay living.
    let exc = pollster::block_on(h.events.get_by_calendar_and_google_id("cal-1", "exc-absent"))
        .expect("exc q")
        .expect("cancelled exception living");
    assert_eq!(exc.status, "cancelled");
    let task = pollster::block_on(h.events.get_by_calendar_and_google_id("cal-1", "task-linked"))
        .expect("task q")
        .expect("task-linked living");
    assert_eq!(task.task_id, "task-99");

    // The swept event's local date and its ±1 sleep neighbours are dirty; the
    // skipped rows' date (2026-10-02) is not — a drifted SELECT predicate would
    // show up here.
    assert_eq!(
        pollster::block_on(h.digest().list_dirty("u-1", 10)).expect("list dirty"),
        vec![
            "2026-09-09".to_string(),
            "2026-09-10".to_string(),
            "2026-09-11".to_string(),
        ],
        "the sweep must dirty exactly the days it deletes"
    );
}

// ── Digest tables (slice 2) ─────────────────────────────────────────────────

/// A batch upsert dirties the union of every row's stored span and incoming
/// span in **one** pass: the chunk resolves the owner once and reads all stored
/// spans in one `google_event_id IN (…)` SELECT (F6).
#[test]
fn d1_batch_upsert_dirties_the_union_of_three_spans() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().unwrap();
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed");
    }
    let digest = h.digest();

    // One event already exists with a **different** stored span, so the batch
    // has to union the old dates in as well.
    let mut existing = sample_new_event("g-a", "A");
    existing.start_time = "2026-09-01T10:00:00Z".to_string();
    existing.end_time = "2026-09-01T11:00:00Z".to_string();
    pollster::block_on(h.events.upsert(existing, NOW_RFC)).expect("seed a");
    pollster::block_on(digest.clear_dirty(
        "u-1",
        &[
            "2026-08-31".to_string(),
            "2026-09-01".to_string(),
            "2026-09-02".to_string(),
        ],
    ))
    .expect("clear");

    let mut moved = sample_new_event("g-a", "A moved");
    moved.start_time = "2026-09-20T10:00:00Z".to_string();
    moved.end_time = "2026-09-20T11:00:00Z".to_string();
    let mut b = sample_new_event("g-b", "B");
    b.start_time = "2026-09-05T10:00:00Z".to_string();
    b.end_time = "2026-09-05T11:00:00Z".to_string();
    let mut c = sample_new_event("g-c", "C");
    c.start_time = "2026-09-10T10:00:00Z".to_string();
    c.end_time = "2026-09-10T11:00:00Z".to_string();

    pollster::block_on(h.events.upsert_batch(vec![moved, b, c], NOW_RFC)).expect("batch");

    assert_eq!(
        pollster::block_on(digest.list_dirty("u-1", 20)).expect("dirty"),
        vec![
            // A's stored span (read for the whole chunk in one SELECT).
            "2026-08-31".to_string(),
            "2026-09-01".to_string(),
            "2026-09-02".to_string(),
            // B.
            "2026-09-04".to_string(),
            "2026-09-05".to_string(),
            "2026-09-06".to_string(),
            // C.
            "2026-09-09".to_string(),
            "2026-09-10".to_string(),
            "2026-09-11".to_string(),
            // A's incoming span.
            "2026-09-19".to_string(),
            "2026-09-20".to_string(),
            "2026-09-21".to_string(),
        ],
        "the 3-row batch dirties the union of the stored and incoming spans"
    );
}

/// Soft-deleting a calendar dirties every local date its **living** events used
/// to fill: the digest stops seeing those events the moment `deleted_at` is set
/// (F9).
#[test]
fn d1_calendar_soft_delete_dirties_its_living_events_days() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().unwrap();
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed");
        seed_timed_event(
            &conn,
            "e-1",
            "g-1",
            "2026-09-10T22:00:00Z",
            "2026-09-10T23:00:00Z",
            "UTC",
            None,
            "confirmed",
            "",
        )
        .expect("event");
    }
    let digest = h.digest();

    pollster::block_on(h.calendars.delete("cal-1", NOW_RFC)).expect("delete");

    let stored = pollster::block_on(h.calendars.get_by_id_unfiltered("cal-1"))
        .expect("read")
        .expect("row");
    assert!(stored.deleted_at.is_some(), "the calendar is soft-deleted");
    assert_eq!(
        pollster::block_on(digest.list_dirty("u-1", 10)).expect("dirty"),
        vec![
            "2026-09-09".to_string(),
            "2026-09-10".to_string(),
            "2026-09-11".to_string()
        ],
        "the deleted calendar's living event days are dirty"
    );

    // A second delete is a no-op: the calendar is already gone, so its owner is
    // no longer resolvable and nothing is re-marked.
    pollster::block_on(digest.clear_dirty(
        "u-1",
        &[
            "2026-09-09".to_string(),
            "2026-09-10".to_string(),
            "2026-09-11".to_string(),
        ],
    ))
    .expect("clear");
    pollster::block_on(h.calendars.delete("cal-1", NOW_RFC)).expect("delete again");
    assert!(
        pollster::block_on(digest.list_dirty("u-1", 10))
            .expect("dirty")
            .is_empty(),
        "an already-deleted calendar marks nothing"
    );
}

/// The work order is fair: the user whose dirty day has waited longest goes
/// first, so one user's backlog cannot starve the others (F14).
#[test]
fn d1_users_with_work_orders_by_the_oldest_dirty_stamp() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().unwrap();
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed");
        conn.execute(
            "INSERT INTO dirty_local_days (user_id, local_date, enqueued_at) VALUES
             ('u-1', '2026-09-01', '2026-09-01T00:00:00Z'),
             ('u-2', '2026-09-02', '2026-09-02T00:00:00Z')",
            [],
        )
        .expect("dirty rows");
        // u-3 has no dirty day, only a running job: it is picked, but after
        // every user with real dirty work.
        conn.execute(
            "INSERT INTO report_job_status (user_id, kind, status, cursor, updated_at)
             VALUES ('u-3', 'backfill', 'running', '', '2026-09-01T00:00:00Z')",
            [],
        )
        .expect("job row");
    }
    let digest = h.digest();

    assert_eq!(
        pollster::block_on(digest.users_with_work(3)).expect("work"),
        vec!["u-1".to_string(), "u-2".to_string(), "u-3".to_string()],
        "oldest dirty stamp first, job-only users last"
    );

    // Re-stamping u-1's day moves it behind u-2.
    {
        let conn = h.db.lock().unwrap();
        conn.execute(
            "UPDATE dirty_local_days SET enqueued_at = '2026-09-03T00:00:00Z'
             WHERE user_id = 'u-1'",
            [],
        )
        .expect("restamp");
    }
    assert_eq!(
        pollster::block_on(digest.users_with_work(3)).expect("work"),
        vec!["u-2".to_string(), "u-1".to_string(), "u-3".to_string()]
    );
}

/// The estimation default is the user's own `sleep_duration` rule target, read
/// from the rules tables through the digest repo (F15).
#[test]
fn d1_sleep_target_reads_the_latest_sleep_duration_revision() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().unwrap();
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed");
        conn.execute(
            "INSERT INTO rules (
               id, user_id, name, icon, kind, status, starts_on, ends_on, paused_on,
               sort_order, created_at, updated_at
             ) VALUES ('r-sleep', 'u-1', 'Sleep duration', 'sleep', 'sleep_duration',
                       'active', '2026-01-01', NULL, NULL, 0, ?1, ?1)",
            rusqlite::params![NOW_RFC],
        )
        .expect("rule");
        conn.execute(
            "INSERT INTO rule_revisions (id, rule_id, effective_from, params_json, created_at)
             VALUES ('rev-1', 'r-sleep', '2026-01-01',
                     '{\"window_days\":60,\"min_nights\":14,\"target_hours\":6.5}', ?1)",
            rusqlite::params![NOW_RFC],
        )
        .expect("revision");
        conn.execute(
            "INSERT INTO rule_revisions (id, rule_id, effective_from, params_json, created_at)
             VALUES ('rev-2', 'r-sleep', '2026-06-01',
                     '{\"window_days\":60,\"min_nights\":14,\"target_hours\":8}', ?1)",
            rusqlite::params![NOW_RFC],
        )
        .expect("revision");
        // An archived rule never wins, even with the newest revision.
        conn.execute(
            "INSERT INTO rules (
               id, user_id, name, icon, kind, status, starts_on, ends_on, paused_on,
               sort_order, created_at, updated_at
             ) VALUES ('r-old', 'u-1', 'Old sleep', 'sleep', 'sleep_duration',
                       'archived', '2025-01-01', NULL, NULL, 1, ?1, ?1)",
            rusqlite::params![NOW_RFC],
        )
        .expect("archived rule");
        conn.execute(
            "INSERT INTO rule_revisions (id, rule_id, effective_from, params_json, created_at)
             VALUES ('rev-3', 'r-old', '2027-01-01', '{\"target_hours\":9}', ?1)",
            rusqlite::params![NOW_RFC],
        )
        .expect("archived revision");
    }
    let digest = h.digest();

    assert_eq!(
        pollster::block_on(digest.sleep_target_secs("u-1")).expect("target"),
        Some(8 * 3600),
        "the latest non-archived revision wins"
    );
    assert_eq!(
        pollster::block_on(digest.sleep_target_secs("u-nobody")).expect("target"),
        None,
        "no rule ⇒ no target (the caller keeps DEFAULT_SLEEP_SECS)"
    );
}

/// The verification scan finds a living, never-classified event whatever its
/// id — the id walk (`e.id > ?`) can skip one that sorts before its cursor
/// (F11/F14). A NULL leaf alone is not pending: it may be a deliberate
/// no-match, so only `category_classified = 0` counts.
#[test]
fn d1_unstamped_scan_finds_rows_the_id_cursor_would_skip() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().unwrap();
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed");
        seed_event(&conn, "e-a", "g-a", None, "confirmed", "").expect("a");
        seed_event(&conn, "e-b", "g-b", None, "confirmed", "").expect("b");
        conn.execute(
            "UPDATE calendar_events SET category_id = 'cat-work' WHERE id = 'e-b'",
            [],
        )
        .expect("stamp b");
        // A soft-deleted row never counts as pending work.
        seed_event(&conn, "e-c", "g-c", None, "confirmed", "").expect("c");
        conn.execute(
            "UPDATE calendar_events SET deleted_at = ?1 WHERE id = 'e-c'",
            rusqlite::params![NOW_RFC],
        )
        .expect("soft-delete c");
        // `e-a` and the soft-deleted `e-c` were never classified (the column
        // default is 1: a row written before 0022 is treated as classified).
        conn.execute(
            "UPDATE calendar_events SET category_classified = 0 WHERE id IN ('e-a', 'e-c')",
            [],
        )
        .expect("mark never classified");
    }
    let digest = h.digest();

    let batch = pollster::block_on(digest.unstamped_batch("u-1", 10)).expect("batch");
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].id, "e-a");
    assert_eq!(batch[0].category_id, None);
    assert_eq!(batch[0].category_classified, 0);
    assert!(
        pollster::block_on(digest.has_unstamped_event("u-1")).expect("has"),
        "the living never-classified row is pending"
    );
    // The walk's cursor is past it: `e.id > 'e-a'` never returns it again.
    assert!(
        pollster::block_on(digest.reclassify_batch("u-1", "e-a", 10))
            .expect("walk")
            .iter()
            .all(|row| row.id != "e-a")
    );

    pollster::block_on(digest.set_category("e-a", Some("cat-work"))).expect("stamp");
    assert!(
        !pollster::block_on(digest.has_unstamped_event("u-1")).expect("has"),
        "nothing never-classified is left"
    );
    assert!(pollster::block_on(digest.unstamped_batch("u-1", 10))
        .expect("batch")
        .is_empty());
}

/// A deliberate no-match still marks the row classified: `category_id` stays
/// SQL NULL, but `category_classified = 1` keeps the verification scan from
/// rewinding over it forever.
#[test]
fn d1_digest_no_match_stamp_sets_the_classified_flag() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().unwrap();
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed");
        seed_event(&conn, "e-a", "g-a", None, "confirmed", "").expect("a");
        conn.execute(
            "UPDATE calendar_events SET category_classified = 0 WHERE id = 'e-a'",
            [],
        )
        .expect("mark never classified");
    }
    let digest = h.digest();

    pollster::block_on(digest.set_category("e-a", None)).expect("no-match stamp");
    {
        let conn = h.db.lock().unwrap();
        let (category, classified): (Option<String>, i64) = conn
            .query_row(
                "SELECT category_id, category_classified FROM calendar_events WHERE id = 'e-a'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row");
        assert_eq!(category, None, "no match leaves the leaf NULL");
        assert_eq!(classified, 1, "…but the row counts as classified");
    }
    assert!(
        !pollster::block_on(digest.has_unstamped_event("u-1")).expect("has"),
        "the no-match row is not pending work"
    );
    assert!(pollster::block_on(digest.unstamped_batch("u-1", 10))
        .expect("batch")
        .is_empty());
}

/// Every digest statement must survive the real schema: apply the Worker
/// migrations, write one day + its dirty mark out of a calendar-event upsert,
/// then read the rows back.
#[test]
fn d1_digest_replace_day_and_dirty_mark_round_trip() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().unwrap();
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed");
    }
    let digest = h.digest();

    // An event upsert dirties its local dates and their ±1 sleep neighbours,
    // without the caller ever touching `dirty_local_days`.
    let mut event = sample_new_event("g-digest", "Deep Work");
    event.start_time = "2026-08-18T09:00:00Z".to_string();
    event.end_time = "2026-08-18T10:00:00Z".to_string();
    event.start_time_zone = "UTC".to_string();
    event.end_time_zone = "UTC".to_string();
    pollster::block_on(h.events.upsert(event, NOW_RFC)).expect("upsert");

    let dirty = pollster::block_on(digest.list_dirty("u-1", 10)).expect("list dirty");
    assert_eq!(
        dirty,
        vec![
            "2026-08-17".to_string(),
            "2026-08-18".to_string(),
            "2026-08-19".to_string()
        ],
        "the upsert dirties the span and its neighbours"
    );
    assert_eq!(
        pollster::block_on(digest.count_dirty("u-1")).expect("count"),
        3
    );

    // One day: a category row, two hour rows, one night, and the meta row.
    let day = api_core::DayDigest {
        local_date: "2026-08-18".to_string(),
        time_zone: "Asia/Kolkata".to_string(),
        tracked_seconds: 3600,
        sleep_seconds: 1800,
        computed_at: NOW_RFC.to_string(),
        category_seconds: vec![
            api_core::CategorySeconds {
                category_id: "cat-a".to_string(),
                seconds: 2400,
            },
            api_core::CategorySeconds {
                category_id: "*sleep".to_string(),
                seconds: 1200,
            },
        ],
        hour_seconds: vec![
            api_core::HourSeconds {
                hour: 9,
                category_id: "cat-a".to_string(),
                seconds: 1200,
            },
            api_core::HourSeconds {
                hour: 10,
                category_id: "cat-a".to_string(),
                seconds: 1200,
            },
        ],
        nights: vec![api_core::NightRow {
            wake_date: "2026-08-18".to_string(),
            asleep_at: "2026-08-17T17:00:00Z".to_string(),
            woke_at: "2026-08-17T23:30:00Z".to_string(),
            seconds: 23_400,
            bed_min_past_noon: 630,
            wake_min_past_midnight: 300,
            source: api_core::SleepSource::Paired,
        }],
    };
    pollster::block_on(digest.replace_day("u-1", &day)).expect("replace day");

    {
        let conn = h.db.lock().unwrap();
        let meta: (String, i64, i64, i64) = conn
            .query_row(
                "SELECT time_zone, tracked_seconds, sleep_seconds, algo_version
                 FROM daily_meta WHERE user_id = 'u-1' AND local_date = '2026-08-18'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("meta row");
        assert_eq!(meta.0, "Asia/Kolkata");
        assert_eq!(meta.1, 3600);
        assert_eq!(meta.2, 1800);
        assert_eq!(meta.3, api_core::ALGO_VERSION);

        assert_eq!(count_rows(&conn, "daily_category_minutes").expect("count"), 2);
        assert_eq!(count_rows(&conn, "daily_hour_seconds").expect("count"), 2);
        assert_eq!(count_rows(&conn, "sleep_nights").expect("count"), 1);
        let night: (String, i64, i64, i64, String) = conn
            .query_row(
                "SELECT wake_date, seconds, bed_min_past_noon, wake_min_past_midnight, source
                 FROM sleep_nights",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("night row");
        assert_eq!(night.0, "2026-08-18");
        assert_eq!(night.1, 23_400);
        assert_eq!(night.2, 630);
        assert_eq!(night.3, 300);
        assert_eq!(night.4, "paired");

        // Hour rows survive SQLite's INTEGER affinity for text binds.
        let hour: i64 = conn
            .query_row(
                "SELECT seconds FROM daily_hour_seconds WHERE hour = 9",
                [],
                |row| row.get(0),
            )
            .expect("hour row");
        assert_eq!(hour, 1200);
    }

    // `replace_day` is idempotent: the same payload twice leaves the same rows.
    pollster::block_on(digest.replace_day("u-1", &day)).expect("replace day again");
    {
        let conn = h.db.lock().unwrap();
        assert_eq!(count_rows(&conn, "daily_category_minutes").expect("count"), 2);
        assert_eq!(count_rows(&conn, "daily_hour_seconds").expect("count"), 2);
        assert_eq!(count_rows(&conn, "sleep_nights").expect("count"), 1);
        assert_eq!(count_rows(&conn, "daily_meta").expect("count"), 1);
    }

    // Clearing the day's dirty rows is what the tick does after a rebuild.
    pollster::block_on(digest.clear_dirty("u-1", &["2026-08-18".to_string()])).expect("clear");
    assert_eq!(
        pollster::block_on(digest.count_dirty("u-1")).expect("count"),
        2
    );

    // Job rows: the reclassify cursor round-trips, and `users_with_work` sees
    // a running job even with no dirty days left.
    pollster::block_on(digest.upsert_job_status(
        "u-1",
        "reclassify",
        "running",
        "e-7",
        NOW_RFC,
    ))
    .expect("job upsert");
    let job = pollster::block_on(digest.job_status("u-1", "reclassify"))
        .expect("job read")
        .expect("job row");
    assert_eq!(job.status, "running");
    assert_eq!(job.cursor, "e-7");
    assert!(job.is_running());
    assert_eq!(
        pollster::block_on(digest.users_with_work(5)).expect("work"),
        vec!["u-1".to_string()]
    );
    assert_eq!(
        pollster::block_on(digest.primary_time_zone("u-1")).expect("tz"),
        Some("UTC".to_string())
    );
    // The persisted day zone, as `digest_days`' `prior_zone` lookup sees it:
    // strictly before the asked date, nearest first.
    assert_eq!(
        pollster::block_on(digest.latest_zone_before("u-1", "2026-08-19")).expect("prior zone"),
        Some("Asia/Kolkata".to_string())
    );
    assert_eq!(
        pollster::block_on(digest.latest_zone_before("u-1", "2026-08-18")).expect("prior zone"),
        None,
        "the row itself is not history"
    );
    assert_eq!(
        pollster::block_on(digest.latest_zone_before("u-1", "2026-08-01")).expect("prior zone"),
        None
    );
    assert!(pollster::block_on(digest.has_meta("u-1")).expect("has meta"));
    assert!(!pollster::block_on(digest.has_stale_meta("u-1", api_core::ALGO_VERSION))
        .expect("stale"));
    assert!(
        pollster::block_on(digest.has_stale_meta("u-1", api_core::ALGO_VERSION + 1))
            .expect("stale")
    );
}

#[test]
fn d1_digest_loads_events_and_reclassifies_them() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().unwrap();
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed");
    }
    let digest = h.digest();

    let mut event = sample_new_event("g-load", "Deep Work");
    event.start_time = "2026-08-18T09:00:00Z".to_string();
    event.end_time = "2026-08-18T10:30:00Z".to_string();
    event.start_time_zone = "Asia/Kolkata".to_string();
    event.end_time_zone = "Asia/Kolkata".to_string();
    event.category_id = None;
    event.is_self_organized = true;
    event.event_type = "default".to_string();
    pollster::block_on(h.events.upsert(event, NOW_RFC)).expect("upsert");

    let events = pollster::block_on(digest.load_events_touching("u-1", "2026-08-18", "2026-08-18"))
        .expect("load events");
    assert_eq!(events.len(), 1);
    let loaded = &events[0];
    assert!(!loaded.id.is_empty(), "the local id");
    assert_eq!(loaded.title, "Deep Work");
    assert_eq!(loaded.start_time_zone, "Asia/Kolkata");
    assert_eq!(loaded.category_id, None);
    assert!(!loaded.deleted);
    assert!(api_core::include_event(loaded));
    assert_eq!(
        pollster::block_on(digest.earliest_event_date("u-1")).expect("earliest"),
        Some("2026-08-18".to_string())
    );

    // A reclassify batch: living rows after the cursor, oldest local id
    // first, with the stored stamp. The cursor is the LOCAL `calendar_events.id`
    // (the Google id comes back in its own column for pattern scoping).
    let local_id = loaded.id.clone();
    let batch = pollster::block_on(digest.reclassify_batch("u-1", "", 10)).expect("batch");
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].id, local_id);
    assert_eq!(batch[0].google_calendar_id, "primary@example.com");
    assert_eq!(batch[0].title, "Deep Work");
    assert_eq!(batch[0].category_id, None);
    assert_eq!(
        batch[0].category_classified, 1,
        "the write-time upsert classified the row"
    );
    assert!(
        pollster::block_on(digest.reclassify_batch("u-1", &local_id, 10))
            .expect("batch")
            .is_empty(),
        "the cursor is exclusive"
    );

    pollster::block_on(digest.set_category(&local_id, Some("cat-work"))).expect("stamp");
    let stored = pollster::block_on(h.events.get_by_calendar_and_google_id("cal-1", "g-load"))
        .expect("event")
        .expect("living");
    assert_eq!(stored.category_id.as_deref(), Some("cat-work"));
    assert_eq!(
        pollster::block_on(digest.earliest_event_date("u-1")).expect("earliest"),
        Some("2026-08-18".to_string())
    );
    assert_eq!(stored.updated_at, NOW_RFC, "a re-stamp never bumps updated_at");

    // An empty string clears the stamp back to SQL NULL.
    pollster::block_on(digest.set_category(&local_id, None)).expect("clear stamp");
    let cleared = pollster::block_on(h.events.get_by_calendar_and_google_id("cal-1", "g-load"))
        .expect("event")
        .expect("living");
    assert_eq!(cleared.category_id, None);

    // A soft-deleted calendar stops feeding the digest.
    {
        let conn = h.db.lock().unwrap();
        conn.execute(
            "UPDATE google_calendars SET deleted_at = ?1 WHERE id = 'cal-1'",
            rusqlite::params![NOW_RFC],
        )
        .expect("soft delete calendar");
    }
    assert!(
        pollster::block_on(digest.load_events_touching("u-1", "2026-08-01", "2026-09-01"))
            .expect("load events")
            .is_empty()
    );
    assert_eq!(
        pollster::block_on(digest.earliest_event_date("u-1")).expect("earliest"),
        None
    );
}
