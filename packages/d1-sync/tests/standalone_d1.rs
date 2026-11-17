//! Standalone mode against the real Worker migrations (local SQLite = D1
//! schema).
//!
//! Drives [`api_core::standalone::register`] / `login`, the standalone calendar
//! seed, and the real write journal + [`StandaloneCalendarHttp`] over the
//! rusqlite repos. The regression under test is `sync_enabled = 1` on the
//! seeded standalone calendar: without it the created events save but never
//! appear in `EVENT_LIST_BY_USER_ID_AND_TIME_RANGE_SQL` /
//! `EVENT_LIST_RUNNING_BY_USER_ID_SQL`.

use std::sync::Arc;

use api_core::calendar::{
    create_event, delete_event_for_user, patch_event_fields, update_event_for_user,
    StandaloneCalendarHttp,
};
use api_core::google_color::GOOGLE_EVENT_LABEL_COLORS;
use api_core::models::{
    CalendarEvent, GoogleCalendar, IntegrationType, NewCalendar, NewEventInput, NewLocalCredential,
    NewStandaloneUser, NewUser, PatchEventFields,
};
use api_core::oauth::HttpClient;
use api_core::password::verify_password;
use api_core::realtime::RealtimeNotify;
use api_core::repo::{
    CalendarEventRepo, CalendarRepo, LocalCredentialRepo, RepoError, UserRepo,
    OCCURRENCE_LIST_IN_PROGRESS_SQL, TASK_LIST_IN_PROGRESS_SQL,
};
use api_core::standalone::{
    ensure_standalone_calendar, login, register, LoginInput, RegisterInput, StandaloneAuthConfig,
    StandaloneAuthError,
};
use api_core::token::GoogleAccess;
use api_core::{CalendarError, SessionUser};
use d1_sync::{
    exec, open_harness, query_one, query_vec, seed_user_token_calendar, EmptyTaskCategoryRepo,
    Harness, SeedOpts, SqliteCalendarEventRepo, SqliteCalendarRepo,
};

/// 2023-11-14T22:13:20Z — the frozen "now" every test writes at.
const NOW: i64 = 1_700_000_000;
const NOW_RFC: &str = "2023-11-14T22:13:20Z";
const WINDOW_START: &str = "2023-11-14T00:00:00Z";
const WINDOW_END: &str = "2023-11-15T00:00:00Z";
const PASSWORD: &str = "correct horse battery";

/// These tests never exercise classification: an empty taxonomy stamps NULL.
const NO_TAXONOMY: EmptyTaskCategoryRepo = EmptyTaskCategoryRepo;

struct NoopNotify;

#[async_trait::async_trait(?Send)]
impl RealtimeNotify for NoopNotify {
    async fn calendar_changed(&self, _user_id: &str, _calendar_id: Option<&str>) {}
}

static NOTIFY: NoopNotify = NoopNotify;

fn config() -> StandaloneAuthConfig {
    StandaloneAuthConfig {
        default_time_zone: "UTC".to_string(),
        password_iterations: 1_000,
    }
}

fn access() -> GoogleAccess {
    GoogleAccess {
        access_token: "standalone".to_string(),
        token_type: "Bearer".to_string(),
    }
}

fn register_user(h: &Harness, username: &str, time_zone: Option<&str>) -> SessionUser {
    pollster::block_on(register(
        &h.users(),
        &h.credentials(),
        &h.calendars,
        RegisterInput {
            username: username.to_string(),
            password: PASSWORD.to_string(),
            name: None,
            time_zone: time_zone.map(str::to_string),
        },
        &config(),
        NOW,
    ))
    .expect("register")
}

fn login_user(
    h: &Harness,
    username: &str,
    password: &str,
    tz: &str,
) -> Result<SessionUser, String> {
    pollster::block_on(login(
        &h.users(),
        &h.credentials(),
        &h.calendars,
        LoginInput {
            username: username.to_string(),
            password: password.to_string(),
        },
        &StandaloneAuthConfig {
            default_time_zone: tz.to_string(),
            password_iterations: 1_000,
        },
        NOW + 60,
    ))
    .map_err(|err| err.to_string())
}

/// A stand-in over fresh repo handles on the same connection. Each call gets
/// its own `now_unix` (the stand-in stamps `updated` at construction).
fn stand_in(h: &Harness, user_id: &str, now: i64) -> StandaloneCalendarHttp {
    StandaloneCalendarHttp::new(
        Box::new(SqliteCalendarRepo::new(Arc::clone(&h.db))),
        Box::new(SqliteCalendarEventRepo::new(Arc::clone(&h.db))),
        user_id.to_string(),
        now,
    )
}

fn calendar_by_gcal(h: &Harness, user_id: &str, google_calendar_id: &str) -> GoogleCalendar {
    pollster::block_on(
        h.calendars
            .get_by_google_cal_id(user_id, google_calendar_id),
    )
    .expect("lookup calendar")
    .expect("calendar exists")
}

fn new_event_input(calendar_id: &str, summary: &str, start: &str, end: &str) -> NewEventInput {
    NewEventInput {
        calendar_id: calendar_id.to_string(),
        calendar_id_explicit: true,
        summary: summary.to_string(),
        description: None,
        start: start.to_string(),
        end: end.to_string(),
        task_id: None,
        routine_id: None,
        occurrence_id: None,
        color_hex: None,
        sanctuary_focus: false,
        priority: None,
        difficulty: None,
        is_all_day: None,
        start_time_zone: None,
    }
}

fn create(
    h: &Harness,
    user_id: &str,
    input: &NewEventInput,
    now: i64,
) -> Result<CalendarEvent, CalendarError> {
    let http = stand_in(h, user_id, now);
    pollster::block_on(create_event(
        &http,
        &h.calendars,
        &h.events,
        &h.operations,
        &access(),
        user_id,
        input,
        now,
        &NO_TAXONOMY,
        &NOTIFY,
    ))
    .map(|out| out.event)
}

fn patch(
    h: &Harness,
    user_id: &str,
    calendar_id: &str,
    google_event_id: &str,
    fields: &PatchEventFields,
    now: i64,
) -> Result<CalendarEvent, CalendarError> {
    let http = stand_in(h, user_id, now);
    pollster::block_on(patch_event_fields(
        &http,
        &h.calendars,
        &h.events,
        &h.operations,
        &access(),
        calendar_id,
        google_event_id,
        fields,
        now,
        &NO_TAXONOMY,
        &NOTIFY,
    ))
    .map(|out| out.event)
}

fn google_base(google_calendar_id: &str) -> String {
    format!("https://www.googleapis.com/calendar/v3/calendars/{google_calendar_id}")
}

fn event_url(google_calendar_id: &str, google_event_id: &str) -> String {
    format!(
        "{}/events/{google_event_id}",
        google_base(google_calendar_id)
    )
}

fn insert_task(conn: &rusqlite::Connection, id: &str, user_id: &str, status: &str) {
    exec(
        conn,
        "INSERT INTO tasks (id, user_id, title, status, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)",
        &[&id, &user_id, &"Test task", &status, &NOW_RFC, &NOW_RFC],
    )
    .expect("insert task");
}

/// A raw `calendar_events` row including soft-deleted ones (the repo reads
/// filter, so a tombstone is only observable this way).
fn raw_event(conn: &rusqlite::Connection, id: &str) -> CalendarEvent {
    query_one(conn, "SELECT * FROM calendar_events WHERE id = ?", &[&id])
        .expect("query event")
        .expect("event row")
}

/// `SELECT COUNT(*)` with one text parameter.
fn count(conn: &rusqlite::Connection, sql: &str, param: &str) -> i64 {
    conn.query_row(sql, rusqlite::params![param], |row| row.get(0))
        .expect("count")
}

#[derive(Debug, serde::Deserialize)]
struct UserRow {
    integration_type: String,
    google_id: String,
    email: String,
    picture: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct CalendarRow {
    google_calendar_id: String,
    summary: String,
    time_zone: String,
    is_primary: i64,
    access_role: String,
    sync_enabled: i64,
    sync_status: String,
    initial_sync_complete: i64,
    watch_coverage: String,
    event_coverage: String,
    last_success_at: Option<String>,
    deleted_at: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct IdRow {
    id: String,
}

fn raw_calendar(conn: &rusqlite::Connection, user_id: &str) -> CalendarRow {
    query_one(
        conn,
        "SELECT * FROM google_calendars WHERE user_id = ?",
        &[&user_id],
    )
    .expect("query calendar")
    .expect("calendar row")
}

#[test]
fn register_writes_standalone_identity_credential_and_seeded_calendar() {
    let h = open_harness().expect("harness");
    let session = register_user(&h, "  Ada.Lovelace ", Some("Asia/Kolkata"));

    assert_eq!(session.integration_type, IntegrationType::Standalone);
    assert_eq!(session.email, "ada.lovelace@standalone.invalid");
    assert_eq!(session.picture, "");
    assert_eq!(session.id.len(), 32, "16 random bytes as hex");

    let user = pollster::block_on(h.users().get_by_id(&session.id))
        .expect("user query")
        .expect("user row");
    assert_eq!(user.integration_type, IntegrationType::Standalone);
    assert_eq!(user.google_id, format!("standalone:{}", session.id));
    assert_eq!(user.email, "ada.lovelace@standalone.invalid");
    assert_eq!(user.name, "ada.lovelace", "name defaults to the username");
    assert_eq!(user.created_at, NOW_RFC);
    assert_eq!(user.picture, None, "standalone rows never carry a picture");

    // Raw schema: the migration's `integration_type` default is NOT what a
    // standalone insert writes — it is the explicit `'standalone'` literal.
    {
        let conn = h.db.lock().expect("db");
        let row: UserRow = query_one(
            &conn,
            "SELECT integration_type, google_id, email, picture FROM users WHERE id = ?",
            &[&session.id],
        )
        .expect("user query")
        .expect("user row");
        assert_eq!(row.integration_type, "standalone");
        assert_eq!(row.google_id, format!("standalone:{}", session.id));
        assert!(row.email.ends_with("@standalone.invalid"));
        assert_eq!(row.picture, None);
    }

    let credential = pollster::block_on(h.credentials().get_by_username("ada.lovelace"))
        .expect("credential query")
        .expect("credential row");
    assert_eq!(credential.user_id, session.id);
    assert!(credential.password_hash.starts_with("pbkdf2-sha256$1000$"));
    assert!(verify_password(&credential.password_hash, PASSWORD));
    assert!(!verify_password(
        &credential.password_hash,
        "wrong password"
    ));

    let calendar = calendar_by_gcal(&h, &session.id, &format!("standalone:{}", session.id));
    assert_eq!(calendar.id, format!("standalone-cal-{}", session.id));
    assert_eq!(calendar.summary, "Personal");
    assert_eq!(calendar.time_zone, "Asia/Kolkata");
    assert!(calendar.is_primary);
    assert_eq!(calendar.access_role, "owner");
    assert!(
        calendar.sync_enabled,
        "range/running SQL require sync_enabled=1"
    );
    assert_eq!(
        calendar.sync_status, "disabled",
        "no sync/repair/stale banner"
    );
    assert!(calendar.initial_sync_complete);
    assert_eq!(calendar.last_synced_at.as_deref(), Some(NOW_RFC));
    assert_eq!(calendar.last_success_at.as_deref(), Some(NOW_RFC));

    // Raw schema: booleans are INTEGER 0/1 and the health columns are literal.
    {
        let conn = h.db.lock().expect("db");
        let row = raw_calendar(&conn, &session.id);
        assert_eq!(row.google_calendar_id, format!("standalone:{}", session.id));
        assert_eq!(row.summary, "Personal");
        assert_eq!(row.time_zone, "Asia/Kolkata");
        assert_eq!(row.is_primary, 1);
        assert_eq!(row.access_role, "owner");
        assert_eq!(row.sync_enabled, 1);
        assert_eq!(row.sync_status, "disabled");
        assert_eq!(row.initial_sync_complete, 1);
        assert_eq!(row.watch_coverage, "covered");
        assert_eq!(row.event_coverage, "complete");
        assert_eq!(row.last_success_at.as_deref(), Some(NOW_RFC));
        assert_eq!(row.deleted_at, None);
    }

    let labels: Vec<serde_json::Value> =
        serde_json::from_str(&calendar.event_labels).expect("labels JSON");
    assert_eq!(labels.len(), 24);
    assert_eq!(labels.len(), GOOGLE_EVENT_LABEL_COLORS.len());
    for (index, label) in labels.iter().enumerate() {
        assert_eq!(label["id"], (index + 1).to_string());
        assert_eq!(label["backgroundColor"], GOOGLE_EVENT_LABEL_COLORS[index]);
    }
    assert_eq!(
        calendar.event_labels_updated_at.as_deref(),
        Some(NOW_RFC),
        "the label cache is stamped at seed time"
    );
}

#[test]
fn seed_is_idempotent_and_login_self_heals_a_missing_calendar() {
    let h = open_harness().expect("harness");
    let session = register_user(&h, "ada", Some("Asia/Kolkata"));
    let google_calendar_id = format!("standalone:{}", session.id);

    // Re-seeding (register/login/self-heal) never overwrites the stored zone.
    pollster::block_on(ensure_standalone_calendar(
        &h.calendars,
        &session.id,
        "Europe/Berlin",
        NOW + 3_600,
    ))
    .expect("second seed");
    pollster::block_on(ensure_standalone_calendar(
        &h.calendars,
        &session.id,
        "UTC",
        NOW + 7_200,
    ))
    .expect("third seed");
    assert_eq!(
        calendar_by_gcal(&h, &session.id, &google_calendar_id).time_zone,
        "Asia/Kolkata",
        "DO NOTHING keeps the original zone"
    );

    login_user(&h, "ada", PASSWORD, "UTC").expect("login");
    assert_eq!(
        pollster::block_on(h.calendars.list_by_user_id(&session.id))
            .expect("list")
            .len(),
        1,
        "login must not duplicate the calendar"
    );

    // Hard-delete the row (not a soft delete) and let login re-seed it.
    {
        let conn = h.db.lock().expect("db");
        exec(
            &conn,
            "DELETE FROM google_calendars WHERE user_id = ?",
            &[&session.id],
        )
        .expect("hard delete");
    }
    assert!(pollster::block_on(h.calendars.list_by_user_id(&session.id))
        .expect("list")
        .is_empty());

    login_user(&h, "ada", PASSWORD, "America/New_York").expect("login self-heal");

    let healed = calendar_by_gcal(&h, &session.id, &google_calendar_id);
    assert_eq!(healed.id, format!("standalone-cal-{}", session.id));
    assert_eq!(
        healed.time_zone, "America/New_York",
        "login re-seeds with the config default zone"
    );
    assert!(healed.sync_enabled);
    assert_eq!(healed.sync_status, "disabled");
}

#[test]
fn login_accepts_only_the_right_password() {
    let h = open_harness().expect("harness");
    let session = register_user(&h, "ada", None);

    let logged_in = login_user(&h, "  ADA ", PASSWORD, "UTC").expect("login");
    assert_eq!(logged_in, session);

    for (username, password) in [
        ("ada", "wrong password"),
        ("ada", ""),
        ("nobody", PASSWORD),
        ("a b", PASSWORD),
    ] {
        let err = login_user(&h, username, password, "UTC").unwrap_err();
        assert!(
            err.contains("invalid username or password"),
            "{username:?}/{password:?}: {err}"
        );
    }
}

#[test]
fn duplicate_identity_writes_are_conflicts_and_focus_round_trips() {
    let h = open_harness().expect("harness");
    let session = register_user(&h, "ada", None);

    // The pre-check makes the common duplicate cheap...
    let err = pollster::block_on(register(
        &h.users(),
        &h.credentials(),
        &h.calendars,
        RegisterInput {
            username: " ADA ".to_string(),
            password: PASSWORD.to_string(),
            name: None,
            time_zone: None,
        },
        &config(),
        NOW,
    ))
    .unwrap_err();
    assert_eq!(err, StandaloneAuthError::UsernameTaken);

    // ...and the schema UNIQUE is what a registration race actually hits.
    let err = pollster::block_on(h.credentials().insert(NewLocalCredential {
        user_id: session.id.clone(),
        username: "ada".to_string(),
        password_hash: "pbkdf2-sha256$1$c2FsdA==$aGFzaA==".to_string(),
        created_at: NOW_RFC.to_string(),
        updated_at: NOW_RFC.to_string(),
    }))
    .unwrap_err();
    assert_eq!(err, RepoError::Conflict);

    let err = pollster::block_on(h.users().create_standalone(NewStandaloneUser {
        id: session.id.clone(),
        google_id: format!("standalone:{}", session.id),
        email: "other@standalone.invalid".to_string(),
        name: "Other".to_string(),
        created_at: NOW_RFC.to_string(),
        updated_at: NOW_RFC.to_string(),
    }))
    .unwrap_err();
    assert_eq!(err, RepoError::Conflict);

    // The one-focus pointer is a nullable TEXT column: set and clear.
    pollster::block_on(
        h.users()
            .set_focused_task_id(&session.id, Some("task-1"), NOW_RFC),
    )
    .expect("focus");
    assert_eq!(
        pollster::block_on(h.users().get_by_id(&session.id))
            .expect("get")
            .expect("row")
            .focused_task_id
            .as_deref(),
        Some("task-1")
    );
    pollster::block_on(h.users().set_focused_task_id(&session.id, None, NOW_RFC)).expect("unfocus");
    assert_eq!(
        pollster::block_on(h.users().get_by_id(&session.id))
            .expect("get")
            .expect("row")
            .focused_task_id,
        None
    );
}

#[test]
fn register_creates_exactly_one_user_one_credential_and_one_calendar() {
    let h = open_harness().expect("harness");
    let session = register_user(&h, "ada", Some("Asia/Kolkata"));

    let conn = h.db.lock().expect("db");
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM users WHERE id = ?",
            &session.id
        ),
        1
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM local_credentials WHERE user_id = ?",
            &session.id,
        ),
        1,
        "a standalone account always has exactly one credential"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM google_calendars WHERE user_id = ?",
            &session.id,
        ),
        1,
        "the seed writes one calendar"
    );
    // Nothing was orphaned: every user row is paired with a credential.
    let orphaned: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM users WHERE id NOT IN (SELECT user_id FROM local_credentials)",
            [],
            |row| row.get(0),
        )
        .expect("orphan count");
    assert_eq!(orphaned, 0, "no user row exists without a credential");
}

#[test]
fn an_orphan_user_row_makes_register_username_taken_and_writes_no_credential() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().expect("db");
        // The row the old non-atomic register could leave behind: a `users`
        // row with the standalone email and no credential. The cheap
        // `get_by_username` pre-check cannot see it, so register must reach the
        // schema UNIQUE and answer 409 (`UsernameTaken`), not a 500.
        exec(
            &conn,
            "INSERT INTO users (id, google_id, email, name, integration_type, created_at, updated_at)
             VALUES ('orphan', 'standalone:orphan', 'ada@standalone.invalid', 'Ada', 'standalone', ?, ?)",
            &[&NOW_RFC, &NOW_RFC],
        )
        .expect("orphan user row");
    }

    let err = pollster::block_on(register(
        &h.users(),
        &h.credentials(),
        &h.calendars,
        RegisterInput {
            username: "ada".to_string(),
            password: PASSWORD.to_string(),
            name: None,
            time_zone: None,
        },
        &config(),
        NOW,
    ))
    .unwrap_err();
    assert_eq!(
        err,
        StandaloneAuthError::UsernameTaken,
        "a UNIQUE email is a 409, never a backend 500"
    );

    let conn = h.db.lock().expect("db");
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM local_credentials WHERE username = ?",
            "ada",
        ),
        0,
        "the failed attempt wrote no credential"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM users WHERE email = ?",
            "ada@standalone.invalid"
        ),
        1,
        "the orphan row is untouched and no second user row was added"
    );
    assert!(
        query_vec::<IdRow>(
            &conn,
            "SELECT id FROM google_calendars WHERE user_id = 'orphan'",
            &[],
        )
        .expect("calendar rows")
        .is_empty(),
        "no calendar is seeded for the rejected registration"
    );
}

#[test]
fn create_standalone_account_rolls_back_the_user_row_on_a_duplicate_username() {
    let h = open_harness().expect("harness");
    register_user(&h, "ada", None);

    // Fresh id and email, so the `users` INSERT itself would succeed; only the
    // credential's UNIQUE `username` fails. Both rows must roll back.
    let err = pollster::block_on(h.users().create_standalone_account(
        NewStandaloneUser {
            id: "second".to_string(),
            google_id: "standalone:second".to_string(),
            email: "second@standalone.invalid".to_string(),
            name: "Second".to_string(),
            created_at: NOW_RFC.to_string(),
            updated_at: NOW_RFC.to_string(),
        },
        NewLocalCredential {
            user_id: "second".to_string(),
            username: "ada".to_string(),
            password_hash: "pbkdf2-sha256$1$c2FsdA==$aGFzaA==".to_string(),
            created_at: NOW_RFC.to_string(),
            updated_at: NOW_RFC.to_string(),
        },
    ))
    .unwrap_err();
    assert_eq!(err, RepoError::Conflict);

    assert!(
        pollster::block_on(h.users().get_by_id("second"))
            .expect("get second")
            .is_none(),
        "the user INSERT is rolled back with the credential INSERT"
    );
    let conn = h.db.lock().expect("db");
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM users WHERE id = ?", "second"),
        0
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM local_credentials WHERE username = ?",
            "ada",
        ),
        1,
        "the original credential is untouched"
    );
}

#[test]
fn journal_create_patch_cancel_roundtrip_keeps_task_and_occurrence_links() {
    let h = open_harness().expect("harness");
    let session = register_user(&h, "ada", Some("Asia/Kolkata"));
    let google_calendar_id = format!("standalone:{}", session.id);
    let calendar = calendar_by_gcal(&h, &session.id, &google_calendar_id);

    {
        let conn = h.db.lock().expect("db");
        insert_task(&conn, "task-1", &session.id, "IN_PROGRESS");
    }

    let mut input = new_event_input(
        &calendar.id,
        "Deep work",
        "2023-11-14T21:00:00Z",
        "2023-11-14T23:00:00Z",
    );
    input.task_id = Some("task-1".to_string());
    input.sanctuary_focus = true;

    let created = create(&h, &session.id, &input, NOW).expect("create");
    assert_eq!(created.title, "Deep work");
    assert_eq!(created.task_id, "task-1");
    assert_eq!(created.start_time, "2023-11-14T21:00:00Z");
    assert_eq!(created.end_time, "2023-11-14T23:00:00Z");
    assert!(
        created.is_self_organized,
        "the stand-in echoes organizer.self"
    );
    assert_eq!(created.status, "confirmed");
    assert_eq!(created.sequence, 0);
    assert_eq!(
        created.ical_uid,
        format!("sanc-{}@standalone.invalid", created.google_event_id)
    );
    assert!(
        created.google_etag.starts_with("\"0-"),
        "etag: {}",
        created.google_etag
    );
    let raw: serde_json::Value = serde_json::from_str(&created.raw_json).expect("raw JSON");
    assert_eq!(
        raw["extendedProperties"]["shared"]["sanctuary_task_id"],
        "task-1"
    );
    assert_eq!(raw["extendedProperties"]["shared"]["sanctuary_focus"], "1");

    // A summary-only patch round-trips the whole envelope (`sequence` bumps,
    // organizer/links survive) and keeps the app-owned `task_id` column.
    let patched = patch(
        &h,
        &session.id,
        &calendar.id,
        &created.google_event_id,
        &PatchEventFields {
            summary: Some("Renamed".to_string()),
            ..Default::default()
        },
        NOW,
    )
    .expect("patch summary");
    assert_eq!(patched.id, created.id);
    assert_eq!(patched.title, "Renamed");
    assert_eq!(patched.task_id, "task-1");
    assert!(patched.is_self_organized);
    assert_eq!(patched.sequence, 1);
    let raw: serde_json::Value = serde_json::from_str(&patched.raw_json).expect("raw JSON");
    assert_eq!(
        raw["extendedProperties"]["shared"]["sanctuary_task_id"],
        "task-1"
    );

    // Occurrence carriers live only in `extendedProperties` (no task column);
    // a patch must merge, never replace, the shared map.
    let mut occurrence_input = new_event_input(
        &calendar.id,
        "Morning routine",
        "2023-11-14T20:00:00Z",
        "2023-11-14T21:00:00Z",
    );
    occurrence_input.routine_id = Some("routine-1".to_string());
    occurrence_input.occurrence_id = Some("occ-1".to_string());
    let occurrence = create(&h, &session.id, &occurrence_input, NOW).expect("occurrence create");
    assert_eq!(occurrence.task_id, "");
    let raw: serde_json::Value = serde_json::from_str(&occurrence.raw_json).expect("raw JSON");
    assert_eq!(
        raw["extendedProperties"]["shared"]["sanctuary_routine_id"],
        "routine-1"
    );
    assert_eq!(
        raw["extendedProperties"]["shared"]["sanctuary_occurrence_id"],
        "occ-1"
    );

    let patched = patch(
        &h,
        &session.id,
        &calendar.id,
        &occurrence.google_event_id,
        &PatchEventFields {
            description: Some("updated notes".to_string()),
            ..Default::default()
        },
        NOW,
    )
    .expect("patch occurrence");
    let raw: serde_json::Value = serde_json::from_str(&patched.raw_json).expect("raw JSON");
    assert_eq!(raw["description"], "updated notes");
    assert_eq!(
        raw["extendedProperties"]["shared"]["sanctuary_routine_id"], "routine-1",
        "a patch must not drop the occurrence carrier"
    );
    assert_eq!(
        raw["extendedProperties"]["shared"]["sanctuary_occurrence_id"],
        "occ-1"
    );

    // Cancel: the journal patches `status: cancelled` then soft-deletes.
    pollster::block_on(delete_event_for_user(
        &stand_in(&h, &session.id, NOW),
        &h.calendars,
        &h.events,
        &h.operations,
        &access(),
        &session.id,
        &created.id,
        NOW,
        &NOTIFY,
    ))
    .expect("cancel");

    assert!(
        pollster::block_on(h.events.get_by_id(&created.id))
            .expect("get")
            .is_none(),
        "the local row is soft-deleted"
    );
    {
        let conn = h.db.lock().expect("db");
        let tombstone = raw_event(&conn, &created.id);
        assert!(tombstone.deleted_at.is_some());
        // The journal's cancel is a `status: cancelled` PATCH followed by a
        // local soft delete (the tombstone keeps the pre-cancel payload).
        let op = query_one::<api_core::models::CalendarEventOperation>(
            &conn,
            "SELECT * FROM calendar_event_operations ORDER BY rowid DESC LIMIT 1",
            &[],
        )
        .expect("query op")
        .expect("op row");
        assert_eq!(op.verb, "delete");
        assert_eq!(op.status, "cache_applied");
        let payload: serde_json::Value =
            serde_json::from_str(&op.payload_json).expect("payload JSON");
        assert_eq!(payload["status"], "cancelled");
    }
}

#[test]
fn journal_converts_timed_to_all_day_and_back() {
    let h = open_harness().expect("harness");
    let session = register_user(&h, "ada", Some("America/New_York"));
    let google_calendar_id = format!("standalone:{}", session.id);
    let calendar = calendar_by_gcal(&h, &session.id, &google_calendar_id);

    let created = create(
        &h,
        &session.id,
        &new_event_input(
            &calendar.id,
            "Focus",
            "2023-11-14T14:00:00-05:00",
            "2023-11-14T15:00:00-05:00",
        ),
        NOW,
    )
    .expect("create");
    // Insert already normalizes a local offset to UTC `…Z` for the range SQL.
    assert_eq!(created.start_time, "2023-11-14T19:00:00Z");
    assert!(!created.is_all_day);

    let all_day = patch(
        &h,
        &session.id,
        &calendar.id,
        &created.google_event_id,
        &PatchEventFields {
            start: Some("2026-08-20".to_string()),
            end: Some("2026-08-21".to_string()),
            is_all_day: Some(true),
            ..Default::default()
        },
        NOW,
    )
    .expect("patch to all-day");
    assert!(all_day.is_all_day);
    assert_eq!(all_day.start_time, "2026-08-20T00:00:00Z");
    assert_eq!(all_day.end_time, "2026-08-21T00:00:00Z");
    let raw: serde_json::Value = serde_json::from_str(&all_day.raw_json).expect("raw JSON");
    assert_eq!(raw["start"]["date"], "2026-08-20");
    assert!(
        raw["start"]["dateTime"].is_null(),
        "an all-day start drops dateTime"
    );

    let timed = patch(
        &h,
        &session.id,
        &calendar.id,
        &created.google_event_id,
        &PatchEventFields {
            start: Some("2026-08-20T09:00:00-04:00".to_string()),
            end: Some("2026-08-20T10:00:00-04:00".to_string()),
            is_all_day: Some(false),
            start_time_zone: Some("America/New_York".to_string()),
            ..Default::default()
        },
        NOW,
    )
    .expect("patch back to timed");
    assert!(!timed.is_all_day);
    assert_eq!(timed.start_time, "2026-08-20T13:00:00Z");
    assert_eq!(timed.end_time, "2026-08-20T14:00:00Z");
    assert_eq!(timed.start_time_zone, "America/New_York");
    assert_eq!(timed.end_time_zone, "America/New_York");
    let raw: serde_json::Value = serde_json::from_str(&timed.raw_json).expect("raw JSON");
    assert_eq!(raw["start"]["dateTime"], "2026-08-20T13:00:00Z");
    assert!(
        raw["start"]["date"].is_null(),
        "a timed start drops the all-day date"
    );
}

#[test]
fn etag_changes_on_every_write_and_a_stale_if_match_retries() {
    let h = open_harness().expect("harness");
    let session = register_user(&h, "ada", None);
    let google_calendar_id = format!("standalone:{}", session.id);
    let calendar = calendar_by_gcal(&h, &session.id, &google_calendar_id);

    let created = create(
        &h,
        &session.id,
        &new_event_input(
            &calendar.id,
            "Focus",
            "2023-11-14T21:00:00Z",
            "2023-11-14T23:00:00Z",
        ),
        NOW,
    )
    .expect("create");

    // Two patches at the SAME `now_unix`: the etag must still change (it is
    // sequence + random bytes, never the one-second timestamp).
    let first = patch(
        &h,
        &session.id,
        &calendar.id,
        &created.google_event_id,
        &PatchEventFields {
            summary: Some("one".to_string()),
            ..Default::default()
        },
        NOW,
    )
    .expect("first patch");
    let second = patch(
        &h,
        &session.id,
        &calendar.id,
        &created.google_event_id,
        &PatchEventFields {
            summary: Some("two".to_string()),
            ..Default::default()
        },
        NOW,
    )
    .expect("second patch");

    assert_ne!(created.google_etag, first.google_etag);
    assert_ne!(first.google_etag, second.google_etag);
    assert_eq!(second.sequence, 2);
    assert_eq!(
        pollster::block_on(h.events.get_by_id(&created.id))
            .expect("get")
            .expect("row")
            .google_etag,
        second.google_etag
    );

    // Force a stale precondition: the journal sends the stored column as
    // `If-Match`, the stand-in compares it to the etag inside `raw_json`.
    {
        let conn = h.db.lock().expect("db");
        exec(
            &conn,
            "UPDATE calendar_events SET google_etag = 'stale-etag' WHERE id = ?",
            &[&created.id],
        )
        .expect("stale etag");
    }

    let retried = patch(
        &h,
        &session.id,
        &calendar.id,
        &created.google_event_id,
        &PatchEventFields {
            summary: Some("after retry".to_string()),
            ..Default::default()
        },
        NOW,
    )
    .expect("the journal refreshes the etag after 412 and retries");
    assert_eq!(retried.title, "after retry");
    assert_ne!(retried.google_etag, "stale-etag");
    assert_eq!(retried.sequence, 3);

    {
        let conn = h.db.lock().expect("db");
        let op = query_one::<api_core::models::CalendarEventOperation>(
            &conn,
            "SELECT * FROM calendar_event_operations ORDER BY rowid DESC LIMIT 1",
            &[],
        )
        .expect("query op")
        .expect("op row");
        assert_eq!(op.verb, "patch");
        assert_eq!(op.status, "cache_applied");
        assert_eq!(op.attempt_count, 1, "one 412 retry was recorded");
    }
}

#[test]
fn journal_move_relocates_the_row_and_404s_for_bad_destinations() {
    let h = open_harness().expect("harness");
    let session = register_user(&h, "ada", None);
    let google_calendar_id = format!("standalone:{}", session.id);
    let source = calendar_by_gcal(&h, &session.id, &google_calendar_id);

    pollster::block_on(h.calendars.upsert(NewCalendar {
        user_id: session.id.clone(),
        google_calendar_id: format!("{google_calendar_id}-two"),
        summary: "Second".to_string(),
        time_zone: "UTC".to_string(),
        is_primary: false,
        access_role: "owner".to_string(),
        sync_enabled: true,
        sync_token: String::new(),
        last_synced_at: None,
    }))
    .expect("second calendar");
    let destination = calendar_by_gcal(&h, &session.id, &format!("{google_calendar_id}-two"));

    let created = create(
        &h,
        &session.id,
        &new_event_input(
            &source.id,
            "Move me",
            "2023-11-14T21:00:00Z",
            "2023-11-14T23:00:00Z",
        ),
        NOW,
    )
    .expect("create");

    let moved = pollster::block_on(update_event_for_user(
        &stand_in(&h, &session.id, NOW),
        &h.calendars,
        &h.events,
        &h.operations,
        &access(),
        &session.id,
        &created.id,
        &PatchEventFields {
            calendar_id: Some(destination.id.clone()),
            ..Default::default()
        },
        NOW,
        &NO_TAXONOMY,
        &NOTIFY,
    ))
    .expect("move")
    .event;
    assert_eq!(moved.id, created.id, "the local row moves in place");
    assert_eq!(moved.calendar_id, destination.id);
    assert_eq!(
        pollster::block_on(h.events.get_by_id(&created.id))
            .expect("get")
            .expect("row")
            .calendar_id,
        destination.id
    );

    // Unknown destination.
    let err = pollster::block_on(update_event_for_user(
        &stand_in(&h, &session.id, NOW),
        &h.calendars,
        &h.events,
        &h.operations,
        &access(),
        &session.id,
        &created.id,
        &PatchEventFields {
            calendar_id: Some("no-such-calendar".to_string()),
            ..Default::default()
        },
        NOW,
        &NO_TAXONOMY,
        &NOTIFY,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::NotFound), "{err:?}");

    // A destination owned by another user is a 404 (no cross-user move).
    {
        let conn = h.db.lock().expect("db");
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("google seed");
    }
    let err = pollster::block_on(update_event_for_user(
        &stand_in(&h, &session.id, NOW),
        &h.calendars,
        &h.events,
        &h.operations,
        &access(),
        &session.id,
        &created.id,
        &PatchEventFields {
            calendar_id: Some("cal-1".to_string()),
            ..Default::default()
        },
        NOW,
        &NO_TAXONOMY,
        &NOTIFY,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::NotFound), "{err:?}");

    // Missing local event id.
    let err = pollster::block_on(delete_event_for_user(
        &stand_in(&h, &session.id, NOW),
        &h.calendars,
        &h.events,
        &h.operations,
        &access(),
        &session.id,
        "missing-event",
        NOW,
        &NOTIFY,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::NotFound), "{err:?}");

    // Soft-deleted local row.
    {
        let conn = h.db.lock().expect("db");
        exec(
            &conn,
            "UPDATE calendar_events SET deleted_at = ? WHERE id = ?",
            &[&NOW_RFC, &created.id],
        )
        .expect("soft delete");
    }
    let err = pollster::block_on(delete_event_for_user(
        &stand_in(&h, &session.id, NOW),
        &h.calendars,
        &h.events,
        &h.operations,
        &access(),
        &session.id,
        &created.id,
        NOW,
        &NOTIFY,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::NotFound), "{err:?}");
}

#[test]
fn watch_stop_and_list_are_501_and_empty_raw_json_is_500() {
    let h = open_harness().expect("harness");
    let session = register_user(&h, "ada", None);
    let google_calendar_id = format!("standalone:{}", session.id);
    let calendar = calendar_by_gcal(&h, &session.id, &google_calendar_id);
    let http = stand_in(&h, &session.id, NOW);

    // A 404 here would make the worker disable the calendar and hide events.
    let (status, _) = pollster::block_on(http.post_json(
        &format!("{}/events/watch", google_base(&google_calendar_id)),
        "standalone",
        b"{}",
    ))
    .expect("watch");
    assert_eq!(status, 501);

    let (status, _) = pollster::block_on(http.post_json(
        "https://www.googleapis.com/calendar/v3/channels/stop",
        "standalone",
        b"{}",
    ))
    .expect("channels.stop");
    assert_eq!(status, 501);

    let (status, _) = pollster::block_on(http.get_bearer_raw(
        &format!("{}/events", google_base(&google_calendar_id)),
        "standalone",
    ))
    .expect("events.list");
    assert_eq!(status, 501);

    let created = create(
        &h,
        &session.id,
        &new_event_input(
            &calendar.id,
            "Payload",
            "2023-11-14T21:00:00Z",
            "2023-11-14T23:00:00Z",
        ),
        NOW,
    )
    .expect("create");

    {
        let conn = h.db.lock().expect("db");
        exec(
            &conn,
            "UPDATE calendar_events SET raw_json = '' WHERE id = ?",
            &[&created.id],
        )
        .expect("blank raw");
    }
    let url = event_url(&google_calendar_id, &created.google_event_id);
    let (status, body) =
        pollster::block_on(http.get_bearer_raw(&url, "standalone")).expect("get blank raw");
    assert_eq!(status, 500, "columns lose occurrence/task links");
    assert!(String::from_utf8_lossy(&body).contains("no stored payload"));

    let (status, _) =
        pollster::block_on(http.patch_json(&url, "standalone", br#"{"summary":"x"}"#))
            .expect("patch blank raw");
    assert_eq!(status, 500);
}

#[test]
fn standalone_event_is_visible_to_range_and_running_queries() {
    let h = open_harness().expect("harness");
    let session = register_user(&h, "ada", None);
    let google_calendar_id = format!("standalone:{}", session.id);
    let calendar = calendar_by_gcal(&h, &session.id, &google_calendar_id);

    // `sync_enabled = 1` is the whole point of the seed: the two SQL filters
    // below join on it, so a 0 would save the event but never show it.
    assert!(calendar.sync_enabled);
    {
        let conn = h.db.lock().expect("db");
        let sync_enabled: i64 = conn
            .query_row(
                "SELECT sync_enabled FROM google_calendars WHERE id = ?",
                rusqlite::params![calendar.id],
                |row| row.get(0),
            )
            .expect("sync_enabled");
        assert_eq!(sync_enabled, 1);
        insert_task(&conn, "task-1", &session.id, "IN_PROGRESS");
    }

    let mut input = new_event_input(
        &calendar.id,
        "Running",
        "2023-11-14T21:00:00Z",
        "2023-11-14T23:00:00Z",
    );
    input.task_id = Some("task-1".to_string());
    let event = create(&h, &session.id, &input, NOW).expect("create");
    assert_eq!(
        event.task_id, "task-1",
        "the running query needs the carrier"
    );

    // A task really is in progress for this user.
    {
        let conn = h.db.lock().expect("db");
        let status: String = conn
            .query_row(
                "SELECT status FROM tasks WHERE id = ?",
                rusqlite::params!["task-1"],
                |row| row.get(0),
            )
            .expect("task status");
        assert_eq!(status, "IN_PROGRESS");
    }

    let range = pollster::block_on(h.events.list_by_user_id_and_time_range(
        &session.id,
        WINDOW_START,
        WINDOW_END,
    ))
    .expect("range");
    assert_eq!(range.len(), 1, "EVENT_LIST_BY_USER_ID_AND_TIME_RANGE_SQL");
    assert_eq!(range[0].id, event.id);

    let running = pollster::block_on(h.events.list_running_by_user_id(&session.id, NOW_RFC))
        .expect("running");
    assert_eq!(running.len(), 1, "EVENT_LIST_RUNNING_BY_USER_ID_SQL");
    assert_eq!(running[0].id, event.id);
    assert_eq!(running[0].task_id, "task-1");
    assert!(!running[0].is_all_day);

    // Explicit negative control: with `sync_enabled = 0` the SAME row is
    // invisible to both queries. This must fail if the seed ever stops writing
    // `sync_enabled = 1`.
    pollster::block_on(h.calendars.set_sync_enabled(&calendar.id, false, NOW_RFC))
        .expect("disable");
    assert!(pollster::block_on(h.events.list_by_user_id_and_time_range(
        &session.id,
        WINDOW_START,
        WINDOW_END
    ))
    .expect("range disabled")
    .is_empty());
    assert!(
        pollster::block_on(h.events.list_running_by_user_id(&session.id, NOW_RFC))
            .expect("running disabled")
            .is_empty()
    );

    pollster::block_on(h.calendars.set_sync_enabled(&calendar.id, true, NOW_RFC))
        .expect("re-enable");
    assert_eq!(
        pollster::block_on(h.events.list_by_user_id_and_time_range(
            &session.id,
            WINDOW_START,
            WINDOW_END
        ))
        .expect("range re-enabled")
        .len(),
        1
    );
    assert_eq!(
        pollster::block_on(h.events.list_running_by_user_id(&session.id, NOW_RFC))
            .expect("running re-enabled")
            .len(),
        1
    );
}

#[test]
fn standalone_users_are_isolated_and_a_standalone_email_does_not_block_google() {
    let h = open_harness().expect("harness");
    let ada = register_user(&h, "ada", None);
    let grace = register_user(&h, "grace", None);
    assert_ne!(ada.id, grace.id);

    let ada_gcal = format!("standalone:{}", ada.id);
    let ada_calendar = calendar_by_gcal(&h, &ada.id, &ada_gcal);
    let event = create(
        &h,
        &ada.id,
        &new_event_input(
            &ada_calendar.id,
            "Private",
            "2023-11-14T21:00:00Z",
            "2023-11-14T23:00:00Z",
        ),
        NOW,
    )
    .expect("create");

    // Grace's stand-in must not resolve Ada's calendar at all (always keyed on
    // `(user_id, google_calendar_id)`).
    let grace_http = stand_in(&h, &grace.id, NOW);
    let url = event_url(&ada_gcal, &event.google_event_id);
    let (status, _) =
        pollster::block_on(grace_http.get_bearer_raw(&url, "standalone")).expect("cross-user get");
    assert_eq!(status, 404);
    let (status, _) =
        pollster::block_on(grace_http.patch_json(&url, "standalone", br#"{"summary":"x"}"#))
            .expect("cross-user patch");
    assert_eq!(status, 404);

    // The journal's ownership guard rejects the local id before any HTTP.
    let err = pollster::block_on(delete_event_for_user(
        &stand_in(&h, &grace.id, NOW),
        &h.calendars,
        &h.events,
        &h.operations,
        &access(),
        &grace.id,
        &event.id,
        NOW,
        &NOTIFY,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::NotFound), "{err:?}");

    let err = pollster::block_on(update_event_for_user(
        &stand_in(&h, &grace.id, NOW),
        &h.calendars,
        &h.events,
        &h.operations,
        &access(),
        &grace.id,
        &event.id,
        &PatchEventFields {
            summary: Some("stolen".to_string()),
            ..Default::default()
        },
        NOW,
        &NO_TAXONOMY,
        &NOTIFY,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::NotFound), "{err:?}");

    assert_eq!(
        pollster::block_on(h.events.get_by_id(&event.id))
            .expect("get")
            .expect("row")
            .title,
        "Private",
        "the isolation attempts changed nothing"
    );

    // Ada's address is `ada@standalone.invalid`; a real Google email is a
    // different UNIQUE slot, so the Google login still creates its row.
    let google = pollster::block_on(h.users().upsert_by_google_id(NewUser {
        google_id: "g-ada".to_string(),
        email: "ada@example.com".to_string(),
        name: "Ada Google".to_string(),
        picture: Some("https://example.com/ada.png".to_string()),
    }))
    .expect("google upsert");
    assert_eq!(google.integration_type, IntegrationType::Google);
    assert_eq!(google.email, "ada@example.com");
    assert_eq!(
        pollster::block_on(h.users().get_by_google_id("g-ada"))
            .expect("lookup")
            .expect("row")
            .id,
        google.id
    );
    // Both identities coexist; the standalone login is untouched.
    assert_eq!(
        pollster::block_on(h.users().get_by_id(&ada.id))
            .expect("ada")
            .expect("row")
            .integration_type,
        IntegrationType::Standalone
    );
    login_user(&h, "ada", PASSWORD, "UTC").expect("standalone still logs in");
}

#[test]
fn cron_filters_exclude_standalone_users() {
    let h = open_harness().expect("harness");
    let local = register_user(&h, "local", None);

    {
        let conn = h.db.lock().expect("db");
        // Google user `u-1` + calendar `cal-1` (sync enabled).
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("google seed");
    }

    {
        let conn = h.db.lock().expect("db");
        insert_task(&conn, "task-local", &local.id, "IN_PROGRESS");
        insert_task(&conn, "task-google", "u-1", "IN_PROGRESS");
        insert_task(&conn, "task-google-done", "u-1", "COMPLETED");

        for (routine_id, user_id) in [
            ("routine-local", local.id.as_str()),
            ("routine-google", "u-1"),
        ] {
            exec(
                &conn,
                "INSERT INTO routines (id, user_id, title, rrule, sort_order, created_at, updated_at)
                 VALUES (?, ?, 'Morning', 'DTSTART:20231114T090000Z\nRRULE:FREQ=DAILY', 0, ?, ?)",
                &[&routine_id, &user_id, &NOW_RFC, &NOW_RFC],
            )
            .expect("insert routine");
        }

        for (occurrence_id, routine_id, user_id) in [
            ("occ-local", "routine-local", local.id.as_str()),
            ("occ-google", "routine-google", "u-1"),
        ] {
            exec(
                &conn,
                "INSERT INTO routine_occurrences
                    (id, routine_id, user_id, local_date, status, calendar_id,
                     google_event_id, created_at, updated_at)
                 VALUES (?, ?, ?, '2023-11-14', 'in_progress', 'cal-1', ?, ?, ?)",
                &[
                    &occurrence_id,
                    &routine_id,
                    &user_id,
                    &format!("g-{occurrence_id}"),
                    &NOW_RFC,
                    &NOW_RFC,
                ],
            )
            .expect("insert occurrence");
        }
    }

    let user_ids =
        pollster::block_on(h.calendars.list_user_ids_with_calendars()).expect("calendar user ids");
    assert_eq!(
        user_ids,
        vec!["u-1".to_string()],
        "CALENDAR_LIST_USER_IDS_SQL is Google-only"
    );
    let sync_enabled = pollster::block_on(h.calendars.list_sync_enabled()).expect("sync list");
    assert_eq!(sync_enabled.len(), 1);
    assert_eq!(sync_enabled[0].id, "cal-1");

    let conn = h.db.lock().expect("db");
    let tasks: Vec<IdRow> = query_vec(&conn, TASK_LIST_IN_PROGRESS_SQL, &[]).expect("tasks");
    assert_eq!(
        tasks.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        vec!["task-google"],
        "TASK_LIST_IN_PROGRESS_SQL keeps standalone tasks out"
    );
    let occurrences: Vec<IdRow> =
        query_vec(&conn, OCCURRENCE_LIST_IN_PROGRESS_SQL, &[]).expect("occurrences");
    assert_eq!(
        occurrences
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        vec!["occ-google"],
        "OCCURRENCE_LIST_IN_PROGRESS_SQL keeps standalone occurrences out"
    );
}

#[test]
fn cron_filters_return_nothing_with_only_standalone_users() {
    let h = open_harness().expect("harness");
    let local = register_user(&h, "local", None);

    {
        let conn = h.db.lock().expect("db");
        insert_task(&conn, "task-local", &local.id, "IN_PROGRESS");
        exec(
            &conn,
            "INSERT INTO routines (id, user_id, title, rrule, sort_order, created_at, updated_at)
             VALUES ('routine-local', ?, 'Morning', 'DTSTART:20231114T090000Z\nRRULE:FREQ=DAILY', 0, ?, ?)",
            &[&local.id, &NOW_RFC, &NOW_RFC],
        )
        .expect("insert routine");
        exec(
            &conn,
            "INSERT INTO routine_occurrences
                (id, routine_id, user_id, local_date, status, calendar_id,
                 google_event_id, created_at, updated_at)
             VALUES ('occ-local', 'routine-local', ?, '2023-11-14', 'in_progress',
                     'standalone-cal', 'g-occ', ?, ?)",
            &[&local.id, &NOW_RFC, &NOW_RFC],
        )
        .expect("insert occurrence");
    }

    // The standalone calendar IS sync_enabled, so only the integration filter
    // keeps it out of the fallback cron.
    assert!(
        pollster::block_on(h.calendars.list_sync_enabled())
            .expect("sync list")
            .is_empty(),
        "CALENDAR_LIST_SYNC_ENABLED_SQL must not return standalone calendars"
    );
    assert!(
        pollster::block_on(h.calendars.list_user_ids_with_calendars())
            .expect("user ids")
            .is_empty()
    );

    let conn = h.db.lock().expect("db");
    assert!(query_vec::<IdRow>(&conn, TASK_LIST_IN_PROGRESS_SQL, &[])
        .expect("tasks")
        .is_empty());
    assert!(
        query_vec::<IdRow>(&conn, OCCURRENCE_LIST_IN_PROGRESS_SQL, &[])
            .expect("occurrences")
            .is_empty()
    );
}
