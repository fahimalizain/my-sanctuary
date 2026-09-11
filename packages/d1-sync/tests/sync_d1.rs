//! Integration tests: replica apply + health against real Worker migrations
//! (local SQLite = D1 schema). Fail if D1 SQL diverges from in-memory FakeRepo
//! suite behaviour for publish fencing, dirty webhook, and cron notify-after-commit.

use std::sync::Mutex;

use api_core::models::{GoogleCalendar, WatchChannel};
use api_core::oauth::{HttpClient, HttpError};
use api_core::repo::{CalendarEventRepo, CalendarRepo};
use api_core::time::unix_secs_to_rfc3339;
use api_core::token::GoogleAccess;
use api_core::{
    calendar_sync_view, decide_webhook, persist_webhook_decision, replica_query_fingerprint,
    run_fallback_cron, sync_calendar, CalendarError, CalendarReplicaState, CheckpointResult,
    OAuthConfig, OperatorWarningLevel, SyncCalendarOutcome, WebhookPersistResult,
};
use d1_sync::{open_harness, seed_user_token_calendar, SeedOpts};

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
