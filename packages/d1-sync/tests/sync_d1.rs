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

fn seed_event(
    conn: &rusqlite::Connection,
    id: &str,
    google_event_id: &str,
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
            ?3, ?2, '', '2026-08-18T09:00:00Z', '2026-08-18T09:30:00Z', '',
            ?4, '', 0, ?5, ?6, '',
            '', '', 0, '',
            ?3, ?3, NULL
         )",
        rusqlite::params![
            id,
            google_event_id,
            NOW_RFC,
            task_id,
            status,
            recurring_event_id,
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

// ── Digest tables (slice 2) ─────────────────────────────────────────────────

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
