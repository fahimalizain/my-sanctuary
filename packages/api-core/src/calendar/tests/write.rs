use super::support::*;
use crate::calendar::apply::{map_google_event, GoogleEvent, GoogleEventTime};
use crate::calendar::write::build_shared_properties;
use crate::calendar::{
    create_event, delete_event, delete_event_for_user, patch_event, patch_event_fields,
    update_event_for_user, CalendarError,
};
use crate::models::{
    GoogleCalendar, PatchEventFields, OP_STATUS_CACHE_APPLIED, OP_STATUS_CONFLICT,
    OP_STATUS_FAILED, OP_STATUS_GOOGLE_COMMITTED, OP_VERB_DELETE, OP_VERB_INSERT, OP_VERB_MOVE,
    OP_VERB_PATCH,
};
use crate::repo::CalendarEventRepo;

const MINTED_ID: &str = "sanc0123456789abcdef0123456789ab";

// ──────────────────────────────────────────
// build_shared_properties
// ──────────────────────────────────────────

#[test]
fn shared_properties_without_task_id_are_event_id_only() {
    let mut input = input();
    input.priority = Some("high".to_string());
    input.difficulty = Some("hard".to_string());
    let shared = build_shared_properties(&input, MINTED_ID);
    let value = serde_json::to_value(&shared).unwrap();
    let map = value.as_object().unwrap();
    assert_eq!(map.len(), 1, "hand-created: only sanctuary_event_id: {value}");
    assert_eq!(map["sanctuary_event_id"], MINTED_ID);
    assert_eq!(shared.sanctuary_task_id, None);
    assert_eq!(shared.sanctuary_priority, None);
    assert_eq!(shared.sanctuary_difficulty, None);
}

#[test]
fn shared_properties_with_whitespace_only_task_id_are_event_id_only() {
    for blank in ["", "   ", "\t"] {
        let mut input = input();
        input.task_id = Some(blank.to_string());
        let shared = build_shared_properties(&input, MINTED_ID);
        let value = serde_json::to_value(&shared).unwrap();
        let map = value.as_object().unwrap();
        assert_eq!(
            map.len(),
            1,
            "whitespace-only task id {blank:?} is no carrier: {value}"
        );
        assert_eq!(map["sanctuary_event_id"], MINTED_ID);
        assert_eq!(shared.sanctuary_task_id, None);
    }
}

#[test]
fn shared_properties_task_id_only_serializes_event_id_and_carrier() {
    let mut input = input();
    input.task_id = Some("task-1".to_string());
    let shared = build_shared_properties(&input, MINTED_ID);
    assert_eq!(shared.sanctuary_task_id.as_deref(), Some("task-1"));
    assert_eq!(shared.sanctuary_event_id.as_deref(), Some(MINTED_ID));
    let value = serde_json::to_value(&shared).unwrap();
    let map = value.as_object().unwrap();
    assert_eq!(map.len(), 2, "event id + carrier: {value}");
    assert_eq!(map["sanctuary_task_id"], "task-1");
    assert_eq!(map["sanctuary_event_id"], MINTED_ID);
    assert!(
        !matches!(map.get("sanctuary_focus"), Some(v) if v == "0"),
        "focus never serializes as \"0\""
    );
    assert_eq!(shared.sanctuary_focus, None);
    assert_eq!(shared.sanctuary_priority, None);
    assert_eq!(shared.sanctuary_difficulty, None);
}

#[test]
fn shared_properties_focused_sets_sanctuary_focus_to_one() {
    let mut input = input();
    input.task_id = Some("task-1".to_string());
    input.sanctuary_focus = true;
    let shared = build_shared_properties(&input, MINTED_ID);
    assert_eq!(shared.sanctuary_focus.as_deref(), Some("1"));
}

#[test]
fn shared_properties_unfocused_omits_the_focus_key_on_serialize() {
    let mut input = input();
    input.task_id = Some("task-1".to_string());
    input.sanctuary_focus = false;
    let shared = build_shared_properties(&input, MINTED_ID);
    assert_eq!(shared.sanctuary_focus, None);
    let value = serde_json::to_value(&shared).unwrap();
    assert!(
        value.get("sanctuary_focus").is_none(),
        "unfocused never sends the key (never \"0\"): {value}"
    );
}

#[test]
fn shared_properties_carry_priority_and_difficulty_snapshots() {
    let mut input = input();
    input.task_id = Some("task-1".to_string());
    input.priority = Some("high".to_string());
    input.difficulty = Some("hard".to_string());
    let shared = build_shared_properties(&input, MINTED_ID);
    assert_eq!(shared.sanctuary_priority.as_deref(), Some("high"));
    assert_eq!(shared.sanctuary_difficulty.as_deref(), Some("hard"));
    let value = serde_json::to_value(&shared).unwrap();
    assert_eq!(value["sanctuary_priority"], "high");
    assert_eq!(value["sanctuary_difficulty"], "hard");
}

#[test]
fn shared_properties_blank_snapshots_are_dropped() {
    let mut input = input();
    input.task_id = Some("task-1".to_string());
    input.priority = Some("  ".to_string());
    input.difficulty = Some("\t".to_string());
    let shared = build_shared_properties(&input, MINTED_ID);
    assert_eq!(shared.sanctuary_priority, None);
    assert_eq!(shared.sanctuary_difficulty, None);
    let value = serde_json::to_value(&shared).unwrap();
    assert!(value.get("sanctuary_priority").is_none(), "{value}");
    assert!(value.get("sanctuary_difficulty").is_none(), "{value}");
}

#[test]
fn shared_properties_focus_without_a_task_id_omits_focus_and_snapshots() {
    let mut input = input();
    input.sanctuary_focus = true;
    input.priority = Some("high".to_string());
    let shared = build_shared_properties(&input, MINTED_ID);
    let value = serde_json::to_value(&shared).unwrap();
    let map = value.as_object().unwrap();
    assert_eq!(
        map.len(),
        1,
        "focus and snapshots never travel without the task carrier: {value}"
    );
    assert_eq!(map["sanctuary_event_id"], MINTED_ID);
    assert_eq!(shared.sanctuary_focus, None);
    assert_eq!(shared.sanctuary_priority, None);
}

#[test]
fn create_posts_json_to_google_and_upserts_the_cache() {
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events",
        200,
        CREATED_JSON,
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let output = pollster::block_on(create_event(
        &http, &calendars, &events, &ops, &access(), "u-1", &input(), NOW_UNIX,
    ))
    .unwrap();

    assert_eq!(output.source, "google");
    assert!(output.cache_error.is_none());
    assert_eq!(output.event.id, "evt-1", "persisted id from FakeEventRepo upsert");
    assert!(!output.event.google_event_id.is_empty());
    assert!(
        output.event.google_event_id.starts_with("sanc"),
        "minted client id: {}",
        output.event.google_event_id
    );
    assert_eq!(output.event.calendar_id, "cal-1");
    assert_eq!(output.event.title, "New meeting");
    assert_eq!(output.event.start_time, "2026-08-19T09:00:00Z");
    assert_eq!(output.event.last_synced_at, "2023-11-14T22:13:20Z");

    // POST body carries the calendar contract + client id.
    let (url, body) = http.posts.lock().unwrap().first().unwrap().clone();
    assert!(url.contains("primary%40example.com"), "{url}");
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["summary"], "New meeting");
    assert_eq!(body["description"], "About things");
    assert_eq!(body["start"]["dateTime"], "2026-08-19T09:00:00Z");
    assert_eq!(body["end"]["dateTime"], "2026-08-19T10:00:00Z");
    assert_eq!(body["id"], output.event.google_event_id);
    assert_eq!(
        body["extendedProperties"]["shared"]["sanctuary_event_id"],
        output.event.google_event_id
    );
    assert!(body.get("colorId").is_none(), "hand-created events carry no colorId");
    assert!(
        body.get("eventLabelId").is_none(),
        "hand-created events carry no eventLabelId"
    );
    assert!(
        !url.contains("eventLabelVersion"),
        "hand-created events do not require eventLabelVersion: {url}"
    );
    assert_eq!(http.posts.lock().unwrap().len(), 1);

    // Cache upsert happened with the mapped row (echoed mint id).
    let (google_id, upserted) = events.upserted_single.lock().unwrap().clone().unwrap();
    assert_eq!(google_id, output.event.google_event_id);
    assert_eq!(upserted.calendar_id, "cal-1");

    // Journal: pending → google_committed → cache_applied.
    let stored = ops.stored.lock().unwrap();
    assert_eq!(stored.len(), 1);
    let op = &stored[0];
    assert_eq!(op.verb, OP_VERB_INSERT);
    assert_eq!(op.status, OP_STATUS_CACHE_APPLIED);
    assert_eq!(op.google_event_id, output.event.google_event_id);
    assert_eq!(op.local_event_id, "evt-1");
    assert_eq!(op.user_id, "u-1");
    assert_eq!(op.calendar_id, "cal-1");
    assert!(!op.payload_fingerprint.is_empty());
    assert_eq!(op.google_etag, "e1");
}

#[test]
fn create_with_color_hex_sends_event_label_id() {
    // `#535050` snaps chroma-first to graphite `#616161`; the cache maps
    // graphite to a stable fake id.
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events",
        200,
        CREATED_JSON,
    )]);
    let calendars = FakeCalendarRepo::with(vec![GoogleCalendar {
        event_labels: r##"[{"id":"label-graphite","backgroundColor":"#616161"}]"##.to_string(),
        ..calendar("cal-1", "primary@example.com", true)
    }]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();
    let mut input = input();
    input.color_hex = Some("#535050".to_string());

    pollster::block_on(create_event(
        &http, &calendars, &events, &ops, &access(), "u-1", &input, NOW_UNIX,
    ))
    .unwrap();

    let (url, body) = http.posts.lock().unwrap().first().unwrap().clone();
    assert!(url.contains("eventLabelVersion=1"), "{url}");
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["eventLabelId"], "label-graphite");
    assert!(body.get("id").and_then(|v| v.as_str()).is_some(), "client id in body: {body}");
    assert!(
        body.get("colorId").is_none(),
        "create_event never sends colorId: {body}"
    );
}

#[test]
fn create_with_blank_color_hex_omits_color_keys() {
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();
    for blank in ["", "   ", "\t"] {
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events",
            200,
            CREATED_JSON,
        )]);
        let mut input = input();
        input.color_hex = Some(blank.to_string());

        pollster::block_on(create_event(
            &http, &calendars, &events, &ops, &access(), "u-1", &input, NOW_UNIX,
        ))
        .unwrap();

        let (url, body) = http.posts.lock().unwrap().first().unwrap().clone();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(
            body.get("colorId").is_none() && body.get("eventLabelId").is_none(),
            "blank color_hex {blank:?} must omit both color keys"
        );
        assert!(
            !url.contains("eventLabelVersion"),
            "blank color_hex {blank:?} must not require eventLabelVersion: {url}"
        );
    }
}

#[test]
fn create_with_color_hex_and_empty_label_cache_is_invalid() {
    // Empty string = cache miss. The start must fail with a 400-shaped
    // Invalid — no `calendars.get`, no POST, no journal.
    let calendars = FakeCalendarRepo::with(vec![GoogleCalendar {
        event_labels: String::new(),
        ..calendar("cal-1", "primary@example.com", true)
    }]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();
    let http = FakeHttp::new(vec![]);
    let mut input = input();
    input.color_hex = Some("#4285f4".to_string());

    let err = pollster::block_on(create_event(
        &http, &calendars, &events, &ops, &access(), "u-1", &input, NOW_UNIX,
    ))
    .unwrap_err();
    assert!(
        matches!(&err, CalendarError::Invalid(m) if m == "calendar event-label cache is empty"),
        "got {err:?}"
    );
    assert!(http.posts.lock().unwrap().is_empty(), "no Google POST");
    assert!(ops.stored.lock().unwrap().is_empty(), "no journal row");
}

#[test]
fn create_with_color_hex_and_no_matching_label_is_invalid() {
    // Fetched-but-empty cache (`"[]"` = no labels on the calendar).
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();
    let http = FakeHttp::new(vec![]);
    let mut cobalt_input = input();
    cobalt_input.color_hex = Some("#4285f4".to_string());

    let err = pollster::block_on(create_event(
        &http, &calendars, &events, &ops, &access(), "u-1", &cobalt_input, NOW_UNIX,
    ))
    .unwrap_err();
    assert!(
        matches!(&err, CalendarError::Invalid(m) if m == "no event label matches category color"),
        "got {err:?}"
    );
    assert!(http.posts.lock().unwrap().is_empty(), "no Google POST");

    // A populated cache that lacks the snapped hex — same failure.
    let calendars = FakeCalendarRepo::with(vec![GoogleCalendar {
        event_labels: r##"[{"id":"label-banana","backgroundColor":"#f6bf26"}]"##.to_string(),
        ..calendar("cal-1", "primary@example.com", true)
    }]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();
    let http = FakeHttp::new(vec![]);
    let mut banana_input = input();
    banana_input.color_hex = Some("#4285f4".to_string());

    let err = pollster::block_on(create_event(
        &http, &calendars, &events, &ops, &access(), "u-1", &banana_input, NOW_UNIX,
    ))
    .unwrap_err();
    assert!(
        matches!(&err, CalendarError::Invalid(m) if m == "no event label matches category color"),
        "got {err:?}"
    );
    assert!(http.posts.lock().unwrap().is_empty(), "no Google POST");
}

#[test]
fn create_missing_calendar_is_not_found() {
    let http = FakeHttp::new(vec![]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-other", "other", true)]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let err = pollster::block_on(create_event(
        &http, &calendars, &events, &ops, &access(), "u-1", &input(), NOW_UNIX,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::NotFound), "got {err:?}");
    assert!(http.posts.lock().unwrap().is_empty(), "no Google call");
    assert!(ops.stored.lock().unwrap().is_empty(), "no journal row");
}

#[test]
fn create_wrong_owner_is_not_found() {
    let http = FakeHttp::new(vec![]);
    let calendars =
        FakeCalendarRepo::with(vec![calendar_for_user("other-user", "cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let err = pollster::block_on(create_event(
        &http, &calendars, &events, &ops, &access(), "u-1", &input(), NOW_UNIX,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::NotFound), "got {err:?}");
    assert!(http.posts.lock().unwrap().is_empty(), "no Google insert");
    assert!(ops.stored.lock().unwrap().is_empty(), "no journal row");
    assert!(events.upserted_single.lock().unwrap().is_none());
}

#[test]
fn create_soft_deleted_calendar_is_not_found() {
    let http = FakeHttp::new(vec![]);
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.deleted_at = Some("2023-11-14T20:00:00Z".to_string());
    let calendars = FakeCalendarRepo::with(vec![cal]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let err = pollster::block_on(create_event(
        &http, &calendars, &events, &ops, &access(), "u-1", &input(), NOW_UNIX,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::NotFound), "got {err:?}");
    assert!(http.posts.lock().unwrap().is_empty(), "no Google insert");
    assert!(ops.stored.lock().unwrap().is_empty(), "no journal row");
    assert!(events.upserted_single.lock().unwrap().is_none());
}

#[test]
fn create_google_non_2xx_is_an_api_error() {
    let http = FakeHttp::new(vec![("/events", 400, r#"{"error":"invalid"}"#)]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let err = pollster::block_on(create_event(
        &http, &calendars, &events, &ops, &access(), "u-1", &input(), NOW_UNIX,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::GoogleApi(_)), "got {err:?}");
    assert!(events.upserted_single.lock().unwrap().is_none(), "no cache write on failure");
    let stored = ops.stored.lock().unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].status, OP_STATUS_FAILED);
    assert!(stored[0].last_error.contains("400"), "{}", stored[0].last_error);
}

#[test]
fn create_cache_failure_after_google_is_repo_error() {
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events",
        200,
        CREATED_JSON,
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();
    *events.fail_upsert.lock().unwrap() = true;

    let err = pollster::block_on(create_event(
        &http, &calendars, &events, &ops, &access(), "u-1", &input(), NOW_UNIX,
    ))
    .unwrap_err();

    assert!(matches!(err, CalendarError::Repo(_)), "got {err:?}");
    assert_eq!(http.posts.lock().unwrap().len(), 1, "exactly one POST");
    let stored = ops.stored.lock().unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].status, OP_STATUS_GOOGLE_COMMITTED,
        "cache fail leaves journal google_committed"
    );
    assert!(stored[0].local_event_id.is_empty());
    assert!(!stored[0].google_event_id.is_empty());
}

#[test]
fn create_409_duplicate_gets_existing_and_applies_cache() {
    // GET route first — FakeHttp matches first substring; POST path
    // `.../events` would otherwise steal a GET to `.../events/sanc...`.
    let http = FakeHttp::new(vec![
        (
            "/events/sanc",
            200,
            r#"{
                "id": "placeholder", "etag": "etag-from-get",
                "updated": "2026-08-17T12:00:00.000Z",
                "summary": "New meeting", "description": "About things",
                "start": {"dateTime": "2026-08-19T09:00:00Z"},
                "end": {"dateTime": "2026-08-19T10:00:00Z"}
            }"#,
        ),
        (
            "/calendars/primary%40example.com/events",
            409,
            r#"{"error":{"code":409,"message":"already exists"}}"#,
        ),
    ]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let output = pollster::block_on(create_event(
        &http, &calendars, &events, &ops, &access(), "u-1", &input(), NOW_UNIX,
    ))
    .unwrap();

    assert_eq!(output.event.id, "evt-1");
    assert_eq!(http.posts.lock().unwrap().len(), 1, "no second POST");
    let gets = http.gets.lock().unwrap().clone();
    assert_eq!(gets.len(), 1);
    assert!(
        gets[0].contains(&output.event.google_event_id),
        "GET uses minted id: {} vs {}",
        gets[0],
        output.event.google_event_id
    );

    let stored = ops.stored.lock().unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].status, OP_STATUS_CACHE_APPLIED);
    assert_eq!(stored[0].google_etag, "etag-from-get");
    assert_eq!(stored[0].local_event_id, "evt-1");
    assert_eq!(stored[0].google_event_id, output.event.google_event_id);

    let rows = events.stored.lock().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].google_event_id, output.event.google_event_id);
}

#[test]
fn create_journal_insert_failure_skips_google() {
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events",
        200,
        CREATED_JSON,
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();
    *ops.fail_insert.lock().unwrap() = true;

    let err = pollster::block_on(create_event(
        &http, &calendars, &events, &ops, &access(), "u-1", &input(), NOW_UNIX,
    ))
    .unwrap_err();

    assert!(matches!(err, CalendarError::Repo(_)), "got {err:?}");
    assert!(http.posts.lock().unwrap().is_empty(), "no Google POST");
    assert!(ops.stored.lock().unwrap().is_empty());
    assert!(events.upserted_single.lock().unwrap().is_none());
}

#[test]
fn create_with_task_id_sends_extended_properties_and_maps_it_back() {
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events",
        200,
        &created_with_task_json("task-1"),
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let mut input = input();
    input.task_id = Some("task-1".to_string());
    let output = pollster::block_on(create_event(
        &http, &calendars, &events, &ops, &access(), "u-1", &input, NOW_UNIX,
    ))
    .unwrap();

    // The insert body carries the shared carrier — never `private`, never
    // a description footer.
    let (url, body) = http.posts.lock().unwrap().first().unwrap().clone();
    assert!(url.contains("primary%40example.com"), "{url}");
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        body["extendedProperties"]["shared"]["sanctuary_task_id"],
        "task-1"
    );
    assert_eq!(
        body["extendedProperties"]["shared"]["sanctuary_event_id"],
        output.event.google_event_id
    );
    let shared = body["extendedProperties"]["shared"].as_object().unwrap();
    for key in ["sanctuary_focus", "sanctuary_priority", "sanctuary_difficulty"] {
        assert!(!shared.contains_key(key), "{key} absent without a value: {body}");
    }
    assert!(body.get("private").is_none(), "no private properties");

    // The echoed event maps the property onto the cached row.
    assert_eq!(output.event.task_id, "task-1");
    let upserted = events.upserted_single.lock().unwrap().clone().unwrap().1;
    assert_eq!(upserted.task_id, "task-1");
}

#[test]
fn create_without_task_id_sends_only_sanctuary_event_id() {
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events",
        200,
        CREATED_JSON,
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let output = pollster::block_on(create_event(
        &http, &calendars, &events, &ops, &access(), "u-1", &input(), NOW_UNIX,
    ))
    .unwrap();

    let (_, body) = http.posts.lock().unwrap().first().unwrap().clone();
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    let shared = body["extendedProperties"]["shared"].as_object().unwrap();
    assert_eq!(shared.len(), 1, "hand-created: only sanctuary_event_id: {body}");
    assert_eq!(shared["sanctuary_event_id"], output.event.google_event_id);
    for key in [
        "sanctuary_task_id",
        "sanctuary_focus",
        "sanctuary_priority",
        "sanctuary_difficulty",
        "sanctuary_routine_id",
        "sanctuary_occurrence_id",
    ] {
        assert!(!shared.contains_key(key), "{key} must be absent: {body}");
    }
    assert_eq!(output.event.task_id, "", "no task carrier → no task link");
}

#[test]
fn create_with_task_and_focus_sends_both_shared_keys() {
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events",
        200,
        &created_with_focus_json("task-1"),
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let mut input = input();
    input.task_id = Some("task-1".to_string());
    input.sanctuary_focus = true;
    let output = pollster::block_on(create_event(
        &http, &calendars, &events, &ops, &access(), "u-1", &input, NOW_UNIX,
    ))
    .unwrap();

    // Task + focus + event id travel together — never a partial shared map,
    // never `sanctuary_focus` inside a `private` map.
    let (_, body) = http.posts.lock().unwrap().first().unwrap().clone();
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        body["extendedProperties"]["shared"]["sanctuary_task_id"],
        "task-1"
    );
    assert_eq!(body["extendedProperties"]["shared"]["sanctuary_focus"], "1");
    assert_eq!(
        body["extendedProperties"]["shared"]["sanctuary_event_id"],
        output.event.google_event_id
    );
    // No snapshots were set on the input, so P/D stay absent.
    let shared = body["extendedProperties"]["shared"].as_object().unwrap();
    for key in ["sanctuary_priority", "sanctuary_difficulty"] {
        assert!(!shared.contains_key(key), "{key} absent unless set: {body}");
    }
    assert!(body.get("private").is_none(), "no private properties");

    // The echoed event maps the task carrier onto the cached row.
    assert_eq!(output.event.task_id, "task-1");
    let upserted = events.upserted_single.lock().unwrap().clone().unwrap().1;
    assert_eq!(upserted.task_id, "task-1");
}

#[test]
fn create_with_task_but_no_focus_omits_the_focus_key() {
    // Unfocused creates (e.g. `start_task`) send the carrier + event id —
    // the `sanctuary_focus` key must not appear, and never as `"0"`.
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events",
        200,
        &created_with_task_json("task-1"),
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let mut input = input();
    input.task_id = Some("task-1".to_string());
    input.sanctuary_focus = false;
    pollster::block_on(create_event(
        &http, &calendars, &events, &ops, &access(), "u-1", &input, NOW_UNIX,
    ))
    .unwrap();

    let (_, body) = http.posts.lock().unwrap().first().unwrap().clone();
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        body["extendedProperties"]["shared"]["sanctuary_task_id"],
        "task-1"
    );
    assert!(
        body["extendedProperties"]["shared"].get("sanctuary_focus").is_none(),
        "unfocused creates never send sanctuary_focus: {body}"
    );
}

#[test]
fn create_with_task_priority_and_difficulty_snapshots_both_keys() {
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events",
        200,
        &created_with_task_json("task-1"),
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let mut input = input();
    input.task_id = Some("task-1".to_string());
    input.priority = Some("high".to_string());
    input.difficulty = Some("hard".to_string());
    input.sanctuary_focus = false;
    let output = pollster::block_on(create_event(
        &http, &calendars, &events, &ops, &access(), "u-1", &input, NOW_UNIX,
    ))
    .unwrap();

    // The snapshots ride along with the carrier; unfocused sends no
    // `sanctuary_focus`.
    let (_, body) = http.posts.lock().unwrap().first().unwrap().clone();
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        body["extendedProperties"]["shared"]["sanctuary_task_id"],
        "task-1"
    );
    assert_eq!(
        body["extendedProperties"]["shared"]["sanctuary_priority"],
        "high"
    );
    assert_eq!(
        body["extendedProperties"]["shared"]["sanctuary_difficulty"],
        "hard"
    );
    assert!(
        body["extendedProperties"]["shared"]
            .get("sanctuary_focus")
            .is_none(),
        "snapshots never imply focus: {body}"
    );

    // The echo maps the carrier onto the cached row (snapshots are not
    // cached — no D1 columns for them).
    assert_eq!(output.event.task_id, "task-1");
}

/// Seed a living cache row so patch/delete send If-Match without a GET.
fn seed_living(events: &FakeEventRepo, id: &str, cal: &str, google_id: &str) {
    events
        .stored
        .lock()
        .unwrap()
        .push(living_event(id, cal, google_id));
}

#[test]
fn patch_posts_end_and_upserts_the_echoed_event() {
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events/google-evt-created",
        200,
        PATCHED_JSON,
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "google-evt-created");
    let ops = FakeOperationRepo::new();

    let output = pollster::block_on(patch_event(
        &http, &calendars, &events, &ops, &access(), "cal-1", "google-evt-created",
        "2026-08-19T11:00:00Z", NOW_UNIX,
    ))
    .unwrap();

    assert_eq!(output.event.google_event_id, "google-evt-created");
    assert_eq!(output.event.end_time, "2026-08-19T11:00:00Z");
    assert_eq!(output.event.calendar_id, "cal-1");

    // PATCH body: `end.dateTime` only + If-Match from stored etag.
    let patches = http.patches.lock().unwrap();
    assert_eq!(patches.len(), 1);
    let (url, body) = patches.first().unwrap().clone();
    assert!(url.contains("primary%40example.com/events/google-evt-created"), "{url}");
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["end"]["dateTime"], "2026-08-19T11:00:00Z");
    let headers = http.patch_headers.lock().unwrap();
    assert_eq!(headers.len(), 1);
    assert!(
        headers[0].iter().any(|(k, v)| k == "If-Match" && v == "e1"),
        "If-Match e1: {:?}",
        headers[0]
    );

    // The echoed (patched) event replaced the cached row.
    let (google_id, upserted) = events.upserted_single.lock().unwrap().clone().unwrap();
    assert_eq!(google_id, "google-evt-created");
    assert_eq!(upserted.end_time, "2026-08-19T11:00:00Z");
    assert_eq!(upserted.google_etag, "e2");

    let stored = ops.stored.lock().unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].verb, OP_VERB_PATCH);
    assert_eq!(stored[0].status, OP_STATUS_CACHE_APPLIED);
    assert_eq!(stored[0].google_etag, "e2");
}

#[test]
fn patch_preserves_task_link_when_google_echoes_it() {
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events/google-evt-created",
        200,
        &created_with_task_json("task-1"),
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "google-evt-created");
    let ops = FakeOperationRepo::new();

    let output = pollster::block_on(patch_event(
        &http, &calendars, &events, &ops, &access(), "cal-1", "google-evt-created",
        "2026-08-19T11:00:00Z", NOW_UNIX,
    ))
    .unwrap();

    assert_eq!(output.event.task_id, "task-1");
    let (_, upserted) = events.upserted_single.lock().unwrap().clone().unwrap();
    assert_eq!(upserted.task_id, "task-1");
}

#[test]
fn patch_missing_calendar_is_not_found() {
    let http = FakeHttp::new(vec![]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-other", "other", true)]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let err = pollster::block_on(patch_event(
        &http, &calendars, &events, &ops, &access(), "cal-1", "g-1",
        "2026-08-19T11:00:00Z", NOW_UNIX,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::NotFound), "got {err:?}");
    assert!(http.patches.lock().unwrap().is_empty(), "no Google call");
    assert!(ops.stored.lock().unwrap().is_empty(), "no journal");
}

#[test]
fn patch_google_non_2xx_is_an_api_error() {
    let http = FakeHttp::new(vec![("/events/google-evt-created", 400, r#"{"error":"invalid"}"#)]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "google-evt-created");
    let ops = FakeOperationRepo::new();

    let err = pollster::block_on(patch_event(
        &http, &calendars, &events, &ops, &access(), "cal-1", "google-evt-created",
        "2026-08-19T11:00:00Z", NOW_UNIX,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::GoogleApi(_)), "got {err:?}");
    assert!(events.upserted_single.lock().unwrap().is_none(), "no cache write on failure");
    assert_eq!(ops.stored.lock().unwrap()[0].status, OP_STATUS_FAILED);
}

#[test]
fn patch_cache_failure_is_repo_after_google_commit() {
    let http = FakeHttp::new(vec![(
        "/events/google-evt-created",
        200,
        PATCHED_JSON,
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "google-evt-created");
    let ops = FakeOperationRepo::new();
    *events.fail_upsert.lock().unwrap() = true;

    let err = pollster::block_on(patch_event(
        &http, &calendars, &events, &ops, &access(), "cal-1", "google-evt-created",
        "2026-08-19T11:00:00Z", NOW_UNIX,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::Repo(_)), "got {err:?}");
    assert_eq!(ops.stored.lock().unwrap()[0].status, OP_STATUS_GOOGLE_COMMITTED);
}


// ──────────────────────────────────────────
// patch_event_fields
// ──────────────────────────────────────────

#[test]
fn patch_fields_start_only_payload() {
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events/google-evt-created",
        200,
        PATCHED_JSON,
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "google-evt-created");
    let ops = FakeOperationRepo::new();

    pollster::block_on(patch_event_fields(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "cal-1",
        "google-evt-created",
        &PatchEventFields {
            start: Some("2026-08-19T09:00:00Z".to_string()),
            end: None,
            summary: None,
            description: None,
            is_all_day: None,
            start_time_zone: None,
            calendar_id: None,
        },
        NOW_UNIX,
    ))
    .unwrap();

    let patches = http.patches.lock().unwrap();
    assert_eq!(patches.len(), 1);
    let body: serde_json::Value = serde_json::from_str(&patches[0].1).unwrap();
    assert_eq!(body["start"]["dateTime"], "2026-08-19T09:00:00Z");
    assert!(body.get("end").is_none(), "{body}");
    assert!(body.get("summary").is_none(), "{body}");
    assert!(body.get("description").is_none(), "{body}");
    assert!(body.get("calendar_id").is_none(), "{body}");
}

#[test]
fn patch_fields_start_end_summary_payload() {
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events/google-evt-created",
        200,
        PATCHED_JSON,
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "google-evt-created");
    let ops = FakeOperationRepo::new();

    pollster::block_on(patch_event_fields(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "cal-1",
        "google-evt-created",
        &PatchEventFields {
            start: Some("2026-08-19T09:00:00Z".to_string()),
            end: Some("2026-08-19T11:00:00Z".to_string()),
            summary: Some("Renamed".to_string()),
            description: None,
            is_all_day: None,
            start_time_zone: None,
            calendar_id: None,
        },
        NOW_UNIX,
    ))
    .unwrap();

    let patches = http.patches.lock().unwrap();
    assert_eq!(patches.len(), 1);
    let body: serde_json::Value = serde_json::from_str(&patches[0].1).unwrap();
    assert_eq!(body["start"]["dateTime"], "2026-08-19T09:00:00Z");
    assert_eq!(body["end"]["dateTime"], "2026-08-19T11:00:00Z");
    assert_eq!(body["summary"], "Renamed");
    assert!(body.get("description").is_none(), "{body}");
    assert!(body.get("calendar_id").is_none(), "{body}");
}

#[test]
fn patch_fields_description_only_payload() {
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events/google-evt-created",
        200,
        PATCHED_JSON,
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "google-evt-created");
    let ops = FakeOperationRepo::new();

    pollster::block_on(patch_event_fields(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "cal-1",
        "google-evt-created",
        &PatchEventFields {
            start: None,
            end: None,
            summary: None,
            description: Some("Bring snacks".to_string()),
            is_all_day: None,
            start_time_zone: None,
            calendar_id: None,
        },
        NOW_UNIX,
    ))
    .unwrap();

    let patches = http.patches.lock().unwrap();
    assert_eq!(patches.len(), 1);
    let body: serde_json::Value = serde_json::from_str(&patches[0].1).unwrap();
    assert_eq!(body["description"], "Bring snacks");
    assert!(body.get("start").is_none(), "{body}");
    assert!(body.get("end").is_none(), "{body}");
    assert!(body.get("summary").is_none(), "{body}");
    assert!(body.get("calendar_id").is_none(), "{body}");
}

#[test]
fn patch_fields_description_empty_string_clears() {
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events/google-evt-created",
        200,
        PATCHED_JSON,
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "google-evt-created");
    let ops = FakeOperationRepo::new();

    pollster::block_on(patch_event_fields(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "cal-1",
        "google-evt-created",
        &PatchEventFields {
            start: None,
            end: None,
            summary: None,
            description: Some(String::new()),
            is_all_day: None,
            start_time_zone: None,
            calendar_id: None,
        },
        NOW_UNIX,
    ))
    .unwrap();

    let patches = http.patches.lock().unwrap();
    assert_eq!(patches.len(), 1);
    let body: serde_json::Value = serde_json::from_str(&patches[0].1).unwrap();
    assert_eq!(body["description"], "");
    assert!(body.get("start").is_none(), "{body}");
    assert!(body.get("end").is_none(), "{body}");
    assert!(body.get("summary").is_none(), "{body}");
    assert!(body.get("calendar_id").is_none(), "{body}");
}

#[test]
fn patch_fields_all_none_is_invalid() {
    let http = FakeHttp::new(vec![]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let err = pollster::block_on(patch_event_fields(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "cal-1",
        "google-evt-created",
        &PatchEventFields::default(),
        NOW_UNIX,
    ))
    .unwrap_err();
    assert!(
        matches!(err, CalendarError::Invalid(ref m) if m.contains("empty patch")),
        "got {err:?}"
    );
    assert!(http.patches.lock().unwrap().is_empty(), "no Google call");
    assert!(ops.stored.lock().unwrap().is_empty(), "no journal on empty patch");
}

#[test]
fn patch_sends_if_match_and_stores_new_etag() {
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events/google-evt-created",
        200,
        PATCHED_JSON,
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "google-evt-created");
    let ops = FakeOperationRepo::new();

    let output = pollster::block_on(patch_event(
        &http, &calendars, &events, &ops, &access(), "cal-1", "google-evt-created",
        "2026-08-19T11:00:00Z", NOW_UNIX,
    ))
    .unwrap();

    assert_eq!(output.event.google_etag, "e2");
    let headers = &http.patch_headers.lock().unwrap()[0];
    assert!(headers.iter().any(|(k, v)| k == "If-Match" && v == "e1"), "{headers:?}");
    assert_eq!(ops.stored.lock().unwrap()[0].status, OP_STATUS_CACHE_APPLIED);
    assert_eq!(ops.stored.lock().unwrap()[0].google_etag, "e2");
    assert!(http.gets.lock().unwrap().is_empty(), "no GET when etag known");
}

#[test]
fn patch_412_then_success_retries_with_fresh_etag() {
    let get_body = r#"{
        "id": "google-evt-created", "etag": "e2",
        "summary": "New meeting",
        "start": {"dateTime": "2026-08-19T09:00:00Z"},
        "end": {"dateTime": "2026-08-19T10:00:00Z"}
    }"#;
    let http = FakeHttp::new(vec![]).with_one_shots(vec![
        ("/events/google-evt-created", 412, r#"{"error":{"code":412}}"#),
        ("/events/google-evt-created", 200, get_body),
        ("/events/google-evt-created", 200, PATCHED_JSON),
    ]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "google-evt-created");
    let ops = FakeOperationRepo::new();

    let output = pollster::block_on(patch_event(
        &http, &calendars, &events, &ops, &access(), "cal-1", "google-evt-created",
        "2026-08-19T11:00:00Z", NOW_UNIX,
    ))
    .unwrap();

    assert_eq!(output.event.end_time, "2026-08-19T11:00:00Z");
    assert_eq!(http.patches.lock().unwrap().len(), 2);
    assert_eq!(http.gets.lock().unwrap().len(), 1);
    let headers = http.patch_headers.lock().unwrap();
    assert!(
        headers[0].iter().any(|(k, v)| k == "If-Match" && v == "e1"),
        "first If-Match e1: {:?}",
        headers[0]
    );
    assert!(
        headers[1].iter().any(|(k, v)| k == "If-Match" && v == "e2"),
        "second If-Match e2: {:?}",
        headers[1]
    );
    // Same minimal payload both times.
    let patches = http.patches.lock().unwrap();
    assert_eq!(patches[0].1, patches[1].1);
    assert_eq!(ops.stored.lock().unwrap()[0].status, OP_STATUS_CACHE_APPLIED);
    assert!(ops.stored.lock().unwrap()[0].attempt_count >= 1);
}

#[test]
fn patch_three_412s_is_conflict_without_disabling_calendar() {
    let get_body = r#"{"id":"google-evt-created","etag":"eX"}"#;
    let http = FakeHttp::new(vec![]).with_one_shots(vec![
        ("/events/google-evt-created", 412, r#"{"error":{"code":412}}"#),
        ("/events/google-evt-created", 200, get_body),
        ("/events/google-evt-created", 412, r#"{"error":{"code":412}}"#),
        ("/events/google-evt-created", 200, get_body),
        ("/events/google-evt-created", 412, r#"{"error":{"code":412}}"#),
    ]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "google-evt-created");
    let ops = FakeOperationRepo::new();

    let err = pollster::block_on(patch_event(
        &http, &calendars, &events, &ops, &access(), "cal-1", "google-evt-created",
        "2026-08-19T11:00:00Z", NOW_UNIX,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::Conflict), "got {err:?}");
    assert_eq!(http.patches.lock().unwrap().len(), 3, "exactly 3 PATCH attempts");
    let op = &ops.stored.lock().unwrap()[0];
    assert_eq!(op.status, OP_STATUS_CONFLICT);
    assert!(op.attempt_count >= 3, "attempt_count={}", op.attempt_count);
    // Calendar still listed / sync not disabled.
    let cal = calendars.stored.lock().unwrap();
    assert!(cal[0].sync_enabled, "calendar must stay sync_enabled");
    assert!(calendars.disabled.lock().unwrap().is_empty());
}

#[test]
fn patch_403_forbidden_for_non_organizer_does_not_retry() {
    let body = r#"{"error":{"errors":[{"reason":"forbiddenForNonOrganizer"}]}}"#;
    let http = FakeHttp::new(vec![("/events/google-evt-created", 403, body)]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "google-evt-created");
    let ops = FakeOperationRepo::new();

    let err = pollster::block_on(patch_event(
        &http, &calendars, &events, &ops, &access(), "cal-1", "google-evt-created",
        "2026-08-19T11:00:00Z", NOW_UNIX,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::GoogleApi(_)), "got {err:?}");
    assert_eq!(http.patches.lock().unwrap().len(), 1, "exactly one PATCH");
    assert_eq!(ops.stored.lock().unwrap()[0].status, OP_STATUS_FAILED);
}

#[test]
fn delete_event_sends_cancelled_and_soft_deletes_cache() {
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events/g-evt-1",
        200,
        r#"{"id":"g-evt-1","status":"cancelled","etag":"e-del"}"#,
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "g-evt-1");
    let ops = FakeOperationRepo::new();

    pollster::block_on(delete_event(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "cal-1",
        "g-evt-1",
        "local-1",
        NOW_UNIX,
    ))
    .unwrap();

    let patches = http.patches.lock().unwrap();
    assert_eq!(patches.len(), 1);
    let body: serde_json::Value = serde_json::from_str(&patches[0].1).unwrap();
    assert_eq!(body["status"], "cancelled");
    assert!(body.get("end").is_none());
    let headers = &http.patch_headers.lock().unwrap()[0];
    assert!(
        headers.iter().any(|(k, v)| k == "If-Match" && v == "e1"),
        "If-Match on delete: {headers:?}"
    );

    let deleted = events.deleted.lock().unwrap();
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].0, "local-1");
    let op = &ops.stored.lock().unwrap()[0];
    assert_eq!(op.verb, OP_VERB_DELETE);
    assert_eq!(op.status, OP_STATUS_CACHE_APPLIED);
}

#[test]
fn delete_event_google_404_still_soft_deletes_locally() {
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events/g-evt-1",
        404,
        r#"{"error":"notFound"}"#,
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "g-evt-1");
    let ops = FakeOperationRepo::new();

    pollster::block_on(delete_event(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "cal-1",
        "g-evt-1",
        "local-1",
        NOW_UNIX,
    ))
    .unwrap();

    assert_eq!(events.deleted.lock().unwrap().len(), 1);
    assert_eq!(ops.stored.lock().unwrap()[0].status, OP_STATUS_CACHE_APPLIED);
}

#[test]
fn delete_event_empty_etag_get_404_still_soft_deletes_locally() {
    // Pre-V1 / empty-etag row: bootstrap GET 404 must still local-delete
    // (same contract as PATCH 404/410), without a cancel PATCH.
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events/g-evt-1",
        404,
        r#"{"error":"notFound"}"#,
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let mut row = living_event("local-1", "cal-1", "g-evt-1");
    row.google_etag = String::new();
    events.stored.lock().unwrap().push(row);
    let ops = FakeOperationRepo::new();

    pollster::block_on(delete_event(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "cal-1",
        "g-evt-1",
        "local-1",
        NOW_UNIX,
    ))
    .unwrap();

    let deleted = events.deleted.lock().unwrap();
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].0, "local-1");
    assert_eq!(ops.stored.lock().unwrap()[0].status, OP_STATUS_CACHE_APPLIED);
    assert!(
        http.patches.lock().unwrap().is_empty(),
        "no cancel PATCH after GET 404"
    );
    assert!(
        !http.gets.lock().unwrap().is_empty(),
        "bootstrap GET must run when etag empty"
    );
}

#[test]
fn update_event_for_user_wrong_owner_is_not_found() {
    let http = FakeHttp::new(vec![]);
    let calendars =
        FakeCalendarRepo::with(vec![calendar_for_user("other-user", "cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "g-evt-1");
    let ops = FakeOperationRepo::new();

    let err = pollster::block_on(update_event_for_user(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "u-1",
        "local-1",
        &PatchEventFields {
            start: None,
            end: None,
            summary: Some("Nope".to_string()),
            description: None,
            is_all_day: None,
            start_time_zone: None,
            calendar_id: None,
        },
        NOW_UNIX,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::NotFound), "got {err:?}");
    assert!(http.patches.lock().unwrap().is_empty(), "no Google call");
    assert!(ops.stored.lock().unwrap().is_empty(), "no journal");
}

#[test]
fn delete_event_for_user_wrong_owner_is_not_found() {
    let http = FakeHttp::new(vec![]);
    let calendars =
        FakeCalendarRepo::with(vec![calendar_for_user("other-user", "cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "g-evt-1");
    let ops = FakeOperationRepo::new();

    let err = pollster::block_on(delete_event_for_user(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "u-1",
        "local-1",
        NOW_UNIX,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::NotFound), "got {err:?}");
    assert!(http.patches.lock().unwrap().is_empty(), "no Google call");
    assert!(events.deleted.lock().unwrap().is_empty());
    assert!(ops.stored.lock().unwrap().is_empty(), "no journal");
}

#[test]
fn update_event_for_user_patches_owned_event() {
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events/g-evt-1",
        200,
        PATCHED_JSON,
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "g-evt-1");
    let ops = FakeOperationRepo::new();

    let output = pollster::block_on(update_event_for_user(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "u-1",
        "local-1",
        &PatchEventFields {
            start: None,
            end: None,
            summary: Some("Renamed".to_string()),
            description: None,
            is_all_day: None,
            start_time_zone: None,
            calendar_id: None,
        },
        NOW_UNIX,
    ))
    .unwrap();

    assert_eq!(output.event.google_event_id, "google-evt-created");
    let body: serde_json::Value =
        serde_json::from_str(&http.patches.lock().unwrap()[0].1).unwrap();
    assert_eq!(body["summary"], "Renamed");
    assert!(
        http.patch_headers.lock().unwrap()[0]
            .iter()
            .any(|(k, v)| k == "If-Match" && v == "e1")
    );
}

// ──────────────────────────────────────────
// update_event_for_user — move
// ──────────────────────────────────────────

const MOVED_JSON: &str = r#"{
    "id": "g-evt-1", "etag": "e-moved", "updated": "2026-08-17T12:30:00.000Z",
    "summary": "Meeting",
    "start": {"dateTime": "2026-08-19T09:00:00Z"},
    "end": {"dateTime": "2026-08-19T10:00:00Z"}
}"#;

fn two_calendars() -> FakeCalendarRepo {
    FakeCalendarRepo::with(vec![
        calendar("cal-1", "primary@example.com", true),
        calendar("cal-2", "work@example.com", true),
    ])
}

#[test]
fn update_event_for_user_moves_to_destination_calendar() {
    let http = FakeHttp::new(vec![("/move", 200, MOVED_JSON)]);
    let calendars = two_calendars();
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "g-evt-1");
    let ops = FakeOperationRepo::new();

    let output = pollster::block_on(update_event_for_user(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "u-1",
        "local-1",
        &PatchEventFields {
            start: None,
            end: None,
            summary: None,
            description: None,
            is_all_day: None,
            start_time_zone: None,
            calendar_id: Some("cal-2".to_string()),
        },
        NOW_UNIX,
    ))
    .unwrap();

    // Same local id, new calendar.
    assert_eq!(output.event.id, "local-1");
    assert_eq!(output.event.calendar_id, "cal-2");
    assert_eq!(output.event.google_event_id, "g-evt-1");

    // POST move, no PATCH.
    let posts = http.posts.lock().unwrap();
    assert_eq!(posts.len(), 1, "{posts:?}");
    let (url, body) = &posts[0];
    assert!(
        url.contains("/calendars/primary%40example.com/events/g-evt-1/move"),
        "{url}"
    );
    assert!(
        url.contains("destination=work%40example.com"),
        "{url}"
    );
    assert_eq!(body, "{}");
    assert!(http.patches.lock().unwrap().is_empty(), "no events.patch");

    // Replica reassigned in place.
    let stored = events.stored.lock().unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id, "local-1");
    assert_eq!(stored[0].calendar_id, "cal-2");
    assert!(stored[0].deleted_at.is_none());

    let op = &ops.stored.lock().unwrap()[0];
    assert_eq!(op.verb, OP_VERB_MOVE);
    assert_eq!(op.status, OP_STATUS_CACHE_APPLIED);
    assert_eq!(op.calendar_id, "cal-1", "journal keeps source calendar");
    let payload: serde_json::Value = serde_json::from_str(&op.payload_json).unwrap();
    assert_eq!(payload["destination"], "work@example.com");

    // Both source and dest dirty-bumped.
    let cals = calendars.stored.lock().unwrap();
    let src = cals.iter().find(|c| c.id == "cal-1").unwrap();
    let dest = cals.iter().find(|c| c.id == "cal-2").unwrap();
    assert_eq!(src.dirty_requested_generation, 1);
    assert_eq!(dest.dirty_requested_generation, 1);
}

#[test]
fn update_event_for_user_calendar_id_with_summary_is_invalid() {
    let http = FakeHttp::new(vec![]);
    let calendars = two_calendars();
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "g-evt-1");
    let ops = FakeOperationRepo::new();

    let err = pollster::block_on(update_event_for_user(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "u-1",
        "local-1",
        &PatchEventFields {
            start: None,
            end: None,
            summary: Some("Nope".to_string()),
            description: None,
            is_all_day: None,
            start_time_zone: None,
            calendar_id: Some("cal-2".to_string()),
        },
        NOW_UNIX,
    ))
    .unwrap_err();
    assert!(
        matches!(err, CalendarError::Invalid(ref m) if m.contains("cannot be combined")),
        "got {err:?}"
    );
    assert!(http.posts.lock().unwrap().is_empty());
    assert!(http.patches.lock().unwrap().is_empty());
    assert!(ops.stored.lock().unwrap().is_empty());
}

#[test]
fn update_event_for_user_same_calendar_is_noop() {
    let http = FakeHttp::new(vec![]);
    let calendars = two_calendars();
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "g-evt-1");
    let ops = FakeOperationRepo::new();

    let output = pollster::block_on(update_event_for_user(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "u-1",
        "local-1",
        &PatchEventFields {
            start: None,
            end: None,
            summary: None,
            description: None,
            is_all_day: None,
            start_time_zone: None,
            calendar_id: Some("cal-1".to_string()),
        },
        NOW_UNIX,
    ))
    .unwrap();

    assert_eq!(output.event.id, "local-1");
    assert_eq!(output.event.calendar_id, "cal-1");
    assert!(http.posts.lock().unwrap().is_empty());
    assert!(http.patches.lock().unwrap().is_empty());
    assert!(ops.stored.lock().unwrap().is_empty());
}

#[test]
fn update_event_for_user_dest_missing_is_not_found() {
    let http = FakeHttp::new(vec![]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "g-evt-1");
    let ops = FakeOperationRepo::new();

    let err = pollster::block_on(update_event_for_user(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "u-1",
        "local-1",
        &PatchEventFields {
            start: None,
            end: None,
            summary: None,
            description: None,
            is_all_day: None,
            start_time_zone: None,
            calendar_id: Some("cal-missing".to_string()),
        },
        NOW_UNIX,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::NotFound), "got {err:?}");
    assert!(http.posts.lock().unwrap().is_empty());
    assert!(ops.stored.lock().unwrap().is_empty());
}

#[test]
fn update_event_for_user_dest_other_user_is_not_found() {
    let http = FakeHttp::new(vec![]);
    let mut other = calendar("cal-2", "work@example.com", true);
    other.user_id = "u-other".to_string();
    let calendars = FakeCalendarRepo::with(vec![
        calendar("cal-1", "primary@example.com", true),
        other,
    ]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "g-evt-1");
    let ops = FakeOperationRepo::new();

    let err = pollster::block_on(update_event_for_user(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "u-1",
        "local-1",
        &PatchEventFields {
            start: None,
            end: None,
            summary: None,
            description: None,
            is_all_day: None,
            start_time_zone: None,
            calendar_id: Some("cal-2".to_string()),
        },
        NOW_UNIX,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::NotFound), "got {err:?}");
    assert!(http.posts.lock().unwrap().is_empty());
    assert!(ops.stored.lock().unwrap().is_empty());
}

#[test]
fn update_event_for_user_dest_reader_is_invalid() {
    let http = FakeHttp::new(vec![]);
    let mut dest = calendar("cal-2", "work@example.com", true);
    dest.access_role = "reader".to_string();
    let calendars = FakeCalendarRepo::with(vec![
        calendar("cal-1", "primary@example.com", true),
        dest,
    ]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "g-evt-1");
    let ops = FakeOperationRepo::new();

    let err = pollster::block_on(update_event_for_user(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "u-1",
        "local-1",
        &PatchEventFields {
            start: None,
            end: None,
            summary: None,
            description: None,
            is_all_day: None,
            start_time_zone: None,
            calendar_id: Some("cal-2".to_string()),
        },
        NOW_UNIX,
    ))
    .unwrap_err();
    assert!(
        matches!(err, CalendarError::Invalid(ref m) if m.contains("destination")),
        "got {err:?}"
    );
    assert!(http.posts.lock().unwrap().is_empty());
    assert!(ops.stored.lock().unwrap().is_empty());
}

#[test]
fn update_event_for_user_source_reader_is_invalid() {
    let http = FakeHttp::new(vec![]);
    let mut src = calendar("cal-1", "primary@example.com", true);
    src.access_role = "reader".to_string();
    let calendars = FakeCalendarRepo::with(vec![
        src,
        calendar("cal-2", "work@example.com", true),
    ]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "g-evt-1");
    let ops = FakeOperationRepo::new();

    let err = pollster::block_on(update_event_for_user(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "u-1",
        "local-1",
        &PatchEventFields {
            start: None,
            end: None,
            summary: None,
            description: None,
            is_all_day: None,
            start_time_zone: None,
            calendar_id: Some("cal-2".to_string()),
        },
        NOW_UNIX,
    ))
    .unwrap_err();
    assert!(
        matches!(err, CalendarError::Invalid(ref m) if m.contains("source")),
        "got {err:?}"
    );
    assert!(http.posts.lock().unwrap().is_empty());
    assert!(ops.stored.lock().unwrap().is_empty());
}

#[test]
fn update_event_for_user_empty_calendar_id_is_invalid() {
    let http = FakeHttp::new(vec![]);
    let calendars = two_calendars();
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "g-evt-1");
    let ops = FakeOperationRepo::new();

    let err = pollster::block_on(update_event_for_user(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "u-1",
        "local-1",
        &PatchEventFields {
            start: None,
            end: None,
            summary: None,
            description: None,
            is_all_day: None,
            start_time_zone: None,
            calendar_id: Some("  ".to_string()),
        },
        NOW_UNIX,
    ))
    .unwrap_err();
    assert!(
        matches!(err, CalendarError::Invalid(ref m) if m.contains("empty")),
        "got {err:?}"
    );
    assert!(http.posts.lock().unwrap().is_empty());
    assert!(ops.stored.lock().unwrap().is_empty());
}

// ──────────────────────────────────────────
// update_event_for_user — all-day + time zone
// ──────────────────────────────────────────

#[test]
fn update_event_for_user_all_day_emits_start_end_date() {
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events/g-evt-1",
        200,
        PATCHED_JSON,
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "g-evt-1");
    let ops = FakeOperationRepo::new();

    pollster::block_on(update_event_for_user(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "u-1",
        "local-1",
        &PatchEventFields {
            start: Some("2026-09-13".to_string()),
            end: Some("2026-09-14".to_string()),
            summary: None,
            description: None,
            is_all_day: Some(true),
            start_time_zone: None,
            calendar_id: None,
        },
        NOW_UNIX,
    ))
    .unwrap();

    let body: serde_json::Value =
        serde_json::from_str(&http.patches.lock().unwrap()[0].1).unwrap();
    assert_eq!(body["start"]["date"], "2026-09-13");
    assert_eq!(body["end"]["date"], "2026-09-14");
    assert!(body["start"].get("dateTime").is_none(), "{body}");
    assert!(body["end"].get("dateTime").is_none(), "{body}");
    assert!(body["start"].get("timeZone").is_none(), "{body}");
    assert!(body.get("is_all_day").is_none(), "{body}");
}

#[test]
fn update_event_for_user_all_day_accepts_rfc3339_prefix() {
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events/g-evt-1",
        200,
        PATCHED_JSON,
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "g-evt-1");
    let ops = FakeOperationRepo::new();

    pollster::block_on(update_event_for_user(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "u-1",
        "local-1",
        &PatchEventFields {
            start: Some("2026-09-13T00:00:00Z".to_string()),
            end: Some("2026-09-14T00:00:00Z".to_string()),
            summary: None,
            description: None,
            is_all_day: Some(true),
            start_time_zone: None,
            calendar_id: None,
        },
        NOW_UNIX,
    ))
    .unwrap();

    let body: serde_json::Value =
        serde_json::from_str(&http.patches.lock().unwrap()[0].1).unwrap();
    assert_eq!(body["start"]["date"], "2026-09-13");
    assert_eq!(body["end"]["date"], "2026-09-14");
}

#[test]
fn update_event_for_user_timed_with_timezone_on_start_and_end() {
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events/g-evt-1",
        200,
        PATCHED_JSON,
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "g-evt-1");
    let ops = FakeOperationRepo::new();

    pollster::block_on(update_event_for_user(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "u-1",
        "local-1",
        &PatchEventFields {
            start: Some("2026-08-19T09:00:00Z".to_string()),
            end: Some("2026-08-19T10:00:00Z".to_string()),
            summary: None,
            description: None,
            is_all_day: None,
            start_time_zone: Some("America/New_York".to_string()),
            calendar_id: None,
        },
        NOW_UNIX,
    ))
    .unwrap();

    let body: serde_json::Value =
        serde_json::from_str(&http.patches.lock().unwrap()[0].1).unwrap();
    assert_eq!(body["start"]["dateTime"], "2026-08-19T09:00:00Z");
    assert_eq!(body["end"]["dateTime"], "2026-08-19T10:00:00Z");
    assert_eq!(body["start"]["timeZone"], "America/New_York");
    assert_eq!(body["end"]["timeZone"], "America/New_York");
    assert!(body.get("start_time_zone").is_none(), "{body}");
}

#[test]
fn update_event_for_user_all_day_without_start_end_is_invalid() {
    let http = FakeHttp::new(vec![]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "g-evt-1");
    let ops = FakeOperationRepo::new();

    let err = pollster::block_on(update_event_for_user(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "u-1",
        "local-1",
        &PatchEventFields {
            start: None,
            end: None,
            summary: None,
            description: None,
            is_all_day: Some(true),
            start_time_zone: None,
            calendar_id: None,
        },
        NOW_UNIX,
    ))
    .unwrap_err();
    assert!(
        matches!(err, CalendarError::Invalid(ref m) if m.contains("is_all_day")),
        "got {err:?}"
    );
    assert!(http.patches.lock().unwrap().is_empty());
    assert!(ops.stored.lock().unwrap().is_empty());
}

#[test]
fn update_event_for_user_timezone_without_start_end_is_invalid() {
    let http = FakeHttp::new(vec![]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "g-evt-1");
    let ops = FakeOperationRepo::new();

    let err = pollster::block_on(update_event_for_user(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "u-1",
        "local-1",
        &PatchEventFields {
            start: None,
            end: None,
            summary: None,
            description: None,
            is_all_day: None,
            start_time_zone: Some("America/New_York".to_string()),
            calendar_id: None,
        },
        NOW_UNIX,
    ))
    .unwrap_err();
    assert!(
        matches!(err, CalendarError::Invalid(ref m) if m.contains("time zone")),
        "got {err:?}"
    );
    assert!(http.patches.lock().unwrap().is_empty());
    assert!(ops.stored.lock().unwrap().is_empty());
}

#[test]
fn update_event_for_user_calendar_id_with_is_all_day_is_invalid() {
    let http = FakeHttp::new(vec![]);
    let calendars = two_calendars();
    let events = FakeEventRepo::new();
    seed_living(&events, "local-1", "cal-1", "g-evt-1");
    let ops = FakeOperationRepo::new();

    let err = pollster::block_on(update_event_for_user(
        &http,
        &calendars,
        &events,
        &ops,
        &access(),
        "u-1",
        "local-1",
        &PatchEventFields {
            start: None,
            end: None,
            summary: None,
            description: None,
            is_all_day: Some(true),
            start_time_zone: None,
            calendar_id: Some("cal-2".to_string()),
        },
        NOW_UNIX,
    ))
    .unwrap_err();
    assert!(
        matches!(err, CalendarError::Invalid(ref m) if m.contains("cannot be combined")),
        "got {err:?}"
    );
    assert!(http.posts.lock().unwrap().is_empty());
    assert!(http.patches.lock().unwrap().is_empty());
    assert!(ops.stored.lock().unwrap().is_empty());
}

#[test]
fn create_echo_upsert_same_google_id_keeps_one_local_row() {
    // Journaled create → local id evt-1. A later replica echo with the same
    // google id must natural-key upsert onto that row (no second local row).
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events",
        200,
        CREATED_JSON,
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let output = pollster::block_on(create_event(
        &http, &calendars, &events, &ops, &access(), "u-1", &input(), NOW_UNIX,
    ))
    .unwrap();
    let google_id = output.event.google_event_id.clone();
    assert_eq!(output.event.id, "evt-1");
    assert_eq!(events.stored.lock().unwrap().len(), 1);

    let echo = GoogleEvent {
        id: google_id.clone(),
        etag: Some("e-echo".into()),
        updated: Some("2026-08-19T12:00:00Z".into()),
        status: Some("confirmed".into()),
        summary: Some("New meeting".into()),
        description: Some("About things".into()),
        recurrence: None,
        start: Some(GoogleEventTime {
            date_time: Some("2026-08-19T09:00:00Z".into()),
            date: None,
            time_zone: None,
        }),
        end: Some(GoogleEventTime {
            date_time: Some("2026-08-19T10:00:00Z".into()),
            date: None,
            time_zone: None,
        }),
        extended_properties: None,
        ical_uid: None,
        sequence: Some(0),
        recurring_event_id: None,
        original_start_time: None,
    };
    let row = map_google_event(&echo, "cal-1", "2023-11-14T22:13:20Z");
    let echoed_id = pollster::block_on(events.upsert(row, "2023-11-14T22:13:20Z")).unwrap();

    assert_eq!(echoed_id, "evt-1", "natural-key upsert returns same local id");
    let stored = events.stored.lock().unwrap();
    assert_eq!(stored.len(), 1, "still one row: {stored:?}");
    assert_eq!(stored[0].id, "evt-1");
    assert_eq!(stored[0].google_event_id, google_id);
    assert_eq!(stored[0].google_etag, "e-echo");
}
