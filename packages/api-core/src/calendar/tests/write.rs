use super::support::*;
use crate::calendar::write::build_shared_properties;
use crate::calendar::{
    create_event, delete_event, delete_event_for_user, patch_event, patch_event_fields,
    update_event_for_user, CalendarError,
};
use crate::models::{
    GoogleCalendar, PatchEventFields, OP_STATUS_CACHE_APPLIED, OP_STATUS_FAILED,
    OP_STATUS_GOOGLE_COMMITTED, OP_VERB_INSERT,
};

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
        &http, &calendars, &events, &ops, &access(), &input(), NOW_UNIX,
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
        &http, &calendars, &events, &ops, &access(), &input, NOW_UNIX,
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
            &http, &calendars, &events, &ops, &access(), &input, NOW_UNIX,
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
        &http, &calendars, &events, &ops, &access(), &input, NOW_UNIX,
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
        &http, &calendars, &events, &ops, &access(), &cobalt_input, NOW_UNIX,
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
        &http, &calendars, &events, &ops, &access(), &banana_input, NOW_UNIX,
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
        &http, &calendars, &events, &ops, &access(), &input(), NOW_UNIX,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::NotFound), "got {err:?}");
    assert!(http.posts.lock().unwrap().is_empty(), "no Google call");
    assert!(ops.stored.lock().unwrap().is_empty(), "no journal row");
}

#[test]
fn create_google_non_2xx_is_an_api_error() {
    let http = FakeHttp::new(vec![("/events", 400, r#"{"error":"invalid"}"#)]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    let ops = FakeOperationRepo::new();

    let err = pollster::block_on(create_event(
        &http, &calendars, &events, &ops, &access(), &input(), NOW_UNIX,
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
        &http, &calendars, &events, &ops, &access(), &input(), NOW_UNIX,
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
        &http, &calendars, &events, &ops, &access(), &input(), NOW_UNIX,
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
        &http, &calendars, &events, &ops, &access(), &input(), NOW_UNIX,
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
        &http, &calendars, &events, &ops, &access(), &input, NOW_UNIX,
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
        &http, &calendars, &events, &ops, &access(), &input(), NOW_UNIX,
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
        &http, &calendars, &events, &ops, &access(), &input, NOW_UNIX,
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
        &http, &calendars, &events, &ops, &access(), &input, NOW_UNIX,
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
        &http, &calendars, &events, &ops, &access(), &input, NOW_UNIX,
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

#[test]
fn patch_posts_end_and_upserts_the_echoed_event() {
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events/google-evt-created",
        200,
        PATCHED_JSON,
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();

    let output = pollster::block_on(patch_event(
        &http, &calendars, &events, &access(), "cal-1", "google-evt-created",
        "2026-08-19T11:00:00Z", NOW_UNIX,
    ))
    .unwrap();

    assert_eq!(output.event.google_event_id, "google-evt-created");
    assert_eq!(output.event.end_time, "2026-08-19T11:00:00Z");
    assert_eq!(output.event.calendar_id, "cal-1");

    // PATCH body: `end.dateTime` only.
    let patches = http.patches.lock().unwrap();
    assert_eq!(patches.len(), 1);
    let (url, body) = patches.first().unwrap().clone();
    assert!(url.contains("primary%40example.com/events/google-evt-created"), "{url}");
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["end"]["dateTime"], "2026-08-19T11:00:00Z");

    // The echoed (patched) event replaced the cached row.
    let (google_id, upserted) = events.upserted_single.lock().unwrap().clone().unwrap();
    assert_eq!(google_id, "google-evt-created");
    assert_eq!(upserted.end_time, "2026-08-19T11:00:00Z");
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

    let output = pollster::block_on(patch_event(
        &http, &calendars, &events, &access(), "cal-1", "google-evt-created",
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

    let err = pollster::block_on(patch_event(
        &http, &calendars, &events, &access(), "cal-1", "g-1",
        "2026-08-19T11:00:00Z", NOW_UNIX,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::NotFound), "got {err:?}");
    assert!(http.patches.lock().unwrap().is_empty(), "no Google call");
}

#[test]
fn patch_google_non_2xx_is_an_api_error() {
    let http = FakeHttp::new(vec![("/events/google-evt-created", 400, r#"{"error":"invalid"}"#)]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();

    let err = pollster::block_on(patch_event(
        &http, &calendars, &events, &access(), "cal-1", "google-evt-created",
        "2026-08-19T11:00:00Z", NOW_UNIX,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::GoogleApi(_)), "got {err:?}");
    assert!(events.upserted_single.lock().unwrap().is_none(), "no cache write on failure");
}

#[test]
fn patch_cache_failure_is_logged_not_fatal() {
    let http = FakeHttp::new(vec![(
        "/events/google-evt-created",
        200,
        PATCHED_JSON,
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    *events.fail_upsert.lock().unwrap() = true;

    let output = pollster::block_on(patch_event(
        &http, &calendars, &events, &access(), "cal-1", "google-evt-created",
        "2026-08-19T11:00:00Z", NOW_UNIX,
    ))
    .unwrap();

    assert_eq!(output.event.end_time, "2026-08-19T11:00:00Z");
    assert!(
        matches!(output.cache_error.as_deref(), Some(message) if message.contains("cache write failed")),
        "{:?}",
        output.cache_error
    );
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

    pollster::block_on(patch_event_fields(
        &http,
        &calendars,
        &events,
        &access(),
        "cal-1",
        "google-evt-created",
        &PatchEventFields {
            start: Some("2026-08-19T09:00:00Z".to_string()),
            end: None,
            summary: None,
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

    pollster::block_on(patch_event_fields(
        &http,
        &calendars,
        &events,
        &access(),
        "cal-1",
        "google-evt-created",
        &PatchEventFields {
            start: Some("2026-08-19T09:00:00Z".to_string()),
            end: Some("2026-08-19T11:00:00Z".to_string()),
            summary: Some("Renamed".to_string()),
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
}

#[test]
fn patch_fields_all_none_is_invalid() {
    let http = FakeHttp::new(vec![]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();

    let err = pollster::block_on(patch_event_fields(
        &http,
        &calendars,
        &events,
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
}

#[test]
fn delete_event_sends_cancelled_and_soft_deletes_cache() {
    let http = FakeHttp::new(vec![(
        "/calendars/primary%40example.com/events/g-evt-1",
        200,
        r#"{"id":"g-evt-1","status":"cancelled"}"#,
    )]);
    let calendars = FakeCalendarRepo::with(vec![calendar("cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    events
        .stored
        .lock()
        .unwrap()
        .push(living_event("local-1", "cal-1", "g-evt-1"));

    pollster::block_on(delete_event(
        &http,
        &calendars,
        &events,
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

    let deleted = events.deleted.lock().unwrap();
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].0, "local-1");
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

    pollster::block_on(delete_event(
        &http,
        &calendars,
        &events,
        &access(),
        "cal-1",
        "g-evt-1",
        "local-1",
        NOW_UNIX,
    ))
    .unwrap();

    assert_eq!(events.deleted.lock().unwrap().len(), 1);
}

#[test]
fn update_event_for_user_wrong_owner_is_not_found() {
    let http = FakeHttp::new(vec![]);
    let calendars =
        FakeCalendarRepo::with(vec![calendar_for_user("other-user", "cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    events
        .stored
        .lock()
        .unwrap()
        .push(living_event("local-1", "cal-1", "g-evt-1"));

    let err = pollster::block_on(update_event_for_user(
        &http,
        &calendars,
        &events,
        &access(),
        "u-1",
        "local-1",
        &PatchEventFields {
            start: None,
            end: None,
            summary: Some("Nope".to_string()),
        },
        NOW_UNIX,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::NotFound), "got {err:?}");
    assert!(http.patches.lock().unwrap().is_empty(), "no Google call");
}

#[test]
fn delete_event_for_user_wrong_owner_is_not_found() {
    let http = FakeHttp::new(vec![]);
    let calendars =
        FakeCalendarRepo::with(vec![calendar_for_user("other-user", "cal-1", "primary@example.com", true)]);
    let events = FakeEventRepo::new();
    events
        .stored
        .lock()
        .unwrap()
        .push(living_event("local-1", "cal-1", "g-evt-1"));

    let err = pollster::block_on(delete_event_for_user(
        &http,
        &calendars,
        &events,
        &access(),
        "u-1",
        "local-1",
        NOW_UNIX,
    ))
    .unwrap_err();
    assert!(matches!(err, CalendarError::NotFound), "got {err:?}");
    assert!(http.patches.lock().unwrap().is_empty(), "no Google call");
    assert!(events.deleted.lock().unwrap().is_empty());
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
    events
        .stored
        .lock()
        .unwrap()
        .push(living_event("local-1", "cal-1", "g-evt-1"));

    let output = pollster::block_on(update_event_for_user(
        &http,
        &calendars,
        &events,
        &access(),
        "u-1",
        "local-1",
        &PatchEventFields {
            start: None,
            end: None,
            summary: Some("Renamed".to_string()),
        },
        NOW_UNIX,
    ))
    .unwrap();

    assert_eq!(output.event.google_event_id, "google-evt-created");
    let body: serde_json::Value =
        serde_json::from_str(&http.patches.lock().unwrap()[0].1).unwrap();
    assert_eq!(body["summary"], "Renamed");
}
